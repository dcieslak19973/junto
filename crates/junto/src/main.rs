//! junto — the host/app entry point.
//!
//! NOTE: junto is **terminal-less for humans** (CLAUDE.md constraint #2): the
//! constraint is about the *working* surface, not setup plumbing. This binary
//! is the host *process* plus one-time setup commands; `serve` starts the
//! long-running singleton host (docs/adr/0015) that agents (and, later, human
//! surfaces) connect to. Binary/`main` code may use `anyhow` and may
//! `?`-propagate — unlike the library crates.

mod acp;
mod agent;
mod binding;
mod enroll;
mod forge;
mod grader;
mod host;
mod identity;
mod init;
mod invites;
mod keys;
mod launch;
mod live_bridge;
mod live_plane;
mod live_ws;
mod mcp;
mod members;
mod outcome;
mod pending_lineage;
mod render;
mod verify;
mod web;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use junto_kernel::{
    ChannelId, EntryId, EntryPayload, LedgerEntry, Member, MemberKind, PublicKey, Timestamp,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

#[derive(Parser)]
#[command(name = "junto", about = "junto host process", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the MCP write surface over streamable HTTP (docs/adr/0012) and
    /// the read surface (docs/adr/0013) over every registered home substrate
    /// (docs/adr/0015).
    ///
    /// Agents connect at http://127.0.0.1:<port>/mcp; humans read channels at
    /// http://127.0.0.1:<port>/.
    Serve {
        /// Serve only this repository instead of the machine registry
        /// (single-substrate dev mode).
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Port to bind on localhost. 1727: the year Franklin founded the Junto.
        #[arg(long, default_value_t = 1727)]
        port: u16,
    },
    /// Open a channel (docs/adr/0014/0016): mint its id and write the
    /// ChannelOpened genesis entry binding the name, directly into the home
    /// substrate (no running host required).
    Open {
        /// The channel's human-facing name (unique within the home substrate).
        name: String,
        /// The home substrate repo. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// The opener's display name.
        #[arg(long)]
        author_name: String,
        /// The opener's email (the stable identity key).
        #[arg(long)]
        author_email: String,
        /// Declare an existing channel id instead of minting one — the
        /// grandfathering path for records that predate docs/adr/0014.
        #[arg(long)]
        id: Option<String>,
    },
    /// Set a project repo up for junto (docs/adr/0015): register it as a home
    /// substrate, wire the agent harness (.mcp.json + the SessionStart recall
    /// hook), and write the committed channel binding (.junto.toml).
    Init {
        /// The project repo. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// The ambient channel name for the committed binding. Defaults to
        /// the repo's directory name.
        #[arg(long)]
        channel: Option<String>,
        /// Also open the ambient channel (genesis authored by the repo's git
        /// user).
        #[arg(long)]
        open: bool,
        /// Also grant this agent membership (docs/adr/0017) and write its
        /// code relay into the checkout. Requires the channel to be open
        /// (pass --open, or run on an already-opened channel) and the repo's
        /// git user to be its founder. Pass with --agent-email.
        #[arg(long, requires = "agent_email")]
        agent_name: Option<String>,
        /// The agent's email (the stable identity key). Pass with --agent-name.
        #[arg(long, requires = "agent_name")]
        agent_email: Option<String>,
    },
    /// Print the briefs of every channel this checkout is bound to
    /// (.junto.toml + .junto.local.toml) — the SessionStart recall hook
    /// (docs/adr/0013). Also auto-heals a fresh git worktree by seeding its
    /// agent member code from the primary checkout (docs/adr/0017).
    /// Best-effort: never fails session start.
    Brief {
        /// The checkout directory. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Grant channel membership (docs/adr/0017): append a founder-authored
    /// MemberAdded entry and mint the new member's machine-local code (or,
    /// with --enroll, attach the key an enrolled device already minted for
    /// itself). The granter is the home substrate's git user, who must be
    /// the channel's founding member. `--enroll` takes no `--channel`: the
    /// set of channels redeemed comes from the invite the enrolled device
    /// answered (device-key-enrollment plan, Task 4's `redeem_enrollment`)
    /// — one invite may cover several channels (Task 3), and every one it
    /// still covers is redeemed in one call, reported per channel.
    AddMember {
        /// The new member's email (the stable identity key). Required
        /// unless --enroll is passed, whose payload already carries the
        /// email the enrolling device echoed back — retyping it here would
        /// risk it silently diverging from what was actually enrolled.
        #[arg(required_unless_present = "enroll")]
        email: Option<String>,
        /// The new member's display name. Required unless --enroll is
        /// passed (same reasoning as `email`).
        #[arg(long, required_unless_present = "enroll")]
        name: Option<String>,
        /// "human" or "agent" — decides whether this machine may mint the
        /// member's key (docs/adr/0033), so it must be stated, never
        /// defaulted: a keyless remote human left to default to "agent"
        /// would skip `Host::add_member`'s refusal gate and mint their
        /// keypair here anyway. Required on EVERY invocation, including
        /// `--enroll`: the founder authors the `MemberAdded` and is the
        /// trust anchor for who is admitted (docs/adr/0017), while an
        /// enrolling device only proves possession of a keypair — it
        /// cannot say whether the identity behind it is a human or an
        /// agent, so that declaration still has to come from the founder,
        /// not be derived from the act of enrolling.
        #[arg(long, required = true)]
        kind: Option<String>,
        /// Channel name or id. Required unless `--enroll` is passed: with
        /// `--enroll`, the channel set comes from the invite the enrolled
        /// device answered, never a single flag — accepting one here would
        /// reintroduce exactly the name-vs-id divergence a name-addressed
        /// enrollment used to risk.
        #[arg(long, required_unless_present = "enroll", conflicts_with = "enroll")]
        channel: Option<String>,
        /// The granter's display name. Defaults to the home substrate's git
        /// user — override when the founder's recorded identity differs from
        /// git config (identity is claimed; the terminal is the trust anchor).
        #[arg(long)]
        author_name: Option<String>,
        /// The granter's email. Defaults to the home substrate's git user.
        #[arg(long)]
        author_email: Option<String>,
        /// Also write the new member's code into this checkout's gitignored
        /// .junto.local.toml (the code relay, docs/adr/0017) so the session
        /// brief carries it — no hand-copying.
        #[arg(long)]
        checkout: Option<PathBuf>,
        /// Complete an enrollment (device-key-enrollment plan, Task 4/8):
        /// the third leg of the three-step exchange. `<url>` is the
        /// `junto://enroll?code=…` URI from `junto enroll`; its embedded
        /// public key becomes the member's key — this machine never mints
        /// one for them (docs/adr/0033). Redeems EVERY channel the invite
        /// that minted it still covers, one outcome printed per channel;
        /// conflicts with `email`/`--name`/`--channel` — the first two
        /// come from the enroll payload the device echoed back (retyping
        /// them here risks silent divergence from what was actually
        /// enrolled), and the last is meaningless here: the channel set
        /// comes from the invite store, not a flag. `--kind` does NOT
        /// conflict here: the device proves it holds a keypair, never who
        /// holds it, so the founder still declares human-or-agent,
        /// exactly as on the keyless path.
        #[arg(long, conflicts_with_all = ["email", "name", "channel"])]
        enroll: Option<String>,
    },
    /// Mint a founder-issued enrollment invite (device-key-enrollment
    /// plan, Task 6): the first leg of the three-step exchange that lets a
    /// new device join without a private key ever leaving it. One invite
    /// may cover several channels (Task 3): `--channel` is repeatable, and
    /// founder authority is required on every one of them before anything
    /// is minted — an invite the caller cannot themselves complete for
    /// even one channel would send the recipient through the whole
    /// exchange to fail at `junto add-member`.
    Invite {
        /// The new member's email — the invite is a grant for exactly this
        /// identity; `junto enroll` reads it back off the invite, never
        /// re-typed by the enrolling device.
        #[arg(long)]
        member: String,
        /// Channel name or id to invite them into. Repeatable — one
        /// invite may name several channels, capped at
        /// `enroll::MAX_INVITE_CHANNELS`.
        #[arg(long = "channel", required = true, num_args = 1..)]
        channel: Vec<String>,
    },
    /// Mint this device's own keypair and emit its public half
    /// (device-key-enrollment plan, Task 7): the second leg of the
    /// exchange. The secret stays on this machine — only the public key
    /// travels in the printed `junto://enroll?code=…` URI.
    Enroll {
        /// The `junto://invite?code=…` URI from `junto invite`.
        #[arg(long)]
        invite: String,
        /// The display name to enroll under. Defaults to this machine's
        /// git user name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Diverge a child channel from a parent (docs/adr/0027): open the child in
    /// the parent's home substrate (you found it) and record the divergence
    /// edge in both ledgers — the side-quest birth.
    Diverge {
        /// The new child channel's name.
        child_name: String,
        /// The parent channel (name or id) to diverge from.
        #[arg(long)]
        from: String,
        /// The parent entry this child split from (optional).
        #[arg(long)]
        at: Option<String>,
    },
    /// Converge a source channel into a target (docs/adr/0027): record the
    /// convergence edge in both ledgers and close the source. Refuses while the
    /// source has open gates. The target must already exist.
    Converge {
        /// The source channel (name or id) — it closes on convergence.
        source: String,
        /// The target channel (name or id) it converges into.
        #[arg(long)]
        into: String,
        /// Why it converged.
        #[arg(long, default_value = "converged")]
        rationale: String,
    },
    /// List every key grant in a channel (device-key-enrollment plan, Task
    /// 9): per grant, the member, a 16-hex fingerprint (never the full
    /// public key on a shared terminal), the granting entry id (the handle
    /// `retire-device` consumes), and its retirement timestamp when set.
    Keys {
        #[command(subcommand)]
        action: KeysCommand,
    },
    /// Revoke a member's active devices in one act (device-key-enrollment
    /// plan, Task 9): a founder-authored Park for every currently active
    /// key grant they hold. The member stays in the party — this only
    /// stops their entries recorded after now from counting toward
    /// standings, gates, sessions and lineage (docs/adr/0035). Founder-only.
    RevokeMember {
        /// The member being revoked.
        #[arg(long)]
        member: String,
        /// Channel name or id.
        #[arg(long)]
        channel: String,
        /// Why.
        #[arg(long)]
        rationale: String,
    },
    /// Retire exactly one device's key grant (device-key-enrollment plan,
    /// Task 9): a founder-authored Park targeting the entry that granted
    /// it. Founder-only.
    RetireDevice {
        /// The granting entry id, from `junto keys list`.
        #[arg(long)]
        grant: String,
        /// Channel name or id.
        #[arg(long)]
        channel: String,
        /// Why.
        #[arg(long)]
        rationale: String,
    },
}

/// `junto keys` subcommands (device-key-enrollment plan, Task 9).
#[derive(Subcommand)]
enum KeysCommand {
    /// List every key grant in a channel: member, fingerprint, the
    /// granting entry id, and its retirement timestamp when set.
    List {
        /// Channel name or id.
        #[arg(long)]
        channel: String,
        /// Restrict the listing to this member's grants.
        #[arg(long)]
        member: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    match Cli::parse().command {
        Command::Serve { repo, port } => serve(repo, port).await,
        Command::Open {
            name,
            repo,
            author_name,
            author_email,
            id,
        } => open(name, repo, author_name, author_email, id).await,
        Command::Init {
            repo,
            channel,
            open,
            agent_name,
            agent_email,
        } => {
            let agent = match (agent_name, agent_email) {
                (Some(name), Some(email)) => Some(Member::agent(name, email)),
                _ => None,
            };
            init::run(&repo, channel, open, agent).await
        }
        Command::Brief { dir } => brief(dir).await,
        Command::AddMember {
            email,
            name,
            kind,
            channel,
            author_name,
            author_email,
            checkout,
            enroll,
        } => {
            add_member(
                channel,
                email,
                name,
                kind,
                author_name,
                author_email,
                checkout,
                enroll,
            )
            .await
        }
        Command::Invite { member, channel } => invite(channel, member).await,
        Command::Enroll { invite: url, name } => enroll(url, name).await,
        Command::Diverge {
            child_name,
            from,
            at,
        } => diverge(from, child_name, at).await,
        Command::Converge {
            source,
            into,
            rationale,
        } => converge(source, into, rationale).await,
        Command::Keys { action } => match action {
            KeysCommand::List { channel, member } => keys_list(channel, member).await,
        },
        Command::RevokeMember {
            member,
            channel,
            rationale,
        } => revoke_member(channel, member, rationale).await,
        Command::RetireDevice {
            grant,
            channel,
            rationale,
        } => retire_device(channel, grant, rationale).await,
    }
}

/// Grant channel membership from the terminal (`docs/adr/0017`). The granter
/// defaults to the home substrate's git user — the founder check happens in
/// the host. No code is demanded here: whoever can run commands on this
/// machine can edit the code store anyway; codes guard the network surfaces.
///
/// With `--enroll`, this redeems the enrolled device's payload across
/// EVERY channel its invite still covers (device-key-enrollment plan,
/// Task 4) via [`redeem_enrollment`] — one outcome line per channel,
/// never a single email/name/channel triple. `--kind` is still the one
/// thing the payload cannot supply (the device proves it holds a
/// keypair, not who holds it), so it is still taken from `--kind`,
/// exactly as on the keyless path below. `--author-name`/`--author-email`
/// apply on BOTH paths (review round 2): built once, up front, and
/// threaded into `redeem_enrollment` as an explicit override so a
/// founder whose git config does not name the granting identity can
/// still complete an enrollment — `None` falls back to this machine's
/// git identity in each channel's own home substrate, exactly
/// `Host::add_member`'s own keyless-path default.
// Every parameter is a distinct clap flag on `Command::AddMember`; a struct
// would just be `Command::AddMember`'s own fields duplicated one call site
// away — no clarity gained.
#[allow(clippy::too_many_arguments)]
async fn add_member(
    channel: Option<String>,
    email: Option<String>,
    name: Option<String>,
    kind: Option<String>,
    author_name: Option<String>,
    author_email: Option<String>,
    checkout: Option<PathBuf>,
    enroll_url: Option<String>,
) -> Result<()> {
    let host = host::Host::from_registry(host::junto_home()?);
    let kind = kind.expect("clap requires --kind on every add-member invocation");
    let member_kind = match kind.as_str() {
        "human" => MemberKind::Human,
        "agent" => MemberKind::Agent,
        other => bail!("--kind must be 'human' or 'agent', not '{other}'"),
    };
    let author_override = match (author_name, author_email) {
        (Some(name), Some(email)) => Some(Member::human(name, email)),
        (None, None) => None,
        _ => bail!("pass both --author-name and --author-email, or neither"),
    };

    if let Some(url) = enroll_url {
        let payload = enroll::decode_enroll(&url).map_err(|err| {
            if err.to_string().contains("expired") {
                err.context(
                    "enroll codes are short-lived (about 10 minutes); ask them to run \
                     `junto enroll` again against a fresh invite",
                )
            } else {
                err
            }
        })?;
        // Snapshot BEFORE the run: `redeem_enrollment` returns no `Minted`
        // (its return type is the fixed `(host, payload, kind, granted_by)
        // -> Vec<(String, RedeemOutcome)>` contract), so whether the code
        // is newly minted is read back from its own existence now vs.
        // after — the code itself is a per-identity-per-machine artifact
        // (`docs/adr/0017`), unaffected by which channel(s) granted.
        let had_code_before = member_code_for(&payload.email)?.is_some();
        let outcomes = redeem_enrollment(&host, &payload, member_kind, author_override).await?;
        let mut granted_any = false;
        for (channel, outcome) in &outcomes {
            println!("{}", redeem_line(channel, outcome));
            if *outcome == RedeemOutcome::Granted {
                granted_any = true;
            }
        }
        if !granted_any {
            bail!("no channel was granted membership — see the outcomes above for why");
        }
        if let Some(code) = member_code_for(&payload.email)? {
            if let Some(checkout) = checkout {
                let checkout = dunce::canonicalize(&checkout)
                    .with_context(|| format!("checkout {} not found", checkout.display()))?;
                binding::write_local_member_code(&checkout, &code)?;
                println!(
                    "wrote their code relay into {} ({} — gitignored; the session brief \
                     carries it)",
                    checkout.display(),
                    binding::LOCAL_BINDING
                );
            } else {
                println!("{}", member_code_line(&code, !had_code_before));
            }
        }
        return Ok(());
    }

    let channel = channel.expect("clap requires --channel unless --enroll is passed");
    let (substrate, _ledger, _id) = resolve_channel(&host, &channel).await?;
    let email = email.expect("clap requires email unless --enroll is passed");
    let name = name.expect("clap requires --name unless --enroll is passed");
    let member = match member_kind {
        MemberKind::Human => Member::human(&name, &email),
        MemberKind::Agent => Member::agent(&name, &email),
    };

    let granted_by = match author_override {
        Some(member) => member,
        None => host::git_user(&substrate)?,
    };
    let minted = host
        .add_member(&channel, &granted_by, member, None, None)
        .await?;
    println!("added {email} to channel '{channel}'");
    if let Some(checkout) = checkout {
        let checkout = dunce::canonicalize(&checkout)
            .with_context(|| format!("checkout {} not found", checkout.display()))?;
        binding::write_local_member_code(&checkout, &minted.code)?;
        println!(
            "wrote their code relay into {} ({} — gitignored; the session brief carries it)",
            checkout.display(),
            binding::LOCAL_BINDING
        );
    } else {
        println!("{}", member_code_line(&minted.code, minted.newly_minted));
    }
    Ok(())
}

/// The line `add_member` prints for a member's machine-local code once a
/// grant lands — `--checkout` bypasses this entirely (it writes the code
/// into the checkout instead and prints its own line) — split out so the
/// newly-minted-vs-already-had distinction (review round 2, restored
/// after the multi-channel `--enroll` path briefly lost it) is testable
/// without capturing stdout, which this crate carries no dependency for
/// (see `git_repo`'s own test-module doc).
fn member_code_line(code: &str, newly_minted: bool) -> String {
    if newly_minted {
        format!(
            "their member code is {code} — hand it to them once (for an agent: pass --checkout \
             <dir> to write it into that checkout's {} so the session brief carries it)",
            binding::LOCAL_BINDING
        )
    } else {
        "they already had a member code on this machine; it still applies".to_string()
    }
}

/// One channel's result from [`redeem_enrollment`] (device-key-enrollment
/// plan, Task 4): the shared vocabulary [`redeem_line`] renders for the
/// CLI, and the HTTP redemption endpoint (a later task) will render for
/// its own response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RedeemOutcome {
    /// The channel's `MemberAdded` was appended.
    Granted,
    /// The email was already on the channel's roster with an ACTIVE grant
    /// for the payload's own key — exactly `Host::add_member`'s own
    /// no-op condition (`host.rs`, `add_member`, the party/keyring check),
    /// detected before the call so it is reported, never hidden behind a
    /// `Granted` that did not actually append anything.
    AlreadyAMember,
    /// `invites::consume` reported `AlreadyUsed` for this channel: a
    /// duplicate covered-channel record for the same token whose earlier
    /// twin was already consumed.
    InviteAlreadyUsed,
    /// The caller is not this channel's founding member. Refused WITHOUT
    /// consuming — the invite record stays live for a retry.
    NotFounder,
    /// Anything else: a `consume` outcome besides `Ok`/`AlreadyUsed`
    /// (`Unknown`, `Expired`, `WrongMember`, `WrongChannel`), or
    /// `Host::add_member` itself erroring after a successful `consume`.
    Failed(String),
}

/// One printed line per channel [`redeem_enrollment`] attempted — every
/// variant reads distinctly, so a mixed run's output never leaves the
/// human guessing which lines are the good news.
fn redeem_line(channel: &str, outcome: &RedeemOutcome) -> String {
    match outcome {
        RedeemOutcome::Granted => format!("{channel}: granted"),
        RedeemOutcome::AlreadyAMember => {
            format!("{channel}: already a member with this key — nothing to grant")
        }
        RedeemOutcome::InviteAlreadyUsed => {
            format!("{channel}: invite already used for this channel")
        }
        RedeemOutcome::NotFounder => format!("{channel}: refused — not this channel's founder"),
        RedeemOutcome::Failed(reason) => format!("{channel}: failed — {reason}"),
    }
}

/// Sentinel marking [`redeem_enrollment`]'s one legitimate refusal — the
/// presented invite token covers no channel this machine can redeem
/// (device-key-enrollment plan, Task 4/9). A typed marker, not a string
/// comparison: `web.rs`'s `/members` endpoint downcasts for this
/// (`err.downcast_ref::<InviteExhausted>()`) to tell this ONE 409 apart
/// from every other `Err` (a genuine failure, e.g. an unreadable invite
/// store, mapped to 500) — a string match on this struct's `Display`
/// text would silently drift out of sync the moment a `.context(..)`
/// landed on the `?` sites above this `bail!`, or a second `bail!` reused
/// the same wording deeper in the engine. `/devices/preview` (`web.rs`,
/// Task 9) shares the identical wording by displaying this same type,
/// never a duplicated literal.
#[derive(Debug)]
pub(crate) struct InviteExhausted;

impl std::fmt::Display for InviteExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "this enroll code's invite token matches nothing this machine can redeem: it was \
             never issued here, or every channel it covered has already redeemed; ask the \
             founder to run `junto invite` again if you still need access",
        )
    }
}

