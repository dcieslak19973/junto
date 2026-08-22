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
    ChannelId, ChannelView, EntryId, EntryPayload, LedgerEntry, Member, PublicKey, Timestamp,
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
    /// the channel's founding member.
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
        /// Channel name or id.
        #[arg(long)]
        channel: String,
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
        /// Complete an enrollment (device-key-enrollment plan, Task 8): the
        /// third leg of the three-step exchange. `<url>` is the
        /// `junto://enroll?code=…` URI from `junto enroll`; its embedded
        /// public key becomes the member's key — this machine never mints
        /// one for them (docs/adr/0033). Requires the invite that produced
        /// it still be unredeemed and unexpired; conflicts with `email`/
        /// `--name` — those come from the enroll payload the device
        /// echoed back, and retyping them here risks silent divergence
        /// from what was actually enrolled. `--kind` does NOT conflict
        /// here: the device proves it holds a keypair, never who holds
        /// it, so the founder still declares human-or-agent, exactly as
        /// on the keyless path.
        #[arg(long, conflicts_with_all = ["email", "name"])]
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
/// With `--enroll`, this completes the three-step exchange
/// (device-key-enrollment plan, Task 8): the member's email, display name,
/// and public key come from the validated enroll payload, never re-typed,
/// and the invite token is burned via `invites::consume` before anything
/// is recorded — see the ordering comment at the call site below. `kind`
/// is the one thing the payload cannot supply (the device proves it holds
/// a keypair, not who holds it), so it is still taken from `--kind`,
/// exactly as on the keyless path below.
// Every parameter is a distinct clap flag on `Command::AddMember`; a struct
// would just be `Command::AddMember`'s own fields duplicated one call site
// away — no clarity gained.
#[allow(clippy::too_many_arguments)]
async fn add_member(
    channel: String,
    email: Option<String>,
    name: Option<String>,
    kind: Option<String>,
    author_name: Option<String>,
    author_email: Option<String>,
    checkout: Option<PathBuf>,
    enroll_url: Option<String>,
) -> Result<()> {
    let host = host::Host::from_registry(host::junto_home()?);
    // Resolve once, up front, to the CANONICAL id — not whatever the caller
    // typed. `--enroll`'s `invites::consume` compares the channel string
    // exactly against what `junto invite` stored (invites.rs:184), and
    // `--channel` here is a SEPARATE invocation from `junto invite`'s that
    // may be a human-typed name: passing it through raw would return a
    // confidently-wrong `WrongChannel` for the exact channel the invite was
    // issued for. Also yields `ledger`, so the revocation-cutoff check
    // below can project the pre-enrollment view without a second
    // resolve.
    let (substrate, ledger, id) = resolve_channel(&host, &channel).await?;

    // Whether this member's key came from `--enroll` — once true, the
    // invite token below is already burned, so every failure from here on
    // must say so (see `spent_token_context`).
    let mut enrolled = false;
    let (member, key, transport_key) = match enroll_url {
        Some(url) => {
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
            // Burn the invite token BEFORE recording the member: this fails
            // closed. A transient append failure after a successful consume
            // costs the founder one `junto invite` re-run; appending first
            // and burning the token after would instead leave a live,
            // replayable token behind an already-recorded member.
            let consumed = invites::consume(
                &host::junto_home()?,
                &payload.invite_token,
                &payload.email,
                &id.to_string(),
            )?;
            if let Some(err) = consumed_error(consumed, &payload.email, &channel) {
                return Err(err);
            }
            enrolled = true;
            // The device proved it holds a keypair; it did not prove who
            // holds it. `--kind` is required here too (see the field doc
            // on `Command::AddMember::kind`), so the founder — the trust
            // anchor for who is admitted (docs/adr/0017) — still declares
            // whether this identity is a human or an agent. Only email,
            // display name, and key come from the payload, exactly as
            // before: those the enrolling device genuinely knows better
            // than a retyped flag could.
            let kind = kind.expect("clap requires --kind on every add-member invocation");
            let member = match kind.as_str() {
                "human" => Member::human(&payload.display_name, &payload.email),
                "agent" => Member::agent(&payload.display_name, &payload.email),
                other => bail!("--kind must be 'human' or 'agent', not '{other}'"),
            };
            (
                member,
                Some(payload.public_key.clone()),
                Some(payload.transport_public_key.clone()),
            )
        }
        None => {
            let email = email.expect("clap requires email unless --enroll is passed");
            let name = name.expect("clap requires --name unless --enroll is passed");
            let kind = kind.expect("clap requires --kind on every add-member invocation");
            let member = match kind.as_str() {
                "human" => Member::human(&name, &email),
                "agent" => Member::agent(&name, &email),
                other => bail!("--kind must be 'human' or 'agent', not '{other}'"),
            };
            (member, None, None)
        }
    };
    let email = member.email.clone();
    // Finding 2, part 3 (final fix wave): re-enrolling a member who
    // currently has a revocation cutoff silently restores it — say so.
    // Advisory only, gated to `--enroll` (the real re-enrollment path);
    // never refuses, since re-admission is legitimate (docs/adr/0035).
    if enrolled {
        let view = spent_token_context(
            ledger
                .lock()
                .await
                .project(&id)
                .await
                .map_err(anyhow::Error::from),
            enrolled,
        )?;
        if let Some(warning) = revocation_cutoff_warning(&view, &email, &channel) {
            println!("{warning}");
        }
    }

    let granted_by = match (author_name, author_email) {
        (Some(name), Some(email)) => Ok(Member::human(name, email)),
        (None, None) => host::git_user(&substrate),
        _ => Err(anyhow!(
            "pass both --author-name and --author-email, or neither"
        )),
    };
    let granted_by = spent_token_context(granted_by, enrolled)?;
    let minted = spent_token_context(
        host.add_member(&channel, &granted_by, member, key, transport_key)
            .await,
        enrolled,
    )?;
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
    } else if minted.newly_minted {
        println!(
            "their member code is {} — hand it to them once (for an agent: pass --checkout \
             <dir> to write it into that checkout's {} so the session brief carries it)",
            minted.code,
            binding::LOCAL_BINDING
        );
    } else {
        println!("they already had a member code on this machine; it still applies");
    }
    Ok(())
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

