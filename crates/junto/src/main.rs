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
use junto_kernel::{ChannelId, EntryId, Member, PublicKey, Timestamp};
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
        /// keypair here anyway. Required unless --enroll is passed, whose
        /// enrollment act already tells us this is a human (see --enroll).
        #[arg(long, required_unless_present = "enroll", conflicts_with = "enroll")]
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
        /// `--name`/`--kind` — an enrolled device is by construction a
        /// remote human, so kind is derived, never taken from a flag.
        #[arg(long, conflicts_with_all = ["email", "name", "kind"])]
        enroll: Option<String>,
    },
    /// Mint a founder-issued enrollment invite (device-key-enrollment
    /// plan, Task 6): the first leg of the three-step exchange that lets a
    /// new device join without a private key ever leaving it. Only the
    /// channel's founding member may issue one — an invite the caller
    /// cannot themselves complete would send the recipient through the
    /// whole exchange to fail at `junto add-member`.
    Invite {
        /// The new member's email — the invite is a grant for exactly this
        /// identity; `junto enroll` reads it back off the invite, never
        /// re-typed by the enrolling device.
        #[arg(long)]
        member: String,
        /// Channel name or id to invite them into.
        #[arg(long)]
        channel: String,
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
    }
}

/// Grant channel membership from the terminal (`docs/adr/0017`). The granter
/// defaults to the home substrate's git user — the founder check happens in
/// the host. No code is demanded here: whoever can run commands on this
/// machine can edit the code store anyway; codes guard the network surfaces.
///
/// With `--enroll`, this completes the three-step exchange
/// (device-key-enrollment plan, Task 8): the member's identity and public
/// key come from the validated enroll payload, never re-typed, and the
/// invite token is burned via `invites::consume` before anything is
/// recorded — see the ordering comment at the call site below.
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
    // issued for.
    let (substrate, id) = match host.resolve(&channel).await? {
        host::Resolution::Resolved { substrate, id, .. } => (substrate, id),
        host::Resolution::NotFound => {
            bail!("no channel '{channel}' in any registered substrate")
        }
        host::Resolution::Ambiguous(substrates) => bail!(
            "channel name '{channel}' exists in several substrates ({substrates:?}); \
             address it by id"
        ),
    };

    // Whether this member's key came from `--enroll` — once true, the
    // invite token below is already burned, so every failure from here on
    // must say so (see `spent_token_context`).
    let mut enrolled = false;
    let (member, key) = match enroll_url {
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
            // An enrolled device is by construction a remote human — this
            // is the whole point of the exchange. `--kind` conflicts with
            // `--enroll` on the `Command::AddMember` definition, so there
            // is no flag to (mis)consult here; deriving it, like the email
            // in `enroll_payload_from_invite`, keeps the recorded kind tied
            // to what actually happened rather than what was typed.
            let member = Member::human(&payload.display_name, &payload.email);
            (member, Some(payload.public_key.clone()))
        }
        None => {
            let email = email.expect("clap requires email unless --enroll is passed");
            let name = name.expect("clap requires --name unless --enroll is passed");
            let kind = kind.expect("clap requires --kind unless --enroll is passed");
            let member = match kind.as_str() {
                "human" => Member::human(&name, &email),
                "agent" => Member::agent(&name, &email),
                other => bail!("--kind must be 'human' or 'agent', not '{other}'"),
            };
            (member, None)
        }
    };
    let email = member.email.clone();

    let granted_by = match (author_name, author_email) {
        (Some(name), Some(email)) => Ok(Member::human(name, email)),
        (None, None) => host::git_user(&substrate),
        _ => Err(anyhow!(
            "pass both --author-name and --author-email, or neither"
        )),
    };
    let granted_by = spent_token_context(granted_by, enrolled)?;
    let minted = spent_token_context(
        host.add_member(&channel, &granted_by, member, key).await,
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
/// (device-key-enrollment plan, Task 6). Refuses if the channel does not
/// exist, or if the caller (this machine's git user) is not the channel's
/// founding member (`view.party.first()`) — an invite the caller cannot
/// themselves complete is worse than an error.
async fn invite(channel: String, member: String) -> Result<()> {
    let host = host::Host::from_registry(host::junto_home()?);
    let (substrate, ledger, id) = match host.resolve(&channel).await? {
        host::Resolution::Resolved {
            substrate,
            ledger,
            id,
        } => (substrate, ledger, id),
        host::Resolution::NotFound => {
            bail!("no channel '{channel}' in any registered substrate")
        }
        host::Resolution::Ambiguous(substrates) => bail!(
            "channel name '{channel}' exists in several substrates ({substrates:?}); \
             address it by id"
        ),
    };
    let view = ledger.lock().await.project(&id).await?;
    let Some(founder) = view.party.first() else {
        bail!(
            "channel '{channel}' has no genesis, so it has no founding member to issue \
             invites (membership is not enforced on pre-genesis channels)"
        );
    };
    let caller = host::git_user(&substrate)?;
    if founder.email != caller.email {
        bail!(
            "only the founding member ({} <{}>) can issue invites for '{channel}' \
             (docs/adr/0017)",
            founder.display_name,
            founder.email
        );
    }
    if member.chars().count() > enroll::MAX_FIELD_CHARS {
        bail!(
            "--member exceeds the {}-char limit",
            enroll::MAX_FIELD_CHARS
        );
    }

    let token = enroll::mint_invite_token();
    let expires_at = Timestamp::now().as_millis() + enroll::MAX_INVITE_TTL_MS;
    // The invite's channel field carries the RESOLVED id, not whatever the
    // caller typed (a name or an id): `invites::consume` (Task 8) compares
    // it exactly, so an invite minted with `--channel <name>` must match an
    // enrollment completed against `--channel <id>` for the same channel —
    // otherwise the two legs of one exchange land on different strings and
    // redemption fails with a misleading `WrongChannel`.
    let canonical_channel = id.to_string();
    invites::issue(
        &host::junto_home()?,
        &token,
        &member,
        &canonical_channel,
        expires_at,
    )?;
    let url = enroll::encode_invite(&enroll::InvitePayload {
        v: 1,
        invite_token: token,
        member_email: member,
        channel: canonical_channel,
        expires_at,
    })?;
    println!("{}", invite_line(&url, expires_at));
    Ok(())
}

/// Format `junto invite`'s output: the shareable URI plus a human-readable
/// expiry, pinned so this shape is testable without a CLI harness.
fn invite_line(url: &str, expires_at: i64) -> String {
    format!("{url}\n(expires {})", render::iso_utc(expires_at))
}

/// Build the `junto enroll` response payload from a validated invite. The
/// invite token is echoed back verbatim (proves which grant this answers),
/// the email comes from the invite itself — never re-typed by the device,
/// which would defeat `invites::consume`'s `WrongMember` check — and
/// `public_key` is the freshly minted (or reused) key's public half.
fn enroll_payload_from_invite(
    invite: &enroll::InvitePayload,
    key: &PublicKey,
    name: &str,
) -> enroll::EnrollPayload {
    enroll::EnrollPayload {
        v: 1,
        invite_token: invite.invite_token.clone(),
        email: invite.member_email.clone(),
        display_name: name.to_string(),
        public_key: key.clone(),
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
    let payload = enroll_payload_from_invite(&invite, &key.public_key(), &display_name);
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
                match ledger.lock().await.project(&id).await {
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
        let line = invite_line(url, expires_at);
        assert!(line.contains(url), "line should contain the URI: {line}");
        assert!(
            line.contains("2023-11-14 22:13 UTC"),
            "line should contain a human-readable expiry: {line}"
        );
    }

    #[test]
    fn enroll_payload_carries_the_invites_email_and_token_verbatim() {
        let invite = enroll::InvitePayload {
            v: 1,
            invite_token: "tok-abc-123".to_string(),
            member_email: "dan@example.com".to_string(),
            channel: "junto-dev".to_string(),
            expires_at: 1_700_000_000_000,
        };
        let key = PublicKey::new(format!("ed25519:{}", "a".repeat(64))).unwrap();
        let payload = enroll_payload_from_invite(&invite, &key, "Dan's Laptop");
        assert_eq!(payload.v, 1);
        assert_eq!(payload.invite_token, invite.invite_token);
        assert_eq!(payload.email, invite.member_email);
        assert_eq!(payload.public_key, key);
        assert_eq!(payload.display_name, "Dan's Laptop");
        assert_eq!(payload.expires_at, invite.expires_at);
    }

    fn git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
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

    fn build_enroll_url(
        token: &str,
        email: &str,
        display_name: &str,
        key: &PublicKey,
        expires_at: i64,
    ) -> String {
        enroll::encode_enroll(&enroll::EnrollPayload {
            v: 1,
            invite_token: token.to_string(),
            email: email.to_string(),
            display_name: display_name.to_string(),
            public_key: key.clone(),
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

    /// Review finding 2: an enrolled device is by construction a remote
    /// human — `--kind` must not be taken from a flag on that path, so
    /// clap refuses the combination outright rather than silently ignoring
    /// whatever `--kind` claims.
    #[test]
    fn add_member_kind_conflicts_with_enroll() {
        let Err(err) = Cli::try_parse_from([
            "junto",
            "add-member",
            "--channel",
            "acme",
            "--enroll",
            "junto://enroll?code=x",
            "--kind",
            "human",
        ]) else {
            panic!("expected a clap parse error");
        };
        assert!(err.to_string().contains("--kind"), "{err}");
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
            None,
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
            None,
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
            None,
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
            None,
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
}