impl std::error::Error for InviteExhausted {}

/// Redeem `payload`'s invite across every channel it still covers
/// (device-key-enrollment plan, Task 4): the engine `add_member`'s
/// `--enroll` path calls, and the HTTP redemption endpoint (a later
/// task) will call too — passing `granted_by: None` there, since that
/// surface has no CLI flags to build an override from. The channel set
/// comes from `invites::channels_for` — an empty set is an error naming
/// both possible causes, since that one read cannot distinguish them: a
/// token this machine never issued, and a token every one of whose
/// channels has already been redeemed, both read back as "nothing left
/// to cover".
///
/// `granted_by`: `Some(member)` uses that identity as the granter on
/// EVERY redeemed channel (review round 2 — `--author-name`/
/// `--author-email` on the CLI); `None` falls back to
/// `host::git_user(&substrate)` per channel, exactly `Host::add_member`'s
/// own keyless-path default.
///
/// Never `bail!`s mid-set: every channel in the covered set gets an
/// outcome, in `channels_for`'s order — a partial run's caller still
/// sees the whole picture, and can decide (as `add_member` does) whether
/// the run as a whole succeeded. See [`redeem_one_channel`] for the
/// per-channel ordering.
pub(crate) async fn redeem_enrollment(
    host: &host::Host,
    payload: &enroll::EnrollPayload,
    kind: MemberKind,
    granted_by: Option<Member>,
) -> Result<Vec<(String, RedeemOutcome)>> {
    let junto_home = host::junto_home()?;
    let channels = invites::channels_for(&junto_home, &payload.invite_token)?;
    if channels.is_empty() {
        bail!(InviteExhausted);
    }

    let member = match kind {
        MemberKind::Human => Member::human(&payload.display_name, &payload.email),
        MemberKind::Agent => Member::agent(&payload.display_name, &payload.email),
    };

    let mut outcomes = Vec::with_capacity(channels.len());
    for channel in channels {
        let outcome = redeem_one_channel(
            host,
            &junto_home,
            payload,
            &member,
            &channel,
            granted_by.as_ref(),
        )
        .await;
        outcomes.push((channel, outcome));
    }
    Ok(outcomes)
}

/// One channel of [`redeem_enrollment`]'s set, in the required order:
/// resolve to the canonical id, project, check founder authority, decide
/// `AlreadyAMember`, `consume` for that id, then `Host::add_member`.
///
/// `AlreadyAMember` is decided BEFORE `consume` (review round 2, finding
/// 4: burn only what appends) — like `NotFounder`, it never touches the
/// invite record, so a channel that needed no append stays retryable
/// (harmless: re-pasting the same code just reports `AlreadyAMember`
/// again). Every OTHER outcome consumes: `consume` immediately precedes
/// `Host::add_member`, so a downstream append failure still burns that
/// channel's record (fails closed, matching the single-channel path's
/// original ordering, `spent_token_context`'s doc comment) — only that
/// channel's token is spent; every other channel's stays untouched.
async fn redeem_one_channel(
    host: &host::Host,
    junto_home: &Path,
    payload: &enroll::EnrollPayload,
    member: &Member,
    channel: &str,
    granted_by_override: Option<&Member>,
) -> RedeemOutcome {
    let (substrate, ledger, id) = match resolve_channel(host, channel).await {
        Ok(resolved) => resolved,
        Err(err) => return RedeemOutcome::Failed(format!("{err:#}")),
    };
    let view = match ledger.lock().await.project(&id).await {
        Ok(view) => view,
        Err(err) => return RedeemOutcome::Failed(format!("{err:#}")),
    };
    let granted_by = match granted_by_override {
        Some(member) => member.clone(),
        None => match host::git_user(&substrate) {
            Ok(member) => member,
            Err(err) => return RedeemOutcome::Failed(format!("{err:#}")),
        },
    };
    let Some(founder) = view.party.first() else {
        return RedeemOutcome::Failed(format!(
            "channel '{channel}' has no genesis, so it has no founding member to grant \
             membership"
        ));
    };
    if founder.email != granted_by.email {
        return RedeemOutcome::NotFounder;
    }

    // Exactly `Host::add_member`'s own no-op condition (host.rs,
    // `add_member`'s party/keyring check): the email is already on the
    // roster AND that exact key already has an ACTIVE grant. Decided on
    // THIS view, before `consume` runs — a *retired* grant for the same
    // key does not count (re-admitting a revoked member must still
    // append), matching `Host::add_member`'s own rule exactly.
    let already_a_member = view.party.iter().any(|m| m.email == member.email)
        && view.keyring.get(&member.email).is_some_and(|grants| {
            grants
                .iter()
                .any(|grant| grant.key == payload.public_key && grant.retired_at.is_none())
        });
    if already_a_member {
        return RedeemOutcome::AlreadyAMember;
    }

    // Burn the invite record BEFORE recording the member: this fails
    // closed, exactly as the single-channel path always did.
    let consumed =
        match invites::consume(junto_home, &payload.invite_token, &payload.email, channel) {
            Ok(consumed) => consumed,
            Err(err) => return RedeemOutcome::Failed(format!("{err:#}")),
        };
    match consumed {
        invites::Consumed::Ok => {}
        invites::Consumed::AlreadyUsed => return RedeemOutcome::InviteAlreadyUsed,
        other => {
            let err = consumed_error(other, &payload.email, channel)
                .expect("every non-Ok Consumed besides AlreadyUsed maps to an error");
            return RedeemOutcome::Failed(format!("{err:#}"));
        }
    }

    // Finding 2, part 3 (final fix wave): re-enrolling a member who
    // currently has a revocation cutoff silently restores it — say so.
    // Printed only right before the append it warns about, never for a
    // channel that turned out not to need one (`AlreadyAMember`,
    // `NotFounder` above).
    if let Some(warning) = identity::revocation_cutoff_warning(&view, &member.email, channel) {
        println!("{warning}");
    }

    // `{err:#}` (anyhow's alternate Debug), not `{err}`/`.to_string()`
    // (review round 3, finding 1): `Display` renders only the outermost
    // context — here `spent_token_context`'s "already burned" wrapper —
    // and discards the actual cause (a substrate write failure, a
    // signing failure, an unexpected refusal). The single-channel path
    // this replaced returned the `Error` to `main`, which prints the
    // full `Caused by` chain via `{:?}`; folding it into one `String`
    // must not lose what that chain carried, especially for the HTTP
    // endpoint, where this string is the client's only diagnostic.
    let appended = spent_token_context(
        host.add_member(
            channel,
            &granted_by,
            member.clone(),
            Some(payload.public_key.clone()),
            Some(payload.transport_public_key.clone()),
        )
        .await,
        true,
    );
    match appended {
        Ok(_) => RedeemOutcome::Granted,
        Err(err) => RedeemOutcome::Failed(format!("{err:#}")),
    }
}