/// `junto invite` — mint a founder-issued enrollment invite
/// (device-key-enrollment plan, Task 6). One invite may cover several
/// channels (Task 3): every one is resolved to its canonical id and
/// checked with `require_founder` BEFORE a token is minted or anything is
/// written — an invite the caller cannot complete for even one channel is
/// refused whole, nothing issued, nothing printed. Two names (or a name
/// and its id) that address the same channel collapse to a single record,
/// in first-seen order, so redemption never reports the same channel
/// twice. Refuses more than `enroll::MAX_INVITE_CHANNELS` channels here
/// too, rather than minting a code `decode_invite` would only reject
/// later.
///
/// Prunes `invites.toml` first (final fix wave, finding 1): every call
/// appends new records, so without this the file would grow without
/// bound (`invites::prune`'s own doc comment) — `invite` is the one
/// command guaranteed to run whenever a human is actively using this
/// mechanism, so it is the natural place to reclaim long-expired ones.
async fn invite(channels: Vec<String>, member: String) -> Result<()> {
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
    for channel in &channels {
        let (substrate, ledger, id) = resolve_channel(&host, channel).await?;
        let view = ledger.lock().await.project(&id).await?;
        let caller = host::git_user(&substrate)?;
        require_founder(&view, &caller, channel)?;
        // The invite's channel field carries RESOLVED ids, not whatever
        // the caller typed (a name or an id): `invites::consume` (Task 8)
        // compares exactly, so an invite minted with `--channel <name>`
        // must match an enrollment completed against `--channel <id>` for
        // the same channel — otherwise the two legs of one exchange land
        // on different strings and redemption fails with a misleading
        // `WrongChannel`. Dedupe here, on the canonical id, so two
        // spellings of one channel never produce two records.
        let canonical = id.to_string();
        if !canonical_channels.contains(&canonical) {
            canonical_channels.push(canonical);
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
    let url = enroll::encode_invite(&enroll::InvitePayload {
        v: enroll::PAYLOAD_VERSION,
        invite_token: token,
        member_email: member,
        channels: canonical_channels.clone(),
        expires_at,
    })?;
    println!("{}", invite_line(&url, expires_at, &canonical_channels));
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

/// Refuse unless `caller` is `view`'s founding member (device-key-
/// enrollment plan, Task 9) — revocation, like granting membership
/// (`invite`, `add-member`), is a founder-only act.
fn require_founder(view: &ChannelView, caller: &Member, channel: &str) -> Result<()> {
    let Some(founder) = view.party.first() else {
        bail!(
            "channel '{channel}' has no genesis, so it has no founding member to authorize \
             revocation (membership is not enforced on pre-genesis channels)"
        );
    };
    if founder.email != caller.email {
        bail!(
            "only the founding member ({} <{}>) can revoke keys in '{channel}' \
             (docs/adr/0033)",
            founder.display_name,
            founder.email
        );
    }
    Ok(())
}

/// Every currently active grant for `email` — the set `revoke-member` parks
/// in one act (device-key-enrollment plan, Task 9). Already-retired grants
/// are skipped: parking one again would append a `Park` that
/// [`junto_kernel::KeyGrant`]'s earliest-wins fold (Task 2) makes a no-op,
/// reporting a success that changed nothing.
fn grants_to_park(view: &ChannelView, email: &str) -> Vec<EntryId> {
    view.keyring
        .get(email)
        .into_iter()
        .flatten()
        .filter(|grant| grant.retired_at.is_none())
        .map(|grant| grant.granted_by)
        .collect()
}

/// The warning `add-member --enroll` prints when `email` currently holds a
/// revocation cutoff (finding 2, final fix wave): at least one grant and
/// every one of them retired — the exact condition
/// [`junto_kernel::ChannelView::unrecognized`]'s cutoff fold requires
/// (`crates/junto-kernel/src/ledger.rs`'s `project_unrecognized`). `None`
/// when there is no cutoff to disturb: no grant at all, or at least one
/// still active.
///
/// Re-enrolling such a member is legitimate — `Host::add_member`
/// deliberately re-grants a previously retired key — but the fresh grant
/// it appends is unconditionally active, so it drops the cutoff and
/// restores every entry `email` wrote after it back to recognized
/// (standings, gate approvals, session and lineage folds included),
/// silently unless this warns (`docs/adr/0035`'s "Re-admitting a revoked
/// member" consequence). This only detects and names the condition; it
/// never refuses — re-admission must still go through.
fn revocation_cutoff_warning(view: &ChannelView, email: &str, channel: &str) -> Option<String> {
    let grants = view.keyring.get(email)?;
    if grants.is_empty() || grants.iter().any(|grant| grant.retired_at.is_none()) {
        return None;
    }
    Some(format!(
        "warning: {email} currently has a revocation cutoff in channel '{channel}' (every \
         grant they hold is retired) — re-enrolling them grants a fresh ACTIVE key, which \
         restores every entry they wrote after that cutoff (standings, gate approvals, \
         session and lineage folds) back to recognized"
    ))
}

/// A stable, 16-hex-char fingerprint for `key` (device-key-enrollment plan,
/// Task 9) — safe to print on a shared terminal, unlike the full
/// `ed25519:<64 hex>` public key. The 16 hex characters *after* the
/// prefix, not the prefix itself, so two distinct keys never collide on
/// the printed prefix.
fn fingerprint(key: &PublicKey) -> String {
    key.as_str()
        .strip_prefix("ed25519:")
        .unwrap_or(key.as_str())
        .chars()
        .take(16)
        .collect()
}

/// The formatted lines `junto keys list` prints, one per grant: member,
/// signing fingerprint (never the full public key), the transport
/// fingerprint labelled `transport=` (or `transport=none` for a grant made
/// before Task 16, or a keyless member) so a reader can tell which device
/// is reachable over a federation transport (`docs/adr/0033` two-key
/// separation), the granting entry id (`retire-device`'s `--grant`
/// handle), and `active` or its retirement timestamp (device-key-
/// enrollment plan, Task 9). Pure and sorted by email so it is testable
/// without capturing stdout — `keys_list` prints exactly what this
/// returns.
fn keys_list_lines(view: &ChannelView, member: Option<&str>) -> Vec<String> {
    let mut emails: Vec<&String> = match member {
        Some(email) => view
            .keyring
            .keys()
            .filter(|e| e.as_str() == email)
            .collect(),
        None => view.keyring.keys().collect(),
    };
    emails.sort();

    let mut lines = Vec::new();
    for email in emails {
        for grant in &view.keyring[email] {
            let status = match grant.retired_at {
                Some(ts) => format!("retired {}", render::iso_utc(ts.as_millis())),
                None => "active".to_string(),
            };
            let transport = grant
                .transport_key
                .as_ref()
                .map_or_else(|| "none".to_string(), fingerprint);
            lines.push(format!(
                "{email}  {}  transport={transport}  granted_by={}  {status}",
                fingerprint(&grant.key),
                grant.granted_by
            ));
        }
    }
    lines
}

/// `junto keys list` — print every grant in a channel (device-key-
/// enrollment plan, Task 9); see [`keys_list_lines`] for the line shape.
async fn keys_list(channel: String, member: Option<String>) -> Result<()> {
    let host = host::Host::from_registry(host::junto_home()?);
    let (_substrate, ledger, id) = resolve_channel(&host, &channel).await?;
    let view = ledger.lock().await.project(&id).await?;

    let lines = keys_list_lines(&view, member.as_deref());
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
    require_founder(&view, &caller, &channel)?;
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

    let targets = grants_to_park(&view, &member);
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
    require_founder(&view, &caller, &channel)?;

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
            "--channel",
            "acme",
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
        let Err(err) = Cli::try_parse_from([
            "junto",
            "add-member",
            "--channel",
            "acme",
            "--enroll",
            "junto://enroll?code=x",
        ]) else {
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
            "--channel",
            "acme",
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
            "--channel",
            "acme",
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

    /// Ruling from the task-8 brief: `--channel` on `add-member --enroll` is
    /// a SEPARATE invocation from `junto invite`'s and may be a human-typed
    /// name, but `invites::consume` compares the channel string exactly
    /// against the CANONICAL id `junto invite` stores. `add_member` must
    /// resolve `--channel` to that same id before calling `consume`, or a
    /// name-addressed enrollment for a channel the invite really was issued
    /// for comes back as a confidently-wrong `WrongChannel`.
    #[tokio::test]
    async fn add_member_enroll_resolves_channel_by_name_before_consuming_the_invite() {
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

        // `--channel acme` — the NAME, not the id `invites::issue` above was
        // keyed against.
        add_member(
            "acme".to_string(),
            None,
            None,
            Some("human".to_string()),
            Some("Dan".to_string()),
            Some("dan@example.com".to_string()),
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
            id.to_string(),
            None,
            None,
            Some("human".to_string()),
            Some("Dan".to_string()),
            Some("dan@example.com".to_string()),
            None,
            Some(url.clone()),
        )
        .await
        .unwrap();

        let err = add_member(
            id.to_string(),
            None,
            None,
            Some("human".to_string()),
            Some("Dan".to_string()),
            Some("dan@example.com".to_string()),
            None,
            Some(url),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("already"), "{err}");
    }

    /// Review finding 3: once `invites::consume` returns `Ok`, the token is
    /// gone. A downstream failure (here, a bad `--author-*` pairing) must
    /// say the token is already burned, so the operator does not retry the
    /// identical URL and land on the `AlreadyUsed` dead end with no idea
    /// why.
    #[tokio::test]
    async fn add_member_enroll_failure_after_consume_names_the_spent_token() {
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
        let key = PublicKey::new(format!("ed25519:{}", "d".repeat(64))).unwrap();
        let url = build_enroll_url(&token, "alice@example.com", "Alice", &key, expires_at);

        // Only --author-name, no --author-email: fails the "pass both, or
        // neither" check AFTER consume already burned the token.
        let err = add_member(
            id.to_string(),
            None,
            None,
            Some("human".to_string()),
            Some("Dan".to_string()),
            None,
            None,
            Some(url),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("already burned"), "{err}");
    }

    /// Regression guard: the keyless/interactive path (no `--enroll`) must
    /// keep minting for the local-agent case.
    #[tokio::test]
    async fn add_member_keyless_path_still_mints_for_a_local_agent() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo, id) = setup_channel("acme").await;

        add_member(
            id.to_string(),
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
            id.to_string(),
            None,
            None,
            Some("agent".to_string()),
            Some("Dan".to_string()),
            Some("dan@example.com".to_string()),
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
            id.to_string(),
            None,
            None,
            Some("human".to_string()),
            Some("Dan".to_string()),
            Some("dan@example.com".to_string()),
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

    /// A minimal `ChannelView` carrying only `keyring` — every other field
    /// defaulted, since `grants_to_park` reads nothing else.
    fn channel_view_with_keyring(keyring: junto_kernel::Keyring) -> ChannelView {
        ChannelView {
            name: None,
            entries: Vec::new(),
            party: Vec::new(),
            keyring,
            unrecognized: std::collections::HashSet::new(),
            unverified: std::collections::HashSet::new(),
            standings: std::collections::HashMap::new(),
            gate_status: std::collections::HashMap::new(),
            gate_executions: std::collections::HashMap::new(),
            sessions: std::collections::HashMap::new(),
            closed: false,
            lineage: Vec::new(),
        }
    }

    /// The only difference between the retired grant and the two active
    /// ones is `retired_at` — same key, same email, distinct `granted_by`
    /// ids — so a mutation that drops the filter (returns all three) or
    /// inverts it (returns only the retired one) fails this exact
    /// assertion. Two active grants, not one, so a mutation that narrows
    /// to `.next()`/`.last()` of the filtered iterator (satisfying "active"
    /// but not "every") also fails it.
    #[test]
    fn grants_to_park_returns_every_active_grant_when_the_email_has_a_mix() {
        use junto_kernel::KeyGrant;
        let key = PublicKey::new(format!("ed25519:{}", "a".repeat(64))).unwrap();
        let active_a = EntryId::new();
        let active_b = EntryId::new();
        let retired_id = EntryId::new();
        let mut keyring = junto_kernel::Keyring::new();
        keyring.insert(
            "alice@example.com".to_string(),
            vec![
                KeyGrant {
                    key: key.clone(),
                    transport_key: None,
                    granted_by: retired_id,
                    retired_at: Some(Timestamp::from_millis(10)),
                },
                KeyGrant {
                    key: key.clone(),
                    transport_key: None,
                    granted_by: active_a,
                    retired_at: None,
                },
                KeyGrant {
                    key,
                    transport_key: None,
                    granted_by: active_b,
                    retired_at: None,
                },
            ],
        );
        let view = channel_view_with_keyring(keyring);
        assert_eq!(
            grants_to_park(&view, "alice@example.com"),
            vec![active_a, active_b]
        );
    }

    #[test]
    fn grants_to_park_returns_empty_for_an_email_with_no_grants_at_all() {
        let view = channel_view_with_keyring(junto_kernel::Keyring::new());
        assert!(grants_to_park(&view, "nobody@example.com").is_empty());
    }

    /// Pinned to a literal expected string, not to calling `fingerprint`
    /// again — proves the 16 chars come from *after* the 8-char
    /// `ed25519:` prefix, not the prefix itself (which would wrongly read
    /// `ed25519:01234567`).
    #[test]
    fn fingerprint_is_the_16_hex_chars_after_the_prefix_not_the_prefix_itself() {
        let key =
            PublicKey::new(format!("ed25519:{}{}", "0123456789abcdef", "0".repeat(48))).unwrap();
        assert_eq!(fingerprint(&key), "0123456789abcdef");
    }

    #[test]
    fn fingerprint_differs_between_distinct_keys() {
        let key_a = PublicKey::new(format!("ed25519:{}", "1".repeat(64))).unwrap();
        let key_b = PublicKey::new(format!("ed25519:{}", "2".repeat(64))).unwrap();
        assert_ne!(fingerprint(&key_a), fingerprint(&key_b));
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

    #[test]
    fn keys_list_lines_prints_the_fingerprint_never_the_full_key() {
        let key = PublicKey::new(format!("ed25519:{}", "7".repeat(64))).unwrap();
        let grant_id = EntryId::new();
        let mut keyring = junto_kernel::Keyring::new();
        keyring.insert(
            "alice@example.com".to_string(),
            vec![junto_kernel::KeyGrant {
                key,
                transport_key: None,
                granted_by: grant_id,
                retired_at: None,
            }],
        );
        let view = channel_view_with_keyring(keyring);
        let lines = keys_list_lines(&view, None);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("7777777777777777"), "{}", lines[0]);
        assert!(!lines[0].contains(&"7".repeat(64)), "{}", lines[0]);
        assert!(
            lines[0].contains(&format!("granted_by={grant_id}")),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("active"), "{}", lines[0]);
    }

    /// Pinned to the literal expected date string, like `invite_line`'s own
    /// test — not derived by calling `render::iso_utc` again.
    #[test]
    fn keys_list_lines_renders_the_retirement_timestamp_when_set() {
        let key = PublicKey::new(format!("ed25519:{}", "8".repeat(64))).unwrap();
        let grant_id = EntryId::new();
        let mut keyring = junto_kernel::Keyring::new();
        keyring.insert(
            "bob@example.com".to_string(),
            vec![junto_kernel::KeyGrant {
                key,
                transport_key: None,
                granted_by: grant_id,
                retired_at: Some(Timestamp::from_millis(1_700_000_000_000)),
            }],
        );
        let view = channel_view_with_keyring(keyring);
        let lines = keys_list_lines(&view, None);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("retired 2023-11-14 22:13 UTC"),
            "{}",
            lines[0]
        );
        assert!(!lines[0].contains("active"), "{}", lines[0]);
    }

    #[test]
    fn keys_list_lines_with_member_filters_to_exactly_that_email() {
        let key_a = PublicKey::new(format!("ed25519:{}", "1".repeat(64))).unwrap();
        let key_b = PublicKey::new(format!("ed25519:{}", "2".repeat(64))).unwrap();
        let mut keyring = junto_kernel::Keyring::new();
        keyring.insert(
            "alice@example.com".to_string(),
            vec![junto_kernel::KeyGrant {
                key: key_a,
                transport_key: None,
                granted_by: EntryId::new(),
                retired_at: None,
            }],
        );
        keyring.insert(
            "bob@example.com".to_string(),
            vec![junto_kernel::KeyGrant {
                key: key_b,
                transport_key: None,
                granted_by: EntryId::new(),
                retired_at: None,
            }],
        );
        let view = channel_view_with_keyring(keyring);
        let lines = keys_list_lines(&view, Some("alice@example.com"));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("alice@example.com"), "{}", lines[0]);
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
    /// back. `alpha` is named twice (once more than needed) to pin the
    /// dedupe rule: it must still produce exactly one record.
    #[tokio::test]
    async fn invite_issues_one_record_per_channel_for_a_single_token() {
        let _home = crate::host::test_home::HomeGuard::new();
        let (_repo_a, id_a) = setup_channel("alpha").await;
        let (_repo_b, id_b) = setup_channel("beta").await;

        invite(
            vec!["alpha".to_string(), "beta".to_string(), "alpha".to_string()],
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
            "dedupe must collapse the repeated 'alpha' flag into one record: {records:?}"
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

    #[test]
    fn revocation_cutoff_warning_is_none_when_the_email_has_no_grants_at_all() {
        let view = channel_view_with_keyring(junto_kernel::Keyring::new());
        assert!(revocation_cutoff_warning(&view, "nobody@example.com", "acme").is_none());
    }

    /// The mixed fixture — one retired grant, one still active — is the
    /// one that actually distinguishes "every grant retired" from "has A
    /// retired grant": a wrong `.any()`/`.all()` swap, or dropping the
    /// active-grant check entirely, would both still pass a fixture built
    /// from only-retired or only-active grants but fails this one.
    #[test]
    fn revocation_cutoff_warning_is_none_while_any_grant_is_still_active() {
        use junto_kernel::KeyGrant;
        let key = PublicKey::new(format!("ed25519:{}", "b".repeat(64))).unwrap();
        let mut keyring = junto_kernel::Keyring::new();
        keyring.insert(
            "alice@example.com".to_string(),
            vec![
                KeyGrant {
                    key: key.clone(),
                    transport_key: None,
                    granted_by: EntryId::new(),
                    retired_at: Some(Timestamp::from_millis(10)),
                },
                KeyGrant {
                    key,
                    transport_key: None,
                    granted_by: EntryId::new(),
                    retired_at: None,
                },
            ],
        );
        let view = channel_view_with_keyring(keyring);
        assert!(revocation_cutoff_warning(&view, "alice@example.com", "acme").is_none());
    }

    #[test]
    fn revocation_cutoff_warning_fires_and_names_the_email_and_channel_when_every_grant_is_retired()
    {
        use junto_kernel::KeyGrant;
        let key = PublicKey::new(format!("ed25519:{}", "c".repeat(64))).unwrap();
        let mut keyring = junto_kernel::Keyring::new();
        keyring.insert(
            "alice@example.com".to_string(),
            vec![
                KeyGrant {
                    key: key.clone(),
                    transport_key: None,
                    granted_by: EntryId::new(),
                    retired_at: Some(Timestamp::from_millis(10)),
                },
                KeyGrant {
                    key,
                    transport_key: None,
                    granted_by: EntryId::new(),
                    retired_at: Some(Timestamp::from_millis(20)),
                },
            ],
        );
        let view = channel_view_with_keyring(keyring);
        let warning = revocation_cutoff_warning(&view, "alice@example.com", "acme")
            .expect("every grant retired must produce a warning");
        assert!(warning.contains("alice@example.com"), "{warning}");
        assert!(warning.contains("acme"), "{warning}");
        assert!(warning.contains("cutoff"), "{warning}");
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
            channel_id.to_string(),
            None,
            None,
            Some("human".to_string()),
            Some("Dan".to_string()),
            Some("dan@example.com".to_string()),
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