/// Note, on `result`'s error, that `--enroll`'s invite token is already
/// burned (device-key-enrollment plan, Task 8, finding 3). Once
/// `invites::consume` returns `Ok`, the token is single-use and gone; every
/// failure between there and the recorded `MemberAdded` — a bad
/// `--author-*` pairing, `git_user` failing, the founder check, the append
/// itself — must say so, or the operator retries the identical URL and
/// lands on the `AlreadyUsed` dead end with no idea why. A no-op when
/// `enrolled` is false (the keyless path never burns a token).
fn spent_token_context<T>(result: Result<T>, enrolled: bool) -> Result<T> {
    if enrolled {
        result.with_context(|| {
            "the invite token was already burned by this attempt (enroll codes are single-use) \
             — re-run `junto invite` for a fresh one before retrying `add-member --enroll`"
        })
    } else {
        result
    }
}

/// Turn an `invites::consume` outcome into an error the human can act on —
/// `add_member`'s `--enroll` path. `Consumed::Ok` has no error (the caller
/// proceeds); every other variant names what happened and what to run next,
/// since a bare enum name would send them straight back to
/// `crate::live_bridge`'s pre-enrollment failure mode: a signature that
/// "does not verify" with no clue why.
fn consumed_error(
    consumed: invites::Consumed,
    member_email: &str,
    channel: &str,
) -> Option<anyhow::Error> {
    match consumed {
        invites::Consumed::Ok => None,
        invites::Consumed::Unknown => Some(anyhow!(
            "no invite on this machine matches this enroll code's token; ask the founder to \
             run `junto invite --member {member_email} --channel {channel}`"
        )),
        invites::Consumed::AlreadyUsed => Some(anyhow!(
            "this invite has already been redeemed; ask the founder to run `junto invite \
             --member {member_email} --channel {channel}` again for a fresh one"
        )),
        invites::Consumed::Expired => Some(anyhow!(
            "this invite has expired (invites last about 10 minutes); ask the founder to run \
             `junto invite --member {member_email} --channel {channel}` again"
        )),
        invites::Consumed::WrongMember => Some(anyhow!(
            "this enroll code's member ({member_email}) does not match who the invite names; \
             ask the founder to check the invite's --member and reissue if needed"
        )),
        invites::Consumed::WrongChannel => Some(anyhow!(
            "this enroll code was issued for a different channel than '{channel}'; ask the \
             founder to run `junto invite --member {member_email} --channel {channel}`"
        )),
    }
}

/// This identity's machine-local member code, if one was minted here
/// (`docs/adr/0017`). The CLI looks it up so it can author through the host's
/// code-checked write surface — whoever can run commands on this machine can
/// read the store anyway.
fn member_code_for(email: &str) -> Result<Option<String>> {
    Ok(members::minted_members(&host::junto_home()?)?
        .into_iter()
        .find(|record| record.member.email == email)
        .map(|record| record.code))
}

/// Everything `junto invite` produces before it prints anything: the
/// payload to encode into the shareable URI, and the display labels —
/// the spellings the founder actually typed, first-seen order, deduped
/// by canonical id — for the line `invite_line` prints. Split out of
/// `invite` so tests can inspect the minted token and payload directly,
/// without capturing `invite`'s `println!` (this crate has, and may add,
/// no stdout-capture dependency).
struct MintedInvite {
    payload: enroll::InvitePayload,
    display_channels: Vec<String>,
}

/// One invite may cover several channels (Task 3): every one is resolved
/// to its canonical id and checked with `require_founder` BEFORE a token
/// is minted or anything is written — an invite the caller cannot
/// complete for even one channel is refused whole, nothing issued,
/// nothing minted. Two spellings that address the same channel (a name
/// and its id, or two aliases) collapse to a single record, kept under
/// the FIRST spelling the founder typed — `invite_line` must show what a
/// human actually typed, never the raw canonical id underneath. Refuses
/// more than `enroll::MAX_INVITE_CHANNELS` channels here too, rather than
/// minting a code `decode_invite` would only reject later.
///
/// Prunes `invites.toml` first (final fix wave, finding 1): every call
/// appends new records, so without this the file would grow without
/// bound (`invites::prune`'s own doc comment) — `invite` is the one
/// command guaranteed to run whenever a human is actively using this
/// mechanism, so it is the natural place to reclaim long-expired ones.
async fn mint_invite(channels: Vec<String>, member: String) -> Result<MintedInvite> {
    invites::prune(&host::junto_home()?)?;
    if channels.len() > enroll::MAX_INVITE_CHANNELS {
        bail!(
            "an invite may name at most {} channels",
            enroll::MAX_INVITE_CHANNELS
        );
    }

    let host = host::Host::from_registry(host::junto_home()?);
    // Resolve every channel and prove founder authority on every one
    // BEFORE minting a token or issuing any record (see doc comment
    // above) — an invite the caller cannot complete for even one channel
    // must leave nothing behind.
    let mut canonical_channels: Vec<String> = Vec::new();
    let mut display_channels: Vec<String> = Vec::new();
    for channel in &channels {
        let (substrate, ledger, id) = resolve_channel(&host, channel).await?;
        let view = ledger.lock().await.project(&id).await?;
        let caller = host::git_user(&substrate)?;
        identity::require_founder(&view, &caller, channel)?;
        // The invite's channel field carries RESOLVED ids, not whatever
        // the caller typed (a name or an id): `invites::consume` (Task 8)
        // compares exactly, so an invite minted with `--channel <name>`
        // must match an enrollment completed against `--channel <id>` for
        // the same channel — otherwise the two legs of one exchange land
        // on different strings and redemption fails with a misleading
        // `WrongChannel`. Dedupe here, on the canonical id, so two
        // spellings of one channel never produce two records — keeping
        // the FIRST spelling as the display label, never the id a human
        // never typed.
        let canonical = id.to_string();
        if !canonical_channels.contains(&canonical) {
            canonical_channels.push(canonical);
            display_channels.push(channel.clone());
        }
    }

    if member.chars().count() > enroll::MAX_FIELD_CHARS {
        bail!(
            "--member exceeds the {}-char limit",
            enroll::MAX_FIELD_CHARS
        );
    }

    let token = enroll::mint_invite_token();
    let expires_at = Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS;
    for canonical in &canonical_channels {
        invites::issue(&host::junto_home()?, &token, &member, canonical, expires_at)?;
    }
    Ok(MintedInvite {
        payload: enroll::InvitePayload {
            v: enroll::PAYLOAD_VERSION,
            invite_token: token,
            member_email: member,
            channels: canonical_channels,
            expires_at,
        },
        display_channels,
    })
}

/// `junto invite` — mint a founder-issued enrollment invite
/// (device-key-enrollment plan, Task 6); see [`mint_invite`] for the
/// rules. This wrapper only encodes the minted payload into a shareable
/// URI and prints it.
async fn invite(channels: Vec<String>, member: String) -> Result<()> {
    let minted = mint_invite(channels, member).await?;
    let expires_at = minted.payload.expires_at;
    let url = enroll::encode_invite(&minted.payload)?;
    println!(
        "{}",
        invite_line(&url, expires_at, &minted.display_channels)
    );
    Ok(())
}

/// Format `junto invite`'s output: the shareable URI, every channel it
/// covers, and a human-readable expiry, pinned so this shape is testable
/// without a CLI harness.
fn invite_line(url: &str, expires_at: i64, channels: &[String]) -> String {
    format!(
        "{url}\n(covers {})\n(expires {})",
        channels.join(", "),
        render::iso_utc(expires_at)
    )
}

/// Build the `junto enroll` response payload from a validated invite. The
/// invite token is echoed back verbatim (proves which grant this answers),
/// the email comes from the invite itself — never re-typed by the device,
/// which would defeat `invites::consume`'s `WrongMember` check — and
/// `public_key`/`transport_public_key` are the freshly minted (or reused)
/// keys' public halves (`docs/adr/0033` two-key separation).
fn enroll_payload_from_invite(
    invite: &enroll::InvitePayload,
    key: &PublicKey,
    transport_key: &PublicKey,
    name: &str,
) -> enroll::EnrollPayload {
    enroll::EnrollPayload {
        v: enroll::PAYLOAD_VERSION,
        invite_token: invite.invite_token.clone(),
        email: invite.member_email.clone(),
        display_name: name.to_string(),
        public_key: key.clone(),
        transport_public_key: transport_key.clone(),
        expires_at: invite.expires_at,
    }
}

/// `junto enroll` — mint this device's own keypair and emit its public
/// half (device-key-enrollment plan, Task 7). The invite is decoded and
/// validated *before* any key is minted, so an expired or malformed
/// invite never leaves a stray keypair on the device. The member email
/// comes from the invite, never a flag — letting the device re-type it
/// would defeat `invites::consume`'s `WrongMember` check.
async fn enroll(invite_url: String, name: Option<String>) -> Result<()> {
    let invite = enroll::decode_invite(&invite_url).map_err(|err| {
        if err.to_string().contains("expired") {
            err.context(
                "invites are short-lived (about 10 minutes); ask the founder to run \
                 `junto invite` again for a fresh one",
            )
        } else {
            err
        }
    })?;
    let display_name = match name {
        Some(name) => name,
        None => {
            host::git_user(Path::new("."))
                .context("no git identity to default --name from; pass --name explicitly")?
                .display_name
        }
    };
    let key = keys::signing_key(&host::junto_home()?, &invite.member_email)?;
    let transport_key = keys::transport_key(&host::junto_home()?, &invite.member_email)?;
    let payload = enroll_payload_from_invite(
        &invite,
        &key.public_key(),
        &transport_key.public_key(),
        &display_name,
    );
    let url = enroll::encode_enroll(&payload)?;
    println!("{url}");
    println!("this device's private key never leaves this machine — do not copy or share it");
    Ok(())
}

/// `junto diverge` — open a child channel off a parent and record the
/// divergence edge (`docs/adr/0027`). The diverger is this machine's git user
/// (taken from the parent's home substrate, like `junto open`).
async fn diverge(from: String, child_name: String, at: Option<String>) -> Result<()> {
    let host = host::Host::from_registry(host::junto_home()?);
    let substrate = match host.resolve(&from).await? {
        host::Resolution::Resolved { substrate, .. } => substrate,
        host::Resolution::NotFound => bail!("no channel '{from}' in any registered substrate"),
        host::Resolution::Ambiguous(substrates) => bail!(
            "channel name '{from}' exists in several substrates ({substrates:?}); address it by id"
        ),
    };
    let author = host::git_user(&substrate)?;
    let code = member_code_for(&author.email)?;
    let at = at.map(|raw| raw.parse::<EntryId>()).transpose()?;
    let child = host
        .diverge(
            &from,
            &child_name,
            at,
            author,
            host::WriteAuth::Agent(code.as_deref()),
        )
        .await?;
    println!(
        "diverged '{child_name}' from '{from}' (child id {})",
        child.id
    );
    Ok(())
}

/// `junto converge` — record a convergence edge and close the source
/// (`docs/adr/0027`). The converger is this machine's git user (from the
/// source's home substrate).
async fn converge(source: String, into: String, rationale: String) -> Result<()> {
    let host = host::Host::from_registry(host::junto_home()?);
    let substrate = match host.resolve(&source).await? {
        host::Resolution::Resolved { substrate, .. } => substrate,
        host::Resolution::NotFound => bail!("no channel '{source}' in any registered substrate"),
        host::Resolution::Ambiguous(substrates) => bail!(
            "channel name '{source}' exists in several substrates ({substrates:?}); \
             address it by id"
        ),
    };
    let author = host::git_user(&substrate)?;
    let code = member_code_for(&author.email)?;
    host.converge(
        &source,
        &into,
        &rationale,
        author,
        host::WriteAuth::Agent(code.as_deref()),
    )
    .await?;
    println!("converged '{source}' into '{into}' — the source is now closed");
    Ok(())
}

/// Resolve a channel reference to its home substrate, ledger and canonical
/// id — the shape `invite`/`add_member`/`converge` each already match,
/// shared here so the three Task 9 commands do not each repeat it a fourth
/// time.
async fn resolve_channel(
    host: &host::Host,
    channel: &str,
) -> Result<(PathBuf, host::SharedLedger, ChannelId)> {
    match host.resolve(channel).await? {
        host::Resolution::Resolved {
            substrate,
            ledger,
            id,
        } => Ok((substrate, ledger, id)),
        host::Resolution::NotFound => {
            bail!("no channel '{channel}' in any registered substrate")
        }
        host::Resolution::Ambiguous(substrates) => bail!(
            "channel name '{channel}' exists in several substrates ({substrates:?}); \
             address it by id"
        ),
    }
}

/// `junto keys list` — print every grant in a channel (device-key-
/// enrollment plan, Task 9); see [`keys_list_lines`] for the line shape.
async fn keys_list(channel: String, member: Option<String>) -> Result<()> {
    let host = host::Host::from_registry(host::junto_home()?);
    let (_substrate, ledger, id) = resolve_channel(&host, &channel).await?;
    let view = ledger.lock().await.project(&id).await?;

    let lines = identity::keys_list_lines(&view, member.as_deref());
    if lines.is_empty() {
        match member {
            Some(email) => println!("no key grants for {email} in channel '{channel}'"),
            None => println!("no key grants in channel '{channel}'"),
        }
    } else {
        for line in lines {
            println!("{line}");
        }
    }
    Ok(())
}

/// `junto revoke-member` — park every active key grant for `member` in one
/// act (device-key-enrollment plan, Task 9): the operator-facing,
/// account-level form of `retire-device`'s per-grant retirement (Task 2).
/// Refuses unless the caller is the channel's founder, refuses if
/// `member` has no active grant (nothing to do), and refuses if `member`
/// IS the founder (finding 3, final fix wave) — `require_founder` only
/// gates who may revoke, nothing gated who may *be* revoked, and parking
/// the founder's own grants would hand them a cutoff and unrecognize
/// every act they author afterward. Revocation never removes the member
/// from the party — recognition is party-set membership, and removal
/// would erase their whole history (`docs/adr/0035`, Task 3) — so the
/// printed warning names that consequence explicitly rather than letting
/// the operator assume otherwise.
async fn revoke_member(channel: String, member: String, rationale: String) -> Result<()> {
    let host = host::Host::from_registry(host::junto_home()?);
    let (substrate, ledger, id) = resolve_channel(&host, &channel).await?;
    let view = ledger.lock().await.project(&id).await?;
    let caller = host::git_user(&substrate)?;
    identity::require_founder(&view, &caller, &channel)?;
    // `require_founder` already proved `caller.email` IS the channel's
    // founder (only the founder passes that check) — so this alone tells
    // `member` apart from every other footgun this file already refuses
    // (an already-retired grant, an email with no active grants): the
    // caller can only ever be revoking either themselves or someone else.
    if member == caller.email {
        bail!(
            "'{member}' is the founder of '{channel}' — revoke-member would park every one \
             of the founder's own key grants and unrecognize everything they author from \
             that moment on. To rotate one of the founder's own machines, retire just that \
             device instead: `junto retire-device --grant <id> --channel {channel}` (get \
             <id> from `junto keys list --channel {channel}`); or, when moving to a new \
             device, enroll it FIRST with `junto add-member --enroll` and only retire the \
             old device's grant once the new one is in place"
        );
    }

    let targets = identity::grants_to_park(&view, &member);
    if targets.is_empty() {
        bail!(
            "{member} has no active key grants in channel '{channel}' — nothing to revoke \
             (already fully retired, or never held a key)"
        );
    }
    for target in &targets {
        let mut entry = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: id,
            author: caller.clone(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::Park {
                target: *target,
                rationale: rationale.clone(),
            },
        };
        host.sign_entry(&mut entry);
        ledger.lock().await.append(entry).await?;
    }
    println!(
        "parked {} grant(s) for {member} in channel '{channel}'",
        targets.len()
    );
    println!(
        "{member} stays in the party — this only stops their entries recorded after now from \
         counting toward standings, gates, sessions and lineage"
    );
    Ok(())
}

/// `junto retire-device` — park exactly one key grant, named by the entry
/// that granted it (`junto keys list`'s `granted_by`, device-key-
/// enrollment plan, Task 9). Refuses unless the caller is the channel's
/// founder. Task 2's projection treats a `Park` targeting anything but a
/// key-granting entry as a silent no-op — this looks the grant up in the
/// keyring FIRST and refuses if it is not there, or is already retired
/// (parking it again would be exactly that no-op), rather than reporting a
/// successful park that changed nothing.
async fn retire_device(channel: String, grant: String, rationale: String) -> Result<()> {
    let target: EntryId = grant
        .parse()
        .with_context(|| format!("--grant '{grant}' is not a valid entry id"))?;
    let host = host::Host::from_registry(host::junto_home()?);
    let (substrate, ledger, id) = resolve_channel(&host, &channel).await?;
    let view = ledger.lock().await.project(&id).await?;
    let caller = host::git_user(&substrate)?;
    identity::require_founder(&view, &caller, &channel)?;

    match view
        .keyring
        .values()
        .flatten()
        .find(|grant| grant.granted_by == target)
    {
        None => bail!(
            "'{grant}' does not name a key-granting entry in channel '{channel}' — check \
             `junto keys list --channel {channel}` for the granted_by id to pass here"
        ),
        Some(found) if found.retired_at.is_some() => {
            bail!("grant '{grant}' is already retired — parking it again would not change anything")
        }
        Some(_) => {}
    }

    let mut entry = LedgerEntry {
        signature: None,
        id: EntryId::new(),
        channel: id,
        author: caller,
        timestamp: Timestamp::now(),
        payload: EntryPayload::Park { target, rationale },
    };
    host.sign_entry(&mut entry);
    ledger.lock().await.append(entry).await?;
    println!("retired device grant '{grant}' in channel '{channel}'");
    Ok(())
}

/// Print the briefs of every channel this checkout is bound to. Best-effort by
/// design — a SessionStart hook must never break session start, so failures
/// are notes on stderr and the exit is always success.
async fn brief(dir: PathBuf) -> Result<()> {
    let channels = match binding::bound_channels(&dir) {
        Ok(channels) => channels,
        Err(err) => {
            eprintln!("junto brief: {err:#}");
            return Ok(());
        }
    };
    if channels.is_empty() {
        return Ok(());
    }
    let host = match host::junto_home() {
        Ok(junto_home) => host::Host::from_registry(junto_home),
        Err(err) => {
            eprintln!("junto brief: {err:#}");
            return Ok(());
        }
    };
    for channel in channels {
        match host.resolve(&channel).await {
            Ok(host::Resolution::Resolved { ledger, id, .. }) => {
                // Bind the projection instead of matching the lock guard's
                // temporary directly: a `MutexGuard` created in a `match`
                // scrutinee lives until the end of the *whole* match, not
                // just its arm, so it would still be held below while
                // `lineage_context` re-locks this same per-substrate ledger
                // for a sibling channel — a guaranteed self-deadlock.
                let projected = ledger.lock().await.project(&id).await;
                match projected {
                    Ok(view) => {
                        let name = view.name.clone().unwrap_or_else(|| channel.clone());
                        let lineage = host.lineage_context(&view).await.unwrap_or_default();
                        println!("{}", render::brief_markdown(&name, &id, &view, &lineage));
                    }
                    Err(err) => eprintln!("junto brief: projecting '{channel}': {err}"),
                }
            }
            Ok(host::Resolution::NotFound) => {
                eprintln!("junto brief: bound channel '{channel}' not found (not opened yet?)");
            }
            Ok(host::Resolution::Ambiguous(substrates)) => {
                eprintln!(
                    "junto brief: bound channel '{channel}' is ambiguous across {substrates:?}; \
                     bind by id"
                );
            }
            Err(err) => eprintln!("junto brief: resolving '{channel}': {err:#}"),
        }
    }
    // Auto-heal a fresh worktree: the local binding is gitignored, so a new
    // `git worktree add` starts without an agent code; copy it from the
    // primary worktree (docs/adr/0017). Best-effort — never fail session start.
    match init::seed_worktree(&dir) {
        Ok(binding::WorktreeSeed::Seeded) => {
            eprintln!("junto brief: seeded this worktree's member code from the primary checkout");
        }
        Ok(_) => {}
        Err(err) => eprintln!("junto brief: seeding worktree member code: {err:#}"),
    }
    // Relay this checkout's agent member code into session context, if the
    // operator put one in the local (gitignored) binding (docs/adr/0017).
    match binding::local_member_code(&dir) {
        Ok(Some(code)) => println!(
            "\nyour member code is {code} — pass it as `code` on junto write tools \
             (record/ratify/park/correct/propose/approve/reject)."
        ),
        Ok(None) => {}
        Err(err) => eprintln!("junto brief: reading member code: {err:#}"),
    }
    Ok(())
}

/// Run the host until Ctrl-C: the MCP write surface at /mcp, the channel index
/// at /, and the read-only channel pages at /channels/{name} (+ /brief).
async fn serve(repo: Option<PathBuf>, port: u16) -> Result<()> {
    let host = match repo {
        Some(repo) => host::Host::fixed(vec![dunce::canonicalize(&repo)?]),
        None => {
            let junto_home = host::junto_home()?;
            if host::registered_substrates(&junto_home)?.is_empty() {
                bail!(
                    "no home substrates registered under {} — run `junto init` in a repo, \
                     or serve one directly with --repo",
                    junto_home.display()
                );
            }
            host::Host::from_registry(junto_home)
        }
    };

    // Drain any pending lineage edges parked by a prior run (docs/adr/0028);
    // best-effort — a reconciliation failure must not stop the host serving.
    if let Err(err) = host.reconcile_lineage().await {
        tracing::warn!("lineage reconciliation at startup failed: {err:#}");
    }

    let handler = mcp::JuntoMcp::new(host.clone());
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );

    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .merge(web::router(host.clone()));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(
        "junto serving MCP at http://127.0.0.1:{port}/mcp and channels at \
         http://127.0.0.1:{port}/ over {:?}",
        host.substrate_paths()?
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Open a channel by writing its genesis straight into the substrate.
async fn open(
    name: String,
    repo: PathBuf,
    author_name: String,
    author_email: String,
    id: Option<String>,
) -> Result<()> {
    let repo = dunce::canonicalize(&repo)?;
    let declared_id = id.map(|raw| raw.parse::<ChannelId>()).transpose()?;
    // A fixed single-substrate host gives us the same open semantics (name
    // uniqueness under the append lock) the MCP tool uses.
    let host = host::Host::fixed(vec![repo.clone()]);
    let opened = host
        .open_channel(
            Some(&repo),
            &name,
            Member::human(&author_name, &author_email),
            declared_id,
        )
        .await?;
    println!(
        "opened channel '{name}' (id {}) in {}",
        opened.id,
        repo.display()
    );
    init::print_founder_code(&opened);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_line_contains_the_uri_and_a_human_readable_expiry() {
        let url = "junto://invite?code=abc123";
        // 1_700_000_000_000ms = 2023-11-14T22:13:20Z; asserted as a literal
        // so this test does not use `iso_utc` as its own oracle — mutating
        // `iso_utc` to its raw-millis fallback must fail this test too, not
        // just pin "invite_line calls iso_utc".
        let expires_at = 1_700_000_000_000;
        let line = invite_line(url, expires_at, &["acme".to_string()]);
        assert!(line.contains(url), "line should contain the URI: {line}");
        assert!(
            line.contains("2023-11-14 22:13 UTC"),
            "line should contain a human-readable expiry: {line}"
        );
    }

    /// The flag is repeatable and the parse keeps order and multiplicity.
    #[test]
    fn invite_accepts_repeated_channel_flags() {
        let cli = Cli::try_parse_from([
            "junto",
            "invite",
            "--member",
            "dan@x.com",
            "--channel",
            "one",
            "--channel",
            "two",
        ])
        .expect("parses");
        let Command::Invite { channel, .. } = cli.command else {
            panic!("expected invite");
        };
        assert_eq!(channel, vec!["one".to_string(), "two".to_string()]);
    }

    /// `--channel` with no value at all is still refused at parse time: an
    /// invite for zero channels grants nothing.
    #[test]
    fn invite_requires_at_least_one_channel() {
        assert!(Cli::try_parse_from(["junto", "invite", "--member", "dan@x.com"]).is_err());
    }

    /// The printed line names every channel, so the founder can see what
    /// they are about to hand over before they paste it.
    #[test]
    fn invite_line_names_every_channel_and_the_expiry() {
        let line = invite_line(
            "junto://invite?code=abc",
            1_781_000_000_000,
            &["alpha".to_string(), "beta".to_string()],
        );
        assert!(line.contains("junto://invite?code=abc"), "{line}");
        assert!(line.contains("alpha") && line.contains("beta"), "{line}");
        assert!(line.contains("2026"), "{line}");
    }

    #[test]
    fn enroll_payload_carries_the_invites_email_and_token_verbatim() {
        let invite = enroll::InvitePayload {
            v: enroll::PAYLOAD_VERSION,
            invite_token: "tok-abc-123".to_string(),
            member_email: "dan@example.com".to_string(),
            channels: vec!["junto-dev".to_string()],
            expires_at: 1_700_000_000_000,
        };
        let key = PublicKey::new(format!("ed25519:{}", "a".repeat(64))).unwrap();
        let transport_key = PublicKey::new(format!("ed25519:{}", "b".repeat(64))).unwrap();
        let payload = enroll_payload_from_invite(&invite, &key, &transport_key, "Dan's Laptop");
        assert_eq!(payload.v, enroll::PAYLOAD_VERSION);
        assert_eq!(payload.invite_token, invite.invite_token);
        assert_eq!(payload.email, invite.member_email);
        assert_eq!(payload.public_key, key);
        assert_eq!(payload.transport_public_key, transport_key);
        assert_eq!(payload.display_name, "Dan's Laptop");
        assert_eq!(payload.expires_at, invite.expires_at);
    }

    fn git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir.path())
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        // Repo-local identity, deterministic and matching `setup_channel`'s
        // founder — isolates `host::git_user` (Task 9's revocation
        // commands read it directly, with no `--author-*` override) from
        // whatever git config the machine running the tests happens to
        // have.
        git(&["config", "user.name", "Dan"]);
        git(&["config", "user.email", "dan@example.com"]);
        dir
    }

    /// Register a fresh substrate repo and open `name` in it (founder: Dan),
    /// so `add_member` (which reads the registry via `host::junto_home`)
    /// can resolve it. Returns the repo (kept alive for its TempDir) and the
    /// channel's canonical id.
    async fn setup_channel(name: &str) -> (tempfile::TempDir, ChannelId) {
        let repo = git_repo();
        let junto_home = host::junto_home().unwrap();
        host::register_substrate(&junto_home, repo.path()).unwrap();
        let fixed = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let opened = fixed
            .open_channel(
                Some(repo.path()),
                name,
                Member::human("Dan", "dan@example.com"),
                None,
            )
            .await
            .unwrap();
        (repo, opened.id)
    }

    /// Register a fresh substrate repo and open `name` in it, founded by
    /// `founder` rather than the repo's own git identity, by appending the
    /// `ChannelOpened` genesis directly onto a fresh ledger — mirrors
    /// `grant_key`'s direct-append approach below, since `git_repo()`'s
    /// identity is fixed for the whole suite and cannot be swapped
    /// mid-test. Lets a test put a channel's founder at odds with the
    /// git user `host::git_user` will read back for that repo (device-
    /// key-enrollment plan, Task 3's all-or-nothing `invite` rule).
    async fn setup_channel_with_founder(
        name: &str,
        founder: Member,
    ) -> (tempfile::TempDir, ChannelId) {
        let repo = git_repo();
        let junto_home = host::junto_home().unwrap();
        host::register_substrate(&junto_home, repo.path()).unwrap();
        let fixed = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let ledger = fixed.ledger_for(repo.path()).await.unwrap();
        let id = ChannelId::new();
        let genesis = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: id,
            author: founder,
            timestamp: Timestamp::now(),
            payload: EntryPayload::ChannelOpened {
                name: name.to_string(),
            },
        };
        ledger.lock().await.append(genesis).await.unwrap();
        (repo, id)
    }

    /// `docs/adr/0027`'s hazard, reproduced exactly as `brief` hits it: the
    /// bound channel has an **incoming lineage edge to a sibling channel in
    /// the same substrate**, so `Host::lineage_context` resolves the same
    /// per-substrate `Ledger` that `brief`'s own projection already locked.
    /// A regression here must never be able to hang the suite, so the whole
    /// call is bounded by `tokio::time::timeout` — a fixture this tiny has
    /// no business taking anywhere near that long, so elapsing it is
    /// unambiguously the deadlock, not slowness.
    #[tokio::test]
    async fn brief_does_not_deadlock_on_a_same_substrate_lineage_edge() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, _parent_id) = setup_channel("parent").await;
        let setup_host = host::Host::fixed(vec![repo.path().to_path_buf()]);
        setup_host
            .diverge(
                "parent",
                "child",
                None,
                Member::human("Dan", "dan@example.com"),
                host::WriteAuth::Human,
            )
            .await
            .unwrap();

        // Bind a fresh checkout to the child — the end this whole diagnosis
        // hinges on: `brief` walks bound channels one at a time, and the
        // child's `view.lineage` carries the Incoming edge back to the
        // parent, both served by the one ledger `Arc` this substrate shares.
        let checkout = tempfile::tempdir().unwrap();
        std::fs::write(
            checkout.path().join(binding::PROJECT_BINDING),
            "channels = [\"child\"]\n",
        )
        .unwrap();

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            brief(checkout.path().to_path_buf()),
        )
        .await;
        assert!(
            outcome.is_ok(),
            "brief() did not return within 5s — a lock taken while projecting the bound \
             channel is still held across lineage_context, which re-locks the same \
             per-substrate ledger for the sibling channel and deadlocks"
        );
        outcome.unwrap().unwrap();
    }

    fn build_enroll_url(
        token: &str,
        email: &str,
        display_name: &str,
        key: &PublicKey,
        expires_at: i64,
    ) -> String {
        enroll::encode_enroll(&enroll::EnrollPayload {
            v: enroll::PAYLOAD_VERSION,
            invite_token: token.to_string(),
            email: email.to_string(),
            display_name: display_name.to_string(),
            public_key: key.clone(),
            transport_public_key: PublicKey::new(format!("ed25519:{}", "9".repeat(64))).unwrap(),
            expires_at,
        })
        .unwrap()
    }

    #[test]
    fn consumed_error_ok_has_no_error() {
        assert!(consumed_error(invites::Consumed::Ok, "alice@example.com", "acme").is_none());
    }

    #[test]
    fn consumed_error_unknown_names_no_matching_invite() {
        let err = consumed_error(invites::Consumed::Unknown, "alice@example.com", "acme")
            .expect("Unknown must produce an error");
        let text = err.to_string();
        assert!(text.contains("no invite"), "{text}");
        assert!(
            text.contains("junto invite --member alice@example.com --channel acme"),
            "{text}"
        );
    }

    #[test]
    fn consumed_error_already_used_tells_them_to_reissue() {
        let err = consumed_error(invites::Consumed::AlreadyUsed, "alice@example.com", "acme")
            .expect("AlreadyUsed must produce an error");
        let text = err.to_string();
        assert!(text.contains("already"), "{text}");
        assert!(
            text.contains("junto invite --member alice@example.com --channel acme"),
            "{text}"
        );
    }

    #[test]
    fn consumed_error_expired_tells_them_invites_are_short_lived() {
        let err = consumed_error(invites::Consumed::Expired, "alice@example.com", "acme")
            .expect("Expired must produce an error");
        let text = err.to_string();
        assert!(text.contains("expired"), "{text}");
        assert!(text.contains("10 minutes"), "{text}");
    }

    #[test]
    fn consumed_error_wrong_member_names_the_mismatched_email() {
        let err = consumed_error(
            invites::Consumed::WrongMember,
            "mallory@evil.example",
            "acme",
        )
        .expect("WrongMember must produce an error");
        assert!(err.to_string().contains("mallory@evil.example"), "{err}");
    }

    #[test]
    fn consumed_error_wrong_channel_names_the_channel() {
        let err = consumed_error(invites::Consumed::WrongChannel, "alice@example.com", "acme")
            .expect("WrongChannel must produce an error");
        assert!(err.to_string().contains("acme"), "{err}");
    }

    /// Review finding 1: `--kind` decides whether this machine may mint the
    /// member's key (docs/adr/0033) — defaulting it to "agent" let a
    /// keyless human sneak past `Host::add_member`'s refusal gate and get
    /// minted locally anyway, precisely the bug this plan closes. The most
    /// likely invocation — no `--kind` at all — must be refused at parse
    /// time, before any host logic runs.
    #[test]
    fn add_member_kind_is_required_on_the_keyless_path() {
        let Err(err) = Cli::try_parse_from([
            "junto",
            "add-member",
            "alice@example.com",
            "--name",
            "Alice",
            "--channel",
            "acme",
        ]) else {
            panic!("expected a clap parse error");
        };
        assert!(err.to_string().contains("--kind"), "{err}");
    }

    /// The founder now declares `kind` on the `--enroll` path too (agent
    /// enrollment fix): the device proves it holds a keypair, never who
    /// holds it, so `--kind` and `--enroll` must parse together rather
    /// than clap refusing the combination outright. A mutation that
    /// reinstates `conflicts_with = "enroll"` on `kind` (or vice versa)
    /// turns this `Ok` back into the old parse error.
    #[test]
    fn add_member_kind_no_longer_conflicts_with_enroll() {
        Cli::try_parse_from([
            "junto",
            "add-member",
            "--enroll",
            "junto://enroll?code=x",
            "--kind",
            "agent",
        ])
        .expect("--kind must be accepted alongside --enroll");
    }

    /// The guard that keeps Task 8's Critical closed: `--kind` losing its
    /// old `conflicts_with = "enroll"` must not silently resurrect a
    /// default. `--enroll` with no `--kind` at all is still refused at
    /// parse time, on both paths.
    #[test]
    fn add_member_enroll_still_requires_kind() {
        let Err(err) =
            Cli::try_parse_from(["junto", "add-member", "--enroll", "junto://enroll?code=x"])
        else {
            panic!("expected a clap parse error");
        };
        assert!(err.to_string().contains("--kind"), "{err}");
    }

    /// `--enroll`'s email and display name come from the validated
    /// payload, never re-typed (unlike `--kind`, which the payload cannot
    /// supply) — retyping either risks silent divergence from what was
    /// actually enrolled, so both flags still conflict with `--enroll`.
    #[test]
    fn add_member_enroll_still_conflicts_with_email_and_name() {
        let Err(err) = Cli::try_parse_from([
            "junto",
            "add-member",
            "alice@example.com",
            "--enroll",
            "junto://enroll?code=x",
            "--kind",
            "human",
        ]) else {
            panic!("expected a clap parse error for --enroll + email");
        };
        assert!(err.to_string().to_lowercase().contains("email"), "{err}");

        let Err(err) = Cli::try_parse_from([
            "junto",
            "add-member",
            "--enroll",
            "junto://enroll?code=x",
            "--kind",
            "human",
            "--name",
            "Alice",
        ]) else {
            panic!("expected a clap parse error for --enroll + --name");
        };
        assert!(err.to_string().contains("--name"), "{err}");
    }

    /// `--channel` is now meaningless on the enroll path: the set comes
    /// from the invite store, and accepting a name here would reintroduce
    /// exactly the name-vs-id divergence
    /// `add_member_enroll_uses_the_stores_canonical_channel_id_verbatim`
    /// was written to prevent.
    #[test]
    fn add_member_enroll_refuses_a_channel_flag() {
        let Err(err) = Cli::try_parse_from([
            "junto",
            "add-member",
            "--enroll",
            "junto://enroll?code=x",
            "--kind",
            "human",
            "--channel",
            "junto-dev",
        ]) else {
            panic!("--channel conflicts with --enroll");
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    /// The keyless path still requires `--channel`: nothing about this
    /// task changes it. `email` is POSITIONAL (no `--email` flag exists
    /// on `Command::AddMember`) — review round 3, finding 3: the
    /// original body used `--email`, which clap rejects as an unknown
    /// flag before ever reaching the missing-`--channel` check, so the
    /// assertion passed for a reason unrelated to `--channel` at all and
    /// stayed green even if `required_unless_present = "enroll"` were
    /// removed from the `channel` field entirely.
    #[test]
    fn add_member_keyless_still_requires_channel() {
        let Err(err) = Cli::try_parse_from([
            "junto",
            "add-member",
            "a@b.c",
            "--name",
            "A",
            "--kind",
            "agent",
        ]) else {
            panic!("expected a clap parse error");
        };
        assert!(
            err.kind() == clap::error::ErrorKind::MissingRequiredArgument
                || err.to_string().contains("--channel"),
            "{err}"
        );
    }

    #[test]
    fn redeem_line_reads_differently_for_every_outcome() {
        let cases = [
            RedeemOutcome::Granted,
            RedeemOutcome::AlreadyAMember,
            RedeemOutcome::InviteAlreadyUsed,
            RedeemOutcome::NotFounder,
            RedeemOutcome::Failed("append failed".into()),
        ];
        let lines: Vec<String> = cases.iter().map(|o| redeem_line("chan", o)).collect();
        for line in &lines {
            assert!(line.contains("chan"), "{line}");
        }
        let unique: std::collections::HashSet<&String> = lines.iter().collect();
        assert_eq!(
            unique.len(),
            cases.len(),
            "each outcome must read differently: {lines:?}"
        );
    }

    /// The core guarantee: a mixed run grants what it can, reports the
    /// rest, and burns ONLY the channels that actually appended — so the
    /// same enroll code retries exactly the remainder. `mine-a`/`mine-b`
    /// are founded by Dan (the git identity every `setup_channel`/
    /// `setup_channel_with_founder` repo shares) and grant; `theirs` is
    /// founded by Carol, so Dan cannot complete it — issued directly via
    /// `invites::issue`, since `junto invite` itself refuses a set the
    /// caller does not wholly found (Task 3); `already-mine` is founded
    /// by Dan too, but alice already holds an ACTIVE grant for the
    /// payload's own key there BEFORE this run, so it must report
    /// `AlreadyAMember` and — review round 2, finding 4 — must NOT be
    /// consumed either: `AlreadyAMember` appends nothing, so it stays
    /// retryable exactly like `NotFounder`.
    #[tokio::test]
    async fn redeeming_a_mixed_set_burns_only_what_it_granted() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo_a, id_a) = setup_channel("mine-a").await;
        let (_repo_b, id_b) = setup_channel("mine-b").await;
        let (repo_already, id_already) = setup_channel("already-mine").await;
        let (_repo_c, id_c) =
            setup_channel_with_founder("theirs", Member::human("Carol", "carol@example.com")).await;

        let key = PublicKey::new(format!("ed25519:{}", "1".repeat(64))).unwrap();
        let transport_key = PublicKey::new(format!("ed25519:{}", "2".repeat(64))).unwrap();

        // Pre-seed "already-mine" with an ACTIVE grant for the payload's
        // own key, directly (bypassing `Host::add_member`'s no-op guard —
        // the same technique `grant_key` exists for).
        let fixed_already = host::Host::fixed(vec![repo_already.path().to_path_buf()]);
        let host::Resolution::Resolved {
            ledger: already_ledger,
            id: already_resolved_id,
            ..
        } = fixed_already
            .resolve(&id_already.to_string())
            .await
            .unwrap()
        else {
            panic!("channel resolves");
        };
        grant_key(
            &already_ledger,
            already_resolved_id,
            "alice@example.com",
            &key,
        )
        .await;

        let minted = mint_invite(
            vec![
                "mine-a".to_string(),
                "mine-b".to_string(),
                "already-mine".to_string(),
            ],
            "alice@example.com".to_string(),
        )
        .await
        .unwrap();
        let junto_home = host::junto_home().unwrap();
        invites::issue(
            &junto_home,
            &minted.payload.invite_token,
            "alice@example.com",
            &id_c.to_string(),
            minted.payload.expires_at,
        )
        .unwrap();

        let payload = enroll::EnrollPayload {
            v: enroll::PAYLOAD_VERSION,
            invite_token: minted.payload.invite_token.clone(),
            email: "alice@example.com".to_string(),
            display_name: "Alice".to_string(),
            public_key: key,
            transport_public_key: transport_key,
            expires_at: minted.payload.expires_at,
        };

        let host = host::Host::from_registry(junto_home.clone());
        let outcomes = redeem_enrollment(&host, &payload, MemberKind::Human, None)
            .await
            .unwrap();
        let outcome_map: std::collections::HashMap<&str, &RedeemOutcome> = outcomes
            .iter()
            .map(|(channel, outcome)| (channel.as_str(), outcome))
            .collect();
        assert_eq!(
            outcome_map.get(id_a.to_string().as_str()),
            Some(&&RedeemOutcome::Granted)
        );
        assert_eq!(
            outcome_map.get(id_b.to_string().as_str()),
            Some(&&RedeemOutcome::Granted)
        );
        assert_eq!(
            outcome_map.get(id_c.to_string().as_str()),
            Some(&&RedeemOutcome::NotFounder)
        );
        assert_eq!(
            outcome_map.get(id_already.to_string().as_str()),
            Some(&&RedeemOutcome::AlreadyAMember)
        );

        let mut remaining = invites::channels_for(&junto_home, &payload.invite_token).unwrap();
        remaining.sort();
        let mut expected = vec![id_c.to_string(), id_already.to_string()];
        expected.sort();
        assert_eq!(
            remaining, expected,
            "only channels that did not append (NotFounder, AlreadyAMember) should remain \
             retryable — the two that granted must be burned"
        );
    }

    /// Task 4 finding 4 (review round 3): `RedeemOutcome::Failed` had no
    /// coverage through the real engine — `redeem_one_channel` must
    /// report `Failed` and KEEP GOING on a genuine post-consume
    /// `Host::add_member` failure, and burn ONLY the channel that was
    /// actually attempted. `members.toml` pre-created as a DIRECTORY (not
    /// a file) makes `members::mint`'s final write fail — deterministic,
    /// cross-platform, and genuinely downstream of a successful ledger
    /// append (`Host::add_member`'s own tail call, after `guard.append`
    /// already succeeded) — not a contrived error. Kills: a mutation
    /// returning `Granted` on an append failure (assertion on
    /// `mine-fails`'s outcome), one that `?`-propagates out of the loop
    /// instead of continuing (assertion that `theirs`, issued and
    /// processed AFTER the failing channel, still gets its own outcome),
    /// and one that skips burning on a downstream failure or burns a
    /// channel that was never attempted (the `channels_for` assertion).
    #[tokio::test]
    async fn a_failing_channel_reports_failed_and_the_run_keeps_going() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo_fails, id_fails) = setup_channel("mine-fails").await;
        let (repo_theirs, id_theirs) =
            setup_channel_with_founder("theirs", Member::human("Carol", "carol@example.com")).await;
        let junto_home = host::junto_home().unwrap();

        let minted = mint_invite(
            vec!["mine-fails".to_string()],
            "alice@example.com".to_string(),
        )
        .await
        .unwrap();
        invites::issue(
            &junto_home,
            &minted.payload.invite_token,
            "alice@example.com",
            &id_theirs.to_string(),
            minted.payload.expires_at,
        )
        .unwrap();

        let key = PublicKey::new(format!("ed25519:{}", "5".repeat(64))).unwrap();
        let transport_key = PublicKey::new(format!("ed25519:{}", "6".repeat(64))).unwrap();
        let payload = enroll::EnrollPayload {
            v: enroll::PAYLOAD_VERSION,
            invite_token: minted.payload.invite_token.clone(),
            email: "alice@example.com".to_string(),
            display_name: "Alice".to_string(),
            public_key: key,
            transport_public_key: transport_key,
            expires_at: minted.payload.expires_at,
        };

        // An unwritable member-code store: `members.toml` is a DIRECTORY,
        // so `members::mint`'s final write — called only AFTER a
        // successful ledger append — fails.
        let broken_home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(broken_home.path().join("members.toml")).unwrap();
        let host = host::Host::fixed_with_member_home(
            vec![
                repo_fails.path().to_path_buf(),
                repo_theirs.path().to_path_buf(),
            ],
            Some(broken_home.path().to_path_buf()),
        );

        let outcomes = redeem_enrollment(&host, &payload, MemberKind::Human, None)
            .await
            .unwrap();
        let outcome_map: std::collections::HashMap<&str, &RedeemOutcome> = outcomes
            .iter()
            .map(|(channel, outcome)| (channel.as_str(), outcome))
            .collect();
        assert!(
            matches!(
                outcome_map.get(id_fails.to_string().as_str()),
                Some(&&RedeemOutcome::Failed(_))
            ),
            "{outcomes:?}"
        );
        assert_eq!(
            outcome_map.get(id_theirs.to_string().as_str()),
            Some(&&RedeemOutcome::NotFounder),
            "the run must keep going past the failing channel: {outcomes:?}"
        );

        let remaining = invites::channels_for(&junto_home, &payload.invite_token).unwrap();
        assert_eq!(
            remaining,
            vec![id_theirs.to_string()],
            "the failing channel's record must still be burned (fails closed, matching a \
             Granted channel's own ordering); the un-attempted NotFounder channel must not be"
        );
    }

    /// One channel, redeemed twice within the same covered set — the
    /// shape a duplicate `invites::issue` for the same (token, channel,
    /// member) triplet produces. The first attempt grants; since
    /// `AlreadyAMember` is decided BEFORE `consume` (review round 2,
    /// finding 4), the second attempt sees the just-granted member on
    /// its own fresh projection and reports `AlreadyAMember` without
    /// touching its own (still-unconsumed) invite record at all — never
    /// a silent second grant, and never a burned record for a channel
    /// that appended nothing.
    #[tokio::test]
    async fn redeeming_a_duplicate_covered_channel_reports_already_a_member_not_a_second_grant() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo, id) = setup_channel("acme").await;
        let junto_home = host::junto_home().unwrap();
        let token = enroll::mint_invite_token();
        let expires_at = Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS;
        invites::issue(
            &junto_home,
            &token,
            "alice@example.com",
            &id.to_string(),
            expires_at,
        )
        .unwrap();
        invites::issue(
            &junto_home,
            &token,
            "alice@example.com",
            &id.to_string(),
            expires_at,
        )
        .unwrap();

        let key = PublicKey::new(format!("ed25519:{}", "7".repeat(64))).unwrap();
        let transport_key = PublicKey::new(format!("ed25519:{}", "8".repeat(64))).unwrap();
        let payload = enroll::EnrollPayload {
            v: enroll::PAYLOAD_VERSION,
            invite_token: token.clone(),
            email: "alice@example.com".to_string(),
            display_name: "Alice".to_string(),
            public_key: key,
            transport_public_key: transport_key,
            expires_at,
        };
        let host = host::Host::from_registry(junto_home.clone());

        let outcomes = redeem_enrollment(&host, &payload, MemberKind::Human, None)
            .await
            .unwrap();
        assert_eq!(
            outcomes,
            vec![
                (id.to_string(), RedeemOutcome::Granted),
                (id.to_string(), RedeemOutcome::AlreadyAMember),
            ]
        );

        let host::Resolution::Resolved {
            ledger,
            id: resolved_id,
            ..
        } = host.resolve(&id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&resolved_id).await.unwrap();
        let grants = view.keyring.get("alice@example.com").unwrap();
        assert_eq!(
            grants.iter().filter(|g| g.retired_at.is_none()).count(),
            1,
            "exactly one active grant, not two: {grants:?}"
        );

        // The second, un-attempted record was never consumed — a
        // duplicate that resolves to `AlreadyAMember` burns nothing.
        assert_eq!(
            invites::channels_for(&junto_home, &token).unwrap(),
            vec![id.to_string()],
            "the un-consumed duplicate record stays retryable"
        );
    }

    /// `consume`'s record lookup finds the FIRST matching (channel,
    /// member) record regardless of its consumed state (invites.rs's
    /// `.find()`), while `channels_for` filters consumed records out
    /// before ever returning a channel string. A stale, already-consumed
    /// record sharing a (channel, member) pair with a fresh, still-
    /// covered one is exactly where those two reads can disagree:
    /// `channels_for` still names the channel once (because of the fresh
    /// record), but `consume` still lands on the stale one first — this
    /// pins that `redeem_enrollment` surfaces that as `InviteAlreadyUsed`,
    /// not a silent success, and is the one remaining way that outcome is
    /// reachable now that `AlreadyAMember` is decided before `consume`.
    #[tokio::test]
    async fn a_stale_consumed_duplicate_record_reports_invite_already_used() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo, id) = setup_channel("acme").await;
        let junto_home = host::junto_home().unwrap();
        let token = enroll::mint_invite_token();
        let expires_at = Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS;
        invites::issue(
            &junto_home,
            &token,
            "alice@example.com",
            &id.to_string(),
            expires_at,
        )
        .unwrap();
        invites::issue(
            &junto_home,
            &token,
            "alice@example.com",
            &id.to_string(),
            expires_at,
        )
        .unwrap();
        // The FIRST of the two records is consumed directly — as if an
        // earlier redemption this run's own `channels_for` read never
        // sees, because the SECOND record is still live.
        assert!(matches!(
            invites::consume(&junto_home, &token, "alice@example.com", &id.to_string()).unwrap(),
            invites::Consumed::Ok
        ));
        assert_eq!(
            invites::channels_for(&junto_home, &token).unwrap(),
            vec![id.to_string()],
            "the second record keeps the channel covered"
        );

        let key = PublicKey::new(format!("ed25519:{}", "3".repeat(64))).unwrap();
        let transport_key = PublicKey::new(format!("ed25519:{}", "4".repeat(64))).unwrap();
        let payload = enroll::EnrollPayload {
            v: enroll::PAYLOAD_VERSION,
            invite_token: token,
            email: "alice@example.com".to_string(),
            display_name: "Alice".to_string(),
            public_key: key,
            transport_public_key: transport_key,
            expires_at,
        };
        let host = host::Host::from_registry(junto_home);
        let outcomes = redeem_enrollment(&host, &payload, MemberKind::Human, None)
            .await
            .unwrap();
        assert_eq!(
            outcomes,
            vec![(id.to_string(), RedeemOutcome::InviteAlreadyUsed)]
        );
    }

    /// A token this machine never issued reads back from `channels_for`
    /// exactly like one that was issued and fully redeemed already — an
    /// empty covered set either way — so the error must name both
    /// possible causes rather than confidently guessing one.
    #[tokio::test]
    async fn an_enroll_code_with_no_remaining_channels_is_an_error_naming_both_causes() {
        let _home = crate::host::test_home::HomeGuard::new();
        let junto_home = host::junto_home().unwrap();
        let host = host::Host::from_registry(junto_home);
        let key = PublicKey::new(format!("ed25519:{}", "9".repeat(64))).unwrap();
        let transport_key = PublicKey::new(format!("ed25519:{}", "a".repeat(64))).unwrap();
        let payload = enroll::EnrollPayload {
            v: enroll::PAYLOAD_VERSION,
            invite_token: "z".repeat(43),
            email: "alice@example.com".to_string(),
            display_name: "Alice".to_string(),
            public_key: key,
            transport_public_key: transport_key,
            expires_at: Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS,
        };
        let err = redeem_enrollment(&host, &payload, MemberKind::Human, None)
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("never issued"), "{text}");
        assert!(text.contains("already redeemed"), "{text}");
    }

    /// Review round 2, concern 1: `--author-name`/`--author-email` must
    /// still work on `--enroll` — without an override, `redeem_enrollment`
    /// falls back to this machine's git identity per channel, which is
    /// Dan (`git_repo`'s fixed config) here, not this channel's real
    /// founder Carol, so the first attempt must fail outright (no channel
    /// granted). The SAME token still covers the channel afterward
    /// (`NotFounder` never consumes), so the second attempt, now WITH
    /// `--author-email carol@example.com`, redeems it.
    #[tokio::test]
    async fn add_member_enroll_honors_an_author_override_when_git_config_names_someone_else() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, id) =
            setup_channel_with_founder("acme", Member::human("Carol", "carol@example.com")).await;
        let junto_home = host::junto_home().unwrap();
        let token = enroll::mint_invite_token();
        let expires_at = Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS;
        invites::issue(
            &junto_home,
            &token,
            "alice@example.com",
            &id.to_string(),
            expires_at,
        )
        .unwrap();
        let key = PublicKey::new(format!("ed25519:{}", "c".repeat(64))).unwrap();
        let url = build_enroll_url(&token, "alice@example.com", "Alice", &key, expires_at);

        let without_override = add_member(
            None,
            None,
            None,
            Some("human".to_string()),
            None,
            None,
            None,
            Some(url.clone()),
        )
        .await;
        assert!(
            without_override.is_err(),
            "Dan (this machine's git identity) is not this channel's founder"
        );

        add_member(
            None,
            None,
            None,
            Some("human".to_string()),
            Some("Carol".to_string()),
            Some("carol@example.com".to_string()),
            None,
            Some(url),
        )
        .await
        .unwrap();

        let fixed = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            fixed.resolve(&id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert!(
            view.party.iter().any(|m| m.email == "alice@example.com"),
            "the override let Carol grant alice's enrollment"
        );
    }

    /// Review round 2, concern 5: `--enroll` briefly lost the newly-
    /// minted-vs-already-had distinction when the multi-channel engine's
    /// return type stopped carrying `members::Minted`. Pins that
    /// `member_code_line` (which both paths now share) still reads
    /// differently for the two cases.
    #[test]
    fn member_code_line_reads_differently_for_newly_minted_vs_already_had() {
        let newly = member_code_line("ABC123", true);
        let already = member_code_line("ABC123", false);
        assert_ne!(newly, already);
        assert!(newly.contains("ABC123"), "{newly}");
    }

    /// `--channel` no longer exists on the `--enroll` path (Task 4): the
    /// channel set comes straight from `invites::channels_for`, which
    /// already stores the CANONICAL id `junto invite` resolved at issue
    /// time (invites.rs's own `issue`/`consume` docs) — there is no
    /// human-typed name left to resolve here. This pins that the store's
    /// id is used VERBATIM: a token issued against the resolved id
    /// redeems successfully with no separate `--channel` to reconcile
    /// against it.
    #[tokio::test]
    async fn add_member_enroll_uses_the_stores_canonical_channel_id_verbatim() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo, id) = setup_channel("acme").await;
        let junto_home = host::junto_home().unwrap();

        let token = enroll::mint_invite_token();
        let expires_at = Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS;
        // Simulate `junto invite`'s own canonicalization: the invite is
        // recorded against the RESOLVED id, exactly like `invite()` does.
        invites::issue(
            &junto_home,
            &token,
            "alice@example.com",
            &id.to_string(),
            expires_at,
        )
        .unwrap();

        let key = PublicKey::new(format!("ed25519:{}", "a".repeat(64))).unwrap();
        let url = build_enroll_url(&token, "alice@example.com", "Alice", &key, expires_at);

        add_member(
            None,
            None,
            None,
            Some("human".to_string()),
            None,
            None,
            None,
            Some(url),
        )
        .await
        .unwrap();

        // The member landed with the supplied key — proof `consume`
        // actually returned `Ok`, not just that no error propagated.
        let fixed = host::Host::fixed(vec![_repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            fixed.resolve(&id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert_eq!(
            view.keyring
                .get("alice@example.com")
                .and_then(|grants| grants.first())
                .map(|grant| grant.key.clone()),
            Some(key)
        );
    }

    /// `AlreadyUsed` end to end: redeeming the same enroll URL twice must
    /// surface a message telling the human what happened and what to do.
    /// One channel, once fully redeemed, is gone from `channels_for`
    /// entirely (Task 4) — so the second attempt hits `redeem_enrollment`'s
    /// empty-channel-set refusal rather than a per-channel `AlreadyUsed`;
    /// either way, the message must still say "already" so the operator
    /// is not left guessing.
    #[tokio::test]
    async fn add_member_enroll_second_use_of_the_same_url_is_refused_as_already_used() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo, id) = setup_channel("acme").await;
        let junto_home = host::junto_home().unwrap();
        let token = enroll::mint_invite_token();
        let expires_at = Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS;
        invites::issue(
            &junto_home,
            &token,
            "alice@example.com",
            &id.to_string(),
            expires_at,
        )
        .unwrap();
        let key = PublicKey::new(format!("ed25519:{}", "b".repeat(64))).unwrap();
        let url = build_enroll_url(&token, "alice@example.com", "Alice", &key, expires_at);

        add_member(
            None,
            None,
            None,
            Some("human".to_string()),
            None,
            None,
            None,
            Some(url.clone()),
        )
        .await
        .unwrap();

        let err = add_member(
            None,
            None,
            None,
            Some("human".to_string()),
            None,
            None,
            None,
            Some(url),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("already"), "{err}");
    }

    /// Review finding 3: once `invites::consume` returns `Ok`, the token is
    /// gone; every failure between there and the recorded `MemberAdded`
    /// must say so, or the operator retries the identical URL and lands
    /// on the `AlreadyUsed` dead end with no idea why. Task 4's
    /// `redeem_enrollment` engine has no per-call author-override slot
    /// (the HTTP redemption endpoint that reuses it has no CLI flags to
    /// thread through), so a bad `--author-*` pairing can no longer be
    /// the trigger — this pins the same "already burned" wrapping
    /// directly around a `Host::add_member` failure (here, a non-founder
    /// granter) that happens after a real `consume`.
    #[tokio::test]
    async fn add_member_enroll_failure_after_consume_names_the_spent_token() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, id) = setup_channel("acme").await;
        let junto_home = host::junto_home().unwrap();
        let token = enroll::mint_invite_token();
        let expires_at = Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS;
        invites::issue(
            &junto_home,
            &token,
            "alice@example.com",
            &id.to_string(),
            expires_at,
        )
        .unwrap();
        assert!(matches!(
            invites::consume(&junto_home, &token, "alice@example.com", &id.to_string()).unwrap(),
            invites::Consumed::Ok
        ));

        // A real `Host::add_member` failure — Carol never founded "acme"
        // — occurring after the token above was genuinely burned.
        let host = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let result = host
            .add_member(
                &id.to_string(),
                &Member::human("Carol", "carol@example.com"),
                Member::human("Alice", "alice@example.com"),
                None,
                None,
            )
            .await;
        let err = spent_token_context(result, true).unwrap_err();
        assert!(err.to_string().contains("already burned"), "{err}");
    }

    /// Regression guard: the keyless/interactive path (no `--enroll`) must
    /// keep minting for the local-agent case.
    #[tokio::test]
    async fn add_member_keyless_path_still_mints_for_a_local_agent() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo, id) = setup_channel("acme").await;

        add_member(
            Some(id.to_string()),
            Some("worker@agents.junto".to_string()),
            Some("Worker".to_string()),
            Some("agent".to_string()),
            Some("Dan".to_string()),
            Some("dan@example.com".to_string()),
            None,
            None,
        )
        .await
        .unwrap();

        let junto_home = host::junto_home().unwrap();
        assert!(keys::has_signing_key(&junto_home, "worker@agents.junto").unwrap());
    }

    /// The defect this fix closes: before it, `--enroll` hardcoded
    /// `Member::human`, so an agent enrolled from another machine could
    /// only ever be recorded `Human` — durable, in an append-only record
    /// with no delete. This asserts BOTH the recorded `MemberKind` and
    /// that the key is the payload's (the device's own), not a locally
    /// minted one — a wrong implementation that ignored `--kind` and
    /// reused `Member::human`'s key handling could still pass a
    /// key-only or kind-only check.
    #[tokio::test]
    async fn add_member_enroll_kind_agent_records_agent_with_the_payloads_key() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo, id) = setup_channel("acme").await;
        let junto_home = host::junto_home().unwrap();
        let token = enroll::mint_invite_token();
        let expires_at = Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS;
        invites::issue(
            &junto_home,
            &token,
            "worker@agents.junto",
            &id.to_string(),
            expires_at,
        )
        .unwrap();
        let key = PublicKey::new(format!("ed25519:{}", "e".repeat(64))).unwrap();
        let url = build_enroll_url(&token, "worker@agents.junto", "Worker", &key, expires_at);

        add_member(
            None,
            None,
            None,
            Some("agent".to_string()),
            None,
            None,
            None,
            Some(url),
        )
        .await
        .unwrap();

        let fixed = host::Host::fixed(vec![_repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            fixed.resolve(&id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        let recorded = view
            .party
            .iter()
            .find(|m| m.email == "worker@agents.junto")
            .expect("enrolled agent is on the roster");
        assert_eq!(
            recorded.kind,
            junto_kernel::MemberKind::Agent,
            "{recorded:?}"
        );
        assert_eq!(
            view.keyring
                .get("worker@agents.junto")
                .and_then(|grants| grants.first())
                .map(|grant| grant.key.clone()),
            Some(key),
            "the recorded key must be the payload's own, not a locally minted one"
        );
        // The founder's machine never minted a key for the agent.
        assert!(!keys::has_signing_key(&junto_home, "worker@agents.junto").unwrap());
    }

    /// No regression: `--enroll --kind human` still records `Human` with
    /// the payload's key, exactly as before this fix.
    #[tokio::test]
    async fn add_member_enroll_kind_human_still_records_human_with_the_payloads_key() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo, id) = setup_channel("acme").await;
        let junto_home = host::junto_home().unwrap();
        let token = enroll::mint_invite_token();
        let expires_at = Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS;
        invites::issue(
            &junto_home,
            &token,
            "alice@example.com",
            &id.to_string(),
            expires_at,
        )
        .unwrap();
        let key = PublicKey::new(format!("ed25519:{}", "f".repeat(64))).unwrap();
        let url = build_enroll_url(&token, "alice@example.com", "Alice", &key, expires_at);

        add_member(
            None,
            None,
            None,
            Some("human".to_string()),
            None,
            None,
            None,
            Some(url),
        )
        .await
        .unwrap();

        let fixed = host::Host::fixed(vec![_repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            fixed.resolve(&id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        let recorded = view
            .party
            .iter()
            .find(|m| m.email == "alice@example.com")
            .expect("enrolled human is on the roster");
        assert_eq!(
            recorded.kind,
            junto_kernel::MemberKind::Human,
            "{recorded:?}"
        );
        assert_eq!(
            view.keyring
                .get("alice@example.com")
                .and_then(|grants| grants.first())
                .map(|grant| grant.key.clone()),
            Some(key)
        );
    }

    fn set_git_identity(repo: &Path, name: &str, email: &str) {
        for (key, value) in [("user.name", name), ("user.email", email)] {
            assert!(
                std::process::Command::new("git")
                    .args(["-C", &repo.display().to_string(), "config", key, value])
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    /// Append a founder-authored `MemberAdded` carrying `key` for `email`
    /// directly — bypasses `Host::add_member`'s "already on the roster is
    /// a no-op" guard, the only way this suite can put a *second* device
    /// grant on one member (mirrors the kernel's own multi-grant fixtures
    /// in `junto-kernel/src/ledger.rs`).
    async fn grant_key(
        ledger: &host::SharedLedger,
        channel: ChannelId,
        email: &str,
        key: &PublicKey,
    ) -> EntryId {
        let id = EntryId::new();
        let entry = LedgerEntry {
            signature: None,
            id,
            channel,
            author: Member::human("Dan", "dan@example.com"),
            timestamp: Timestamp::now(),
            payload: EntryPayload::MemberAdded {
                member: Member::human("Device", email).with_key(key.clone()),
            },
        };
        ledger.lock().await.append(entry).await.unwrap();
        id
    }

    #[tokio::test]
    async fn revoke_member_parks_every_active_grant_and_leaves_the_member_in_the_party() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, channel_id) = setup_channel("acme").await;
        let fixed = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            fixed.resolve(&channel_id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let key1 = PublicKey::new(format!("ed25519:{}", "1".repeat(64))).unwrap();
        let key2 = PublicKey::new(format!("ed25519:{}", "2".repeat(64))).unwrap();
        grant_key(&ledger, id, "alice@example.com", &key1).await;
        grant_key(&ledger, id, "alice@example.com", &key2).await;

        revoke_member(
            channel_id.to_string(),
            "alice@example.com".to_string(),
            "left the org".to_string(),
        )
        .await
        .unwrap();

        let view = ledger.lock().await.project(&id).await.unwrap();
        let grants = view.keyring.get("alice@example.com").unwrap();
        assert_eq!(grants.len(), 2);
        assert!(
            grants.iter().all(|g| g.retired_at.is_some()),
            "both grants should be parked: {grants:?}"
        );
        assert!(
            view.party.iter().any(|m| m.email == "alice@example.com"),
            "revocation must not remove the member from the party"
        );
    }

    #[tokio::test]
    async fn revoke_member_refuses_when_the_email_has_no_active_grants() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo, channel_id) = setup_channel("acme").await;
        let err = revoke_member(
            channel_id.to_string(),
            "nobody@example.com".to_string(),
            "why not".to_string(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no active key grants"), "{err}");
    }

    #[tokio::test]
    async fn revoke_member_refuses_a_non_founder_caller() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, channel_id) = setup_channel("acme").await;
        set_git_identity(repo.path(), "Eve", "eve@example.com");
        let err = revoke_member(
            channel_id.to_string(),
            "alice@example.com".to_string(),
            "not my call".to_string(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("founding member"), "{err}");
    }

    #[tokio::test]
    async fn retire_device_parks_exactly_the_named_grant() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, channel_id) = setup_channel("acme").await;
        let fixed = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            fixed.resolve(&channel_id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let key = PublicKey::new(format!("ed25519:{}", "3".repeat(64))).unwrap();
        let grant_id = grant_key(&ledger, id, "bob@example.com", &key).await;

        retire_device(
            channel_id.to_string(),
            grant_id.to_string(),
            "device lost".to_string(),
        )
        .await
        .unwrap();

        let view = ledger.lock().await.project(&id).await.unwrap();
        let grant = &view.keyring["bob@example.com"][0];
        assert!(grant.retired_at.is_some());
    }

    #[tokio::test]
    async fn retire_device_refuses_a_grant_id_that_does_not_exist() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo, channel_id) = setup_channel("acme").await;
        let bogus = EntryId::new();
        let err = retire_device(
            channel_id.to_string(),
            bogus.to_string(),
            "device lost".to_string(),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("does not name a key-granting entry"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn retire_device_refuses_a_grant_id_that_is_not_key_granting() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, channel_id) = setup_channel("acme").await;
        let fixed = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            fixed.resolve(&channel_id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let not_a_grant = EntryId::new();
        let entry = LedgerEntry {
            signature: None,
            id: not_a_grant,
            channel: id,
            author: Member::human("Dan", "dan@example.com"),
            timestamp: Timestamp::now(),
            payload: EntryPayload::Assertion {
                statement: "not a key grant".into(),
                rationale: "just a claim".into(),
                provenance: Vec::new(),
                frame: None,
            },
        };
        ledger.lock().await.append(entry).await.unwrap();

        let err = retire_device(
            channel_id.to_string(),
            not_a_grant.to_string(),
            "device lost".to_string(),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("does not name a key-granting entry"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn retire_device_refuses_a_grant_already_retired() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, channel_id) = setup_channel("acme").await;
        let fixed = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            fixed.resolve(&channel_id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let key = PublicKey::new(format!("ed25519:{}", "4".repeat(64))).unwrap();
        let grant_id = grant_key(&ledger, id, "carl@example.com", &key).await;
        let park_entry = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: id,
            author: Member::human("Dan", "dan@example.com"),
            timestamp: Timestamp::now(),
            payload: EntryPayload::Park {
                target: grant_id,
                rationale: "device lost the first time".into(),
            },
        };
        ledger.lock().await.append(park_entry).await.unwrap();

        let err = retire_device(
            channel_id.to_string(),
            grant_id.to_string(),
            "device lost again?".to_string(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("already retired"), "{err}");
    }

    #[tokio::test]
    async fn retire_device_refuses_a_non_founder_caller() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, channel_id) = setup_channel("acme").await;
        set_git_identity(repo.path(), "Eve", "eve@example.com");
        let err = retire_device(
            channel_id.to_string(),
            EntryId::new().to_string(),
            "not my call".to_string(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("founding member"), "{err}");
    }

    #[tokio::test]
    async fn keys_list_runs_clean_with_and_without_grants() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, channel_id) = setup_channel("acme").await;
        let fixed = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            fixed.resolve(&channel_id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        keys_list(channel_id.to_string(), None).await.unwrap();
        let key = PublicKey::new(format!("ed25519:{}", "5".repeat(64))).unwrap();
        grant_key(&ledger, id, "dana@example.com", &key).await;
        keys_list(channel_id.to_string(), None).await.unwrap();
        keys_list(channel_id.to_string(), Some("dana@example.com".to_string()))
            .await
            .unwrap();
    }

    /// Finding 1 (final fix wave): `prune` fell between two task briefs
    /// and shipped wired to nothing. This exercises the REAL `invite()`
    /// path end to end — not `invites::prune` directly, which is
    /// `invites.rs`'s own test — to pin the WIRING itself: removing the
    /// `invites::prune(...)?` call from `invite()`'s top would leave the
    /// stale record `Expired` (still on disk) rather than `Unknown`
    /// (actually removed), failing the first assertion below.
    #[tokio::test]
    async fn invite_prunes_a_long_expired_record_but_leaves_a_live_one() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo, channel_id) = setup_channel("acme").await;
        let junto_home = host::junto_home().unwrap();

        let now = Timestamp::now().as_millis();
        let stale = "s".repeat(43);
        let live = "l".repeat(43);
        // Pre-seeded directly (not through `invite()`, which always mints
        // a fresh, unexpired record) so the fixture controls exactly
        // which record is prunable: well past the 24h grace window, and
        // one that has not expired at all.
        invites::issue(
            &junto_home,
            &stale,
            "stale@example.com",
            "some-other-channel",
            now - 25 * 60 * 60 * 1000,
        )
        .unwrap();
        invites::issue(
            &junto_home,
            &live,
            "live@example.com",
            "some-other-channel",
            now + 60_000,
        )
        .unwrap();

        invite(
            vec![channel_id.to_string()],
            "someone@example.com".to_string(),
        )
        .await
        .unwrap();

        assert!(matches!(
            invites::consume(
                &junto_home,
                &stale,
                "stale@example.com",
                "some-other-channel"
            )
            .unwrap(),
            invites::Consumed::Unknown
        ));
        assert!(matches!(
            invites::consume(&junto_home, &live, "live@example.com", "some-other-channel").unwrap(),
            invites::Consumed::Ok
        ));
    }

    /// Founder authority is checked for EVERY channel before a token
    /// exists: a caller who founds one of two channels gets nothing
    /// issued at all — `invites.toml` gains no record for either channel.
    #[tokio::test]
    async fn invite_issues_nothing_when_the_caller_does_not_found_every_channel() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo_mine, _mine_id) = setup_channel("mine").await;
        let (_repo_theirs, _theirs_id) =
            setup_channel_with_founder("theirs", Member::human("Carol", "carol@example.com")).await;

        let err = invite(
            vec!["mine".to_string(), "theirs".to_string()],
            "someone@example.com".to_string(),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("theirs"),
            "error should name the channel the caller does not found: {err}"
        );

        let invites_path = host::junto_home().unwrap().join("invites.toml");
        assert!(
            !invites_path.exists(),
            "a caller who does not found every channel must get nothing issued"
        );
    }

    /// One token, one record per channel — the shape `channels_for` reads
    /// back. `alpha` is named twice under two different spellings — once
    /// by name, once by its canonical id — to pin the dedupe rule on the
    /// RESOLVED id: a dedupe that instead compared raw strings would let
    /// this pair through as two distinct channels.
    #[tokio::test]
    async fn invite_issues_one_record_per_channel_for_a_single_token() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo_a, id_a) = setup_channel("alpha").await;
        let (_repo_b, id_b) = setup_channel("beta").await;

        invite(
            vec!["alpha".to_string(), "beta".to_string(), id_a.to_string()],
            "someone@example.com".to_string(),
        )
        .await
        .unwrap();

        // `invite()` only prints its token (never returns it), so read the
        // store back directly rather than decoding the printed URL — the
        // stored `token_sha256` is exactly the fact "one token, N records"
        // rests on.
        let junto_home = host::junto_home().unwrap();
        let raw = std::fs::read_to_string(junto_home.join("invites.toml")).unwrap();
        let doc: toml::Value = toml::from_str(&raw).unwrap();
        let records = doc["invites"].as_array().unwrap();
        assert_eq!(
            records.len(),
            2,
            "'alpha' and its own id name the same channel and must collapse to one record: \
             {records:?}"
        );
        let tokens: std::collections::HashSet<&str> = records
            .iter()
            .map(|r| r["token_sha256"].as_str().unwrap())
            .collect();
        assert_eq!(
            tokens.len(),
            1,
            "one token must cover every channel: {records:?}"
        );
        let channels: std::collections::HashSet<String> = records
            .iter()
            .map(|r| r["channel"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            channels,
            [id_a.to_string(), id_b.to_string()].into_iter().collect(),
            "the store must name exactly the two canonical ids"
        );
    }

    /// The line a founder reads before pasting it must show what they
    /// actually typed, never the raw canonical id `resolve_channel`
    /// resolves underneath — and when the same channel is named twice
    /// under two spellings (its name, then its own id), only the FIRST
    /// spelling survives as the display label.
    #[tokio::test]
    async fn mint_invite_displays_the_first_spelling_not_the_canonical_id() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo, id) = setup_channel("acme").await;

        let minted = mint_invite(
            vec!["acme".to_string(), id.to_string()],
            "someone@example.com".to_string(),
        )
        .await
        .unwrap();

        assert_eq!(
            minted.payload.channels,
            vec![id.to_string()],
            "one canonical id, deduped"
        );
        assert_eq!(
            minted.display_channels,
            vec!["acme".to_string()],
            "the first spelling wins; the raw id must never be shown"
        );
    }

    /// Task 4 walks `InvitePayload.channels` to produce one outcome per
    /// channel; if that order ever diverged from what `channels_for`
    /// reads back for the same token, the founder would be shown one
    /// order in the minted code and granted in another once it is
    /// redeemed.
    #[tokio::test]
    async fn invite_payload_channel_order_matches_channels_for() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo_a, id_a) = setup_channel("alpha").await;
        let (_repo_b, id_b) = setup_channel("beta").await;
        let (_repo_c, id_c) = setup_channel("gamma").await;

        let minted = mint_invite(
            vec!["gamma".to_string(), "alpha".to_string(), "beta".to_string()],
            "someone@example.com".to_string(),
        )
        .await
        .unwrap();

        assert_eq!(
            minted.payload.channels,
            vec![id_c.to_string(), id_a.to_string(), id_b.to_string()],
            "first-seen order, not sorted"
        );

        let junto_home = host::junto_home().unwrap();
        let stored = invites::channels_for(&junto_home, &minted.payload.invite_token).unwrap();
        assert_eq!(
            stored, minted.payload.channels,
            "the store must read back channels in the same order the minted payload names them"
        );
    }

    /// Finding 2, part 3 (final fix wave): the cutoff-warning check added
    /// to `add_member` is advisory only, never a refusal — re-enrolling a
    /// member who currently has a revocation cutoff must still succeed
    /// and still grant an active key. Were the new check wired as a
    /// `bail!` instead of a `println!`, this `.unwrap()` would panic on
    /// an `Err` instead.
    #[tokio::test]
    async fn add_member_enroll_still_succeeds_when_re_enrolling_a_revoked_member() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, channel_id) = setup_channel("acme").await;
        let junto_home = host::junto_home().unwrap();
        let setup_host = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            setup_host.resolve(&channel_id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };

        let old_key = PublicKey::new(format!("ed25519:{}", "5".repeat(64))).unwrap();
        let grant_id = grant_key(&ledger, id, "alice@example.com", &old_key).await;
        let park = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: id,
            author: Member::human("Dan", "dan@example.com"),
            timestamp: Timestamp::now(),
            payload: EntryPayload::Park {
                target: grant_id,
                rationale: "lost device".into(),
            },
        };
        ledger.lock().await.append(park).await.unwrap();

        let expires_at = Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS;
        let token = enroll::mint_invite_token();
        invites::issue(
            &junto_home,
            &token,
            "alice@example.com",
            &channel_id.to_string(),
            expires_at,
        )
        .unwrap();
        let new_key = PublicKey::new(format!("ed25519:{}", "6".repeat(64))).unwrap();
        let url = build_enroll_url(&token, "alice@example.com", "Alice", &new_key, expires_at);

        add_member(
            None,
            None,
            None,
            Some("human".to_string()),
            None,
            None,
            None,
            Some(url),
        )
        .await
        .unwrap();

        let verify_host = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            verify_host.resolve(&channel_id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        let grants = view.keyring.get("alice@example.com").unwrap();
        assert_eq!(
            grants.len(),
            2,
            "re-enrollment appends a second grant: {grants:?}"
        );
        assert!(
            grants
                .iter()
                .any(|g| g.key == new_key && g.retired_at.is_none()),
            "re-enrollment's new key must be ACTIVE: {grants:?}"
        );
    }

    /// Task 4 finding 2 (review round 3): `already_a_member`'s
    /// `&& grant.retired_at.is_none()` clause was undiscriminated — the
    /// test above enrolls with a DIFFERENT key than the one it retires,
    /// so no keyring grant matches the payload's key at all regardless
    /// of that clause, and deleting it would still leave that test
    /// green. This pins the realistic production case directly:
    /// `keys::signing_key` mints a device's key only once, so a member
    /// re-enrolling from the SAME device after `revoke-member` presents
    /// the SAME public key against a RETIRED grant — that must still be
    /// `Granted`, never read back as `AlreadyAMember`.
    #[tokio::test]
    async fn redeeming_with_a_retired_grant_for_the_same_key_still_grants() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, channel_id) = setup_channel("acme").await;
        let junto_home = host::junto_home().unwrap();
        let setup_host = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            setup_host.resolve(&channel_id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };

        let key = PublicKey::new(format!("ed25519:{}", "b".repeat(64))).unwrap();
        let grant_id = grant_key(&ledger, id, "alice@example.com", &key).await;
        let park = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: id,
            author: Member::human("Dan", "dan@example.com"),
            timestamp: Timestamp::now(),
            payload: EntryPayload::Park {
                target: grant_id,
                rationale: "lost device".into(),
            },
        };
        ledger.lock().await.append(park).await.unwrap();

        let token = enroll::mint_invite_token();
        let expires_at = Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS;
        invites::issue(
            &junto_home,
            &token,
            "alice@example.com",
            &channel_id.to_string(),
            expires_at,
        )
        .unwrap();
        let transport_key = PublicKey::new(format!("ed25519:{}", "c".repeat(64))).unwrap();
        let payload = enroll::EnrollPayload {
            v: enroll::PAYLOAD_VERSION,
            invite_token: token,
            email: "alice@example.com".to_string(),
            display_name: "Alice".to_string(),
            public_key: key.clone(),
            transport_public_key: transport_key,
            expires_at,
        };

        let host = host::Host::from_registry(junto_home);
        let outcomes = redeem_enrollment(&host, &payload, MemberKind::Human, None)
            .await
            .unwrap();
        assert_eq!(
            outcomes,
            vec![(channel_id.to_string(), RedeemOutcome::Granted)],
            "a RETIRED grant for the same key must not read as AlreadyAMember"
        );

        let view = ledger.lock().await.project(&id).await.unwrap();
        let grants = view.keyring.get("alice@example.com").unwrap();
        assert_eq!(
            grants.len(),
            2,
            "re-enrollment with the same key appends a second grant: {grants:?}"
        );
        assert!(
            grants
                .iter()
                .any(|g| g.key == key && g.retired_at.is_none()),
            "the same key must now have an ACTIVE grant: {grants:?}"
        );
    }

    /// Finding 3 (final fix wave): `require_founder` only gates who may
    /// CALL `revoke-member`, not who may BE revoked — `grants_to_park`
    /// reads `view.keyring` by email with no exclusion. The founder is
    /// given a genuine ACTIVE grant first (their own second device), so a
    /// removed guard would let this call actually succeed and park it,
    /// not merely fail earlier on `grants_to_park`'s unrelated "nothing
    /// to revoke" check.
    #[tokio::test]
    async fn revoke_member_refuses_to_revoke_the_founder() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, channel_id) = setup_channel("acme").await;
        let fixed = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            fixed.resolve(&channel_id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let key = PublicKey::new(format!("ed25519:{}", "3".repeat(64))).unwrap();
        grant_key(&ledger, id, "dan@example.com", &key).await;

        let err = revoke_member(
            channel_id.to_string(),
            "dan@example.com".to_string(),
            "rotating laptop".to_string(),
        )
        .await
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("founder"), "{text}");
        assert!(text.contains("retire-device"), "{text}");
        assert!(text.contains("add-member --enroll"), "{text}");

        let view = ledger.lock().await.project(&id).await.unwrap();
        assert!(
            view.keyring["dan@example.com"]
                .iter()
                .all(|g| g.retired_at.is_none()),
            "a refused revoke-member must not park anything"
        );
    }

    /// Finding 3 (final fix wave): the founder guard belongs on
    /// `revoke-member` only — `retire-device` parks one grant at a time,
    /// and the founder may legitimately need it to rotate a single
    /// machine. Pins that `retire_device` is untouched by finding 3's fix
    /// — an over-broad guard copied onto this function too would make
    /// the `.unwrap()` below panic on an `Err`.
    #[tokio::test]
    async fn retire_device_still_works_on_a_founder_grant() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (repo, channel_id) = setup_channel("acme").await;
        let fixed = host::Host::fixed(vec![repo.path().to_path_buf()]);
        let host::Resolution::Resolved { ledger, id, .. } =
            fixed.resolve(&channel_id.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let key = PublicKey::new(format!("ed25519:{}", "4".repeat(64))).unwrap();
        let grant_id = grant_key(&ledger, id, "dan@example.com", &key).await;

        retire_device(
            channel_id.to_string(),
            grant_id.to_string(),
            "rotating this one device".to_string(),
        )
        .await
        .unwrap();

        let view = ledger.lock().await.project(&id).await.unwrap();
        let grant = view.keyring["dan@example.com"]
            .iter()
            .find(|g| g.granted_by == grant_id)
            .unwrap();
        assert!(
            grant.retired_at.is_some(),
            "retire-device must still be able to park a founder's own single grant"
        );
    }
}
