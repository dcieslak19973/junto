//! The MCP write surface — how agents author ledger entries.
//!
//! `junto serve` exposes the kernel's ledger + gate operations as MCP tools
//! over **streamable HTTP** (`docs/adr/0012`), so any MCP-capable agent
//! (Claude Code first) can record decisions, propose gated actions, and sync
//! the record — junto's designed agent integration path ("agents post via
//! MCP", `docs/architecture.md` §Conversation).
//!
//! Channels are addressed by **name or id** (`docs/adr/0014`): a name is a
//! substrate-scoped label bound by the channel's `ChannelOpened` genesis
//! entry, resolved by the host across every registered substrate; a raw id
//! always resolves. A channel must be **opened** (`open_channel`) before
//! anything can be recorded into it — there is no create-on-first-write.
//!
//! **Known dogfood-era limit — identity is claimed, not verified.** Every tool
//! takes an `author`, and the server records whatever it is told (the kernel
//! has no authn; authorship ≠ authority, `docs/adr/0004`). Fine for a
//! single-user localhost surface; real member identity arrives with the Party
//! work.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use junto_kernel::{
    ApprovalRequirement, ChannelId, ChannelView, ContentDigest, EntryId, EntryPayload, LedgerEntry,
    Member, ProvenanceRef, Timestamp, Uri,
};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use serde::Deserialize;

use crate::host::{Host, Resolution, SharedLedger};
use crate::render;

/// Whether a member is a person or an agent (the MCP-facing mirror of
/// [`junto_kernel::MemberKind`]).
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AuthorKind {
    /// A human participant.
    Human,
    /// An automated agent.
    Agent,
}

/// Who is writing. Identity is **claimed** (see module docs) — pass your real
/// name/email; agents pass their own, not their operator's.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct AuthorParam {
    /// Display name, e.g. "Dan Cieslak" or "Claude Code".
    pub name: String,
    /// Stable identity and sort key, e.g. "dcieslak@hotmail.com".
    pub email: String,
    /// "human" or "agent".
    pub kind: AuthorKind,
}

impl From<AuthorParam> for Member {
    fn from(author: AuthorParam) -> Self {
        match author.kind {
            AuthorKind::Human => Member::human(author.name, author.email),
            AuthorKind::Agent => Member::agent(author.name, author.email),
        }
    }
}

/// One piece of evidence backing a claim or proposal.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct ProvenanceParam {
    /// Where the evidence lives (a URL, git object, file path, PR link…).
    pub uri: String,
    /// Optional content digest in `algorithm:value` form (e.g. "sha256:…"),
    /// captured now so later drift of the target is detectable.
    pub digest: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OpenChannelRequest {
    /// The channel's human-facing name — a label, unique within its home
    /// substrate (docs/adr/0014).
    pub name: String,
    pub author: AuthorParam,
    /// The home substrate repo path. May be omitted when the host serves
    /// exactly one substrate.
    pub repo: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListChannelsRequest {
    /// Limit the listing to this substrate repo path. Omit for every
    /// registered substrate.
    pub repo: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecordRequest {
    /// Channel name (bound at open_channel) or raw channel id.
    pub channel: String,
    pub author: AuthorParam,
    /// The author's member code (docs/adr/0017) — required once the channel
    /// has a Party.
    pub code: Option<String>,
    /// The claim / decision / finding itself.
    pub statement: String,
    /// Why — reasoning and alternatives considered.
    pub rationale: String,
    /// Evidence backing the claim.
    pub provenance: Option<Vec<ProvenanceParam>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ActRequest {
    /// Channel name or id.
    pub channel: String,
    pub author: AuthorParam,
    /// The author's member code (docs/adr/0017) — required once the channel
    /// has a Party.
    pub code: Option<String>,
    /// The id of the entry being acted on (shown by `view_channel`).
    pub target: String,
    /// Why. A rationale, not a checkbox.
    pub rationale: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CorrectRequest {
    /// Channel name or id.
    pub channel: String,
    pub author: AuthorParam,
    /// The author's member code (docs/adr/0017) — required once the channel
    /// has a Party.
    pub code: Option<String>,
    /// The id of the entry being superseded.
    pub target: String,
    /// The corrected claim.
    pub statement: String,
    /// Why the correction was made.
    pub rationale: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ProposeRequest {
    /// Channel name or id.
    pub channel: String,
    pub author: AuthorParam,
    /// The author's member code (docs/adr/0017) — required once the channel
    /// has a Party.
    pub code: Option<String>,
    /// What is being proposed (a generic action descriptor, e.g.
    /// "merge PR #5", "push the slice-7 branch").
    pub action: String,
    /// Why the action is proposed.
    pub rationale: String,
    /// Evidence backing the proposal.
    pub provenance: Option<Vec<ProvenanceParam>>,
    /// Require this many distinct approvers. Omit (with `require_all_of`
    /// also omitted) for an auto-approved gate.
    pub require_count: Option<u32>,
    /// Require every one of these members to approve. Mutually exclusive
    /// with `require_count`.
    pub require_all_of: Option<Vec<AuthorParam>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddMemberRequest {
    /// Channel name or id.
    pub channel: String,
    /// The granter — must be the channel's founding member (docs/adr/0017).
    pub author: AuthorParam,
    /// The founding member's own code.
    pub code: Option<String>,
    /// The member being granted membership.
    pub member: AuthorParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ViewRequest {
    /// Channel name or id.
    pub channel: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SyncRequest {
    /// Channel name or id.
    pub channel: String,
    /// Git remote to sync with (name, URL, or path). Defaults to "origin".
    pub remote: Option<String>,
}

/// The MCP handler: the singleton [`Host`] (`docs/adr/0015`), shared by every
/// connected session and with the web read routes.
#[derive(Clone)]
pub struct JuntoMcp {
    host: Arc<Host>,
}

/// Map an internal failure onto an MCP error (messages are already
/// human-readable; nothing sensitive lives in them).
fn internal(err: impl std::fmt::Display) -> McpError {
    McpError::internal_error(err.to_string(), None)
}

fn invalid(message: impl Into<String>) -> McpError {
    McpError::invalid_params(message.into(), None)
}

/// What kind of entry a verification act may target — mirrors the web
/// route's cross-kind refusal.
#[derive(Clone, Copy)]
enum TargetKind {
    /// ratify / park / correct act on assertions (standing-bearing).
    Assertion,
    /// approve / reject act on proposals (gate-bearing).
    Proposal,
}

/// Resolve an act's target against the channel projection: a full id must
/// *exist* and bear the right kind, and a git-style unambiguous id prefix is
/// accepted — so agents stop reproducing full UUIDs from memory (a fabricated
/// id was accepted silently here once; junto-dev entry `5f625741`). Dangling
/// tolerance remains a *projection* property for entries arriving by sync;
/// an interactive surface can and does tell the author immediately.
fn resolve_target(view: &ChannelView, raw: &str, kind: TargetKind) -> Result<EntryId, McpError> {
    let described = match kind {
        TargetKind::Assertion => "an assertion (ratify/park/correct targets)",
        TargetKind::Proposal => "a proposal (approve/reject targets)",
    };
    let bears_kind = |id: &EntryId| match kind {
        TargetKind::Assertion => view.standing(id).is_some(),
        TargetKind::Proposal => view.gate_status(id).is_some(),
    };

    if let Ok(id) = raw.parse::<EntryId>() {
        return if bears_kind(&id) {
            Ok(id)
        } else {
            Err(invalid(format!(
                "{id} is not {described} in this channel — check the id against view_channel"
            )))
        };
    }

    // A prefix: unambiguous within the right kind, at least 6 chars.
    if raw.len() < 6 || !raw.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(invalid(format!(
            "'{raw}' is not an entry id (expected a UUID or an id prefix of 6+ hex chars)"
        )));
    }
    let mut matches = view
        .entries
        .iter()
        .map(|entry| entry.id)
        .filter(|id| id.to_string().starts_with(raw) && bears_kind(id));
    match (matches.next(), matches.next()) {
        (Some(id), None) => Ok(id),
        (Some(a), Some(b)) => Err(invalid(format!(
            "id prefix '{raw}' is ambiguous (at least {a} and {b}); give more characters"
        ))),
        (None, _) => Err(invalid(format!(
            "no entry matching '{raw}' is {described} in this channel — check view_channel"
        ))),
    }
}

/// Convert provenance params, validating each URI/digest.
fn parse_provenance(params: Option<Vec<ProvenanceParam>>) -> Result<Vec<ProvenanceRef>, McpError> {
    params
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let uri = Uri::new(p.uri).map_err(|e| invalid(e.to_string()))?;
            Ok(match p.digest {
                Some(digest) => {
                    let digest = ContentDigest::new(digest).map_err(|e| invalid(e.to_string()))?;
                    ProvenanceRef::with_digest(uri, digest)
                }
                None => ProvenanceRef::new(uri),
            })
        })
        .collect()
}

/// A successful tool result carrying one text block.
fn text(content: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(content.into())])
}

#[tool_router]
impl JuntoMcp {
    /// A handler over the shared host (one per machine/user, docs/adr/0015).
    pub fn new(host: Arc<Host>) -> Self {
        Self { host }
    }

    /// Resolve a channel reference to its home ledger + id, mapping the
    /// not-found / ambiguous outcomes onto agent-actionable errors.
    async fn resolve(&self, channel: &str) -> Result<(SharedLedger, ChannelId), McpError> {
        match self.host.resolve(channel).await.map_err(internal)? {
            Resolution::Resolved { ledger, id, .. } => Ok((ledger, id)),
            Resolution::NotFound => Err(invalid(format!(
                "no channel '{channel}' in any registered substrate — open it first \
                 (open_channel), or check list_channels"
            ))),
            Resolution::Ambiguous(substrates) => Err(invalid(format!(
                "channel name '{channel}' exists in several substrates ({substrates:?}); \
                 address it by id (see list_channels)"
            ))),
        }
    }

    /// Append one entry and report its id.
    async fn append(
        &self,
        channel: &str,
        ledger: SharedLedger,
        entry: LedgerEntry,
    ) -> Result<CallToolResult, McpError> {
        let id = entry.id;
        ledger.lock().await.append(entry).await.map_err(internal)?;
        Ok(text(format!("recorded {id} in channel '{channel}'")))
    }

    /// The write-surface guardrail (`docs/adr/0017`): project the channel and
    /// refuse a non-member author or a missing/wrong member code. Returns the
    /// projection so callers can validate act targets against it without
    /// re-projecting.
    async fn authorize(
        &self,
        ledger: &SharedLedger,
        channel: &ChannelId,
        author: &Member,
        code: Option<&str>,
    ) -> Result<ChannelView, McpError> {
        let view = ledger
            .lock()
            .await
            .project(channel)
            .await
            .map_err(internal)?;
        self.host
            .authorize_write(&view, author, code)
            .map_err(|err| invalid(err.to_string()))?;
        Ok(view)
    }

    /// Build the envelope for a fresh entry authored now.
    fn entry(channel: ChannelId, author: Member, payload: EntryPayload) -> LedgerEntry {
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author,
            timestamp: Timestamp::now(),
            payload,
        }
    }

    #[tool(
        description = "Open a channel: mint its globally unique id and write the ChannelOpened genesis entry binding the name (unique within its home substrate). A channel must be opened before anything can be recorded into it. `repo` picks the home substrate; omit it when the host serves exactly one."
    )]
    async fn open_channel(
        &self,
        Parameters(req): Parameters<OpenChannelRequest>,
    ) -> Result<CallToolResult, McpError> {
        let repo = req.repo.as_deref().map(Path::new);
        let opened = self
            .host
            .open_channel(repo, &req.name, req.author.into(), None)
            .await
            .map_err(|err| invalid(err.to_string()))?;
        let code_note = if opened.founder_code.newly_minted {
            format!(
                "your member code is {} — pass it as `code` on every write (machine-local, \
                 never in the ledger; docs/adr/0017)",
                opened.founder_code.code
            )
        } else {
            "your existing member code applies — pass it as `code` on every write".to_string()
        };
        Ok(text(format!(
            "opened channel '{}' (id {}); you are its founding member. {code_note}",
            req.name, opened.id
        )))
    }

    #[tool(
        description = "Grant channel membership (docs/adr/0017): the founding member adds a new member (human or agent) to the channel's Party and mints their machine-local member code. Only the founder may grant; pass the founder's own `code`."
    )]
    async fn add_member(
        &self,
        Parameters(req): Parameters<AddMemberRequest>,
    ) -> Result<CallToolResult, McpError> {
        let granted_by: Member = req.author.into();
        let (ledger, channel) = self.resolve(&req.channel).await?;
        self.authorize(&ledger, &channel, &granted_by, req.code.as_deref())
            .await?;
        let member: Member = req.member.into();
        let email = member.email.clone();
        let minted = self
            .host
            .add_member(&req.channel, &granted_by, member)
            .await
            .map_err(|err| invalid(err.to_string()))?;
        let code_note = if minted.newly_minted {
            format!(
                "their member code is {} — hand it to them once",
                minted.code
            )
        } else {
            "they already had a member code on this machine; it still applies".to_string()
        };
        Ok(text(format!(
            "added {email} to channel '{}'. {code_note}",
            req.channel
        )))
    }

    #[tool(
        description = "List every channel across every registered home substrate (or one substrate via `repo`): name, id, home substrate, entry count, open (pending) gates, last activity. Most recently active first."
    )]
    async fn list_channels(
        &self,
        Parameters(req): Parameters<ListChannelsRequest>,
    ) -> Result<CallToolResult, McpError> {
        let mut summaries = self.host.inventory().await.map_err(internal)?;
        if let Some(repo) = req.repo {
            let repo = dunce::canonicalize(Path::new(&repo))
                .map_err(|err| invalid(format!("substrate repo {repo} not found: {err}")))?;
            summaries.retain(|summary| summary.substrate == repo);
        }
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.last_activity));
        if summaries.is_empty() {
            return Ok(text("no channels in any registered substrate"));
        }
        let mut out = String::new();
        for summary in summaries {
            let _ = writeln!(
                out,
                "- {name} · id {id} · {substrate} · {entries} entries · {gates} open gates",
                name = summary.name.as_deref().unwrap_or("(unopened)"),
                id = summary.id,
                substrate = summary.substrate.display(),
                entries = summary.entry_count,
                gates = summary.open_gates,
            );
        }
        Ok(text(out))
    }

    #[tool(
        description = "Record an Assertion — a decision, finding, or claim — in a channel's ledger. It enters with Provisional standing; a member ratifies (or parks/corrects) it later. Give the real why in `rationale`, including alternatives considered, and bind evidence via `provenance`."
    )]
    async fn record(
        &self,
        Parameters(req): Parameters<RecordRequest>,
    ) -> Result<CallToolResult, McpError> {
        let author: Member = req.author.into();
        let (ledger, channel) = self.resolve(&req.channel).await?;
        self.authorize(&ledger, &channel, &author, req.code.as_deref())
            .await?;
        let provenance = parse_provenance(req.provenance)?;
        let entry = Self::entry(
            channel,
            author,
            EntryPayload::Assertion {
                statement: req.statement,
                rationale: req.rationale,
                provenance,
            },
        );
        self.append(&req.channel, ledger, entry).await
    }

    #[tool(
        description = "Ratify a prior entry: accept it as verified, moving its standing to Ratified. Use after the claim has been checked — ratification is the slow, deliberate confirmation, not a reflex."
    )]
    async fn ratify(
        &self,
        Parameters(req): Parameters<ActRequest>,
    ) -> Result<CallToolResult, McpError> {
        let author: Member = req.author.into();
        let (ledger, channel) = self.resolve(&req.channel).await?;
        let view = self
            .authorize(&ledger, &channel, &author, req.code.as_deref())
            .await?;
        let target = resolve_target(&view, &req.target, TargetKind::Assertion)?;
        let entry = Self::entry(
            channel,
            author,
            EntryPayload::Ratification {
                target,
                rationale: req.rationale,
            },
        );
        self.append(&req.channel, ledger, entry).await
    }

    #[tool(
        description = "Park a prior entry: set it aside as a negative, abandoned, or disproven result. Parked entries are kept forever as institutional memory — say in `rationale` whether it was abandoned or disproven."
    )]
    async fn park(
        &self,
        Parameters(req): Parameters<ActRequest>,
    ) -> Result<CallToolResult, McpError> {
        let author: Member = req.author.into();
        let (ledger, channel) = self.resolve(&req.channel).await?;
        let view = self
            .authorize(&ledger, &channel, &author, req.code.as_deref())
            .await?;
        let target = resolve_target(&view, &req.target, TargetKind::Assertion)?;
        let entry = Self::entry(
            channel,
            author,
            EntryPayload::Park {
                target,
                rationale: req.rationale,
            },
        );
        self.append(&req.channel, ledger, entry).await
    }

    #[tool(
        description = "Correct a prior entry: supersede it with a restated claim. The original stays in the log (append-only, like an accounting ledger); its standing becomes Superseded."
    )]
    async fn correct(
        &self,
        Parameters(req): Parameters<CorrectRequest>,
    ) -> Result<CallToolResult, McpError> {
        let author: Member = req.author.into();
        let (ledger, channel) = self.resolve(&req.channel).await?;
        let view = self
            .authorize(&ledger, &channel, &author, req.code.as_deref())
            .await?;
        let target = resolve_target(&view, &req.target, TargetKind::Assertion)?;
        let entry = Self::entry(
            channel,
            author,
            EntryPayload::Correction {
                target,
                statement: req.statement,
                rationale: req.rationale,
            },
        );
        self.append(&req.channel, ledger, entry).await
    }

    #[tool(
        description = "Propose a consequential action for a Gate. The gate's requirement is recorded on the proposal: omit both `require_count` and `require_all_of` for auto-approval, or require N distinct approvers / a specific set of members. Approvals accumulate; one rejection blocks (sticky)."
    )]
    async fn propose(
        &self,
        Parameters(req): Parameters<ProposeRequest>,
    ) -> Result<CallToolResult, McpError> {
        let author: Member = req.author.into();
        let (ledger, channel) = self.resolve(&req.channel).await?;
        self.authorize(&ledger, &channel, &author, req.code.as_deref())
            .await?;
        let requirement = match (req.require_count, req.require_all_of) {
            (None, None) => ApprovalRequirement::Auto,
            (Some(n), None) => ApprovalRequirement::Count(n),
            (None, Some(members)) => {
                ApprovalRequirement::AllOf(members.into_iter().map(Member::from).collect())
            }
            (Some(_), Some(_)) => {
                return Err(invalid("set require_count or require_all_of, not both"));
            }
        };
        let provenance = parse_provenance(req.provenance)?;
        let entry = Self::entry(
            channel,
            author,
            EntryPayload::Proposal {
                action: req.action,
                rationale: req.rationale,
                provenance,
                requirement,
            },
        );
        self.append(&req.channel, ledger, entry).await
    }

    #[tool(
        description = "Approve a proposal. Approvals count once per distinct member; the gate opens when the proposal's recorded requirement is met."
    )]
    async fn approve(
        &self,
        Parameters(req): Parameters<ActRequest>,
    ) -> Result<CallToolResult, McpError> {
        let author: Member = req.author.into();
        let (ledger, channel) = self.resolve(&req.channel).await?;
        let view = self
            .authorize(&ledger, &channel, &author, req.code.as_deref())
            .await?;
        let target = resolve_target(&view, &req.target, TargetKind::Proposal)?;
        let entry = Self::entry(
            channel,
            author,
            EntryPayload::Approval {
                target,
                rationale: req.rationale,
            },
        );
        self.append(&req.channel, ledger, entry).await
    }

    #[tool(
        description = "Reject a proposal. Rejection is sticky: one rejection blocks the gate regardless of approvals, and a later approval does not revive it."
    )]
    async fn reject(
        &self,
        Parameters(req): Parameters<ActRequest>,
    ) -> Result<CallToolResult, McpError> {
        let author: Member = req.author.into();
        let (ledger, channel) = self.resolve(&req.channel).await?;
        let view = self
            .authorize(&ledger, &channel, &author, req.code.as_deref())
            .await?;
        let target = resolve_target(&view, &req.target, TargetKind::Proposal)?;
        let entry = Self::entry(
            channel,
            author,
            EntryPayload::Rejection {
                target,
                rationale: req.rationale,
            },
        );
        self.append(&req.channel, ledger, entry).await
    }

    #[tool(
        description = "Project a channel's ledger: every entry in canonical order with derived standings (assertions) and gate statuses (proposals), rendered as markdown. Entry ids shown here are the targets for ratify/park/correct/approve/reject."
    )]
    async fn view_channel(
        &self,
        Parameters(req): Parameters<ViewRequest>,
    ) -> Result<CallToolResult, McpError> {
        let (ledger, channel) = self.resolve(&req.channel).await?;
        let view = ledger
            .lock()
            .await
            .project(&channel)
            .await
            .map_err(internal)?;
        let name = view.name.clone().unwrap_or_else(|| req.channel.clone());
        Ok(text(render::brief_markdown(&name, &channel, &view)))
    }

    #[tool(
        description = "Sync a channel's record with a git remote (default: origin): fetch every member's entries, reconcile, and push your own. Run after recording so the durable record leaves this machine."
    )]
    async fn sync_channel(
        &self,
        Parameters(req): Parameters<SyncRequest>,
    ) -> Result<CallToolResult, McpError> {
        let (ledger, channel) = self.resolve(&req.channel).await?;
        let remote = req.remote.unwrap_or_else(|| "origin".to_string());
        // Sync lives on the substrate, not the generic Ledger; reach through.
        ledger
            .lock()
            .await
            .substrate_mut()
            .sync(&remote, &channel)
            .await
            .map_err(internal)?;
        Ok(text(format!(
            "synced channel '{}' with remote '{remote}'",
            req.channel
        )))
    }
}

#[tool_handler]
impl ServerHandler for JuntoMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.server_info.name = "junto".into();
        info.server_info.version = env!("CARGO_PKG_VERSION").into();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "junto's ledger: open channels (open_channel) and discover them (list_channels), \
             record decisions/findings (assertions), verify them (ratify/park/correct), gate \
             consequential actions (propose/approve/reject), grant membership (add_member, \
             founder only), inspect a channel (view_channel), and sync the durable record \
             through a git remote (sync_channel). Channels are addressed by name (bound when \
             opened) or id; a channel must be opened before recording into it. Always pass \
             your own identity as `author` — agents author as themselves, never as their \
             operator — plus your member code as `code`: writes require membership in the \
             channel's Party and the matching machine-local code (your operator hands you \
             the code when minting you as a member; docs/adr/0017)."
                .to_string(),
        );
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    /// A host over `count` fresh single-purpose git repos, with its
    /// member-code store in its own temp dir (never the real `~/.junto`).
    fn init_host(count: usize) -> (Vec<TempDir>, JuntoMcp) {
        let mut dirs = Vec::new();
        let mut paths = Vec::new();
        for _ in 0..count {
            let dir = tempfile::tempdir().expect("tempdir");
            let ok = StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .expect("git init")
                .success();
            assert!(ok);
            paths.push(dir.path().to_path_buf());
            dirs.push(dir);
        }
        let member_home = tempfile::tempdir().expect("member home");
        let handler = JuntoMcp::new(Host::fixed_with_member_home(
            paths,
            Some(member_home.path().to_path_buf()),
        ));
        dirs.push(member_home);
        (dirs, handler)
    }

    fn init_repo() -> (Vec<TempDir>, JuntoMcp) {
        init_host(1)
    }

    fn dan() -> AuthorParam {
        AuthorParam {
            name: "Dan".into(),
            email: "dan@example.com".into(),
            kind: AuthorKind::Human,
        }
    }

    fn claude() -> AuthorParam {
        AuthorParam {
            name: "Claude Code".into(),
            email: "claude@junto.invalid".into(),
            kind: AuthorKind::Agent,
        }
    }

    /// The member home the test host was built over (the last temp dir
    /// pushed by [`init_host`]).
    fn member_home(dirs: &[TempDir]) -> &std::path::Path {
        dirs.last().expect("member home").path()
    }

    /// A member's code from the test host's store, minting if absent.
    fn code_of(dirs: &[TempDir], who: AuthorParam) -> Option<String> {
        Some(
            crate::members::mint(member_home(dirs), &who.into())
                .expect("mint")
                .code,
        )
    }

    /// Open `name` in the host's only substrate (Dan is the founder) and add
    /// Claude to its Party, so both fixtures can write.
    async fn open(mcp: &JuntoMcp, dirs: &[TempDir], name: &str) {
        mcp.open_channel(Parameters(OpenChannelRequest {
            name: name.into(),
            author: dan(),
            repo: None,
        }))
        .await
        .expect("open channel");
        mcp.add_member(Parameters(AddMemberRequest {
            channel: name.into(),
            author: dan(),
            code: code_of(dirs, dan()),
            member: claude(),
        }))
        .await
        .expect("add claude");
    }

    /// Pull the single text block out of a tool result.
    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect()
    }

    /// "recorded <id> in channel '<name>'" → the id.
    fn recorded_id(result: &CallToolResult) -> String {
        text_of(result)
            .split_whitespace()
            .nth(1)
            .expect("entry id in confirmation")
            .to_string()
    }

    #[tokio::test]
    async fn record_then_view_shows_the_assertion() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "junto-dev").await;
        let recorded = mcp
            .record(Parameters(RecordRequest {
                channel: "junto-dev".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                statement: "the sky is blue".into(),
                rationale: "observed".into(),
                provenance: Some(vec![ProvenanceParam {
                    uri: "https://example.com/sky".into(),
                    digest: Some("sha256:deadbeef".into()),
                }]),
            }))
            .await
            .unwrap();
        let id = recorded_id(&recorded);

        let view = mcp
            .view_channel(Parameters(ViewRequest {
                channel: "junto-dev".into(),
            }))
            .await
            .unwrap();
        let rendered = text_of(&view);
        assert!(rendered.contains(&id), "view lists the entry id");
        assert!(rendered.contains("the sky is blue"));
        assert!(rendered.contains("[provisional]"));
    }

    #[tokio::test]
    async fn recording_into_an_unopened_channel_is_refused() {
        let (_dirs, mcp) = init_repo();
        let err = mcp
            .record(Parameters(RecordRequest {
                channel: "never-opened".into(),
                author: claude(),
                code: None,
                statement: "s".into(),
                rationale: "r".into(),
                provenance: None,
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("no channel 'never-opened'"));
    }

    #[tokio::test]
    async fn open_enforces_name_uniqueness_per_substrate() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "junto-dev").await;
        let err = mcp
            .open_channel(Parameters(OpenChannelRequest {
                name: "junto-dev".into(),
                author: dan(),
                repo: None,
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("already exists"));
    }

    #[tokio::test]
    async fn channels_resolve_by_raw_id_too() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "junto-dev").await;
        let listed = text_of(
            &mcp.list_channels(Parameters(ListChannelsRequest { repo: None }))
                .await
                .unwrap(),
        );
        // "- junto-dev · id <uuid> · ..." → the uuid.
        let id = listed
            .split("id ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .expect("channel id in listing")
            .to_string();

        let rendered = text_of(
            &mcp.view_channel(Parameters(ViewRequest { channel: id }))
                .await
                .unwrap(),
        );
        assert!(rendered.contains("junto-dev"), "genesis names the channel");
    }

    #[tokio::test]
    async fn ambiguous_names_across_substrates_ask_for_the_id() {
        let (_dirs, mcp) = init_host(2);
        let repos = mcp.host.substrate_paths().unwrap();
        for repo in &repos {
            mcp.open_channel(Parameters(OpenChannelRequest {
                name: "dev".into(),
                author: dan(),
                repo: Some(repo.display().to_string()),
            }))
            .await
            .unwrap();
        }
        let err = mcp
            .view_channel(Parameters(ViewRequest {
                channel: "dev".into(),
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("several substrates"));
    }

    #[tokio::test]
    async fn list_channels_spans_substrates() {
        let (_dirs, mcp) = init_host(2);
        let repos = mcp.host.substrate_paths().unwrap();
        mcp.open_channel(Parameters(OpenChannelRequest {
            name: "alpha".into(),
            author: dan(),
            repo: Some(repos[0].display().to_string()),
        }))
        .await
        .unwrap();
        mcp.open_channel(Parameters(OpenChannelRequest {
            name: "beta".into(),
            author: dan(),
            repo: Some(repos[1].display().to_string()),
        }))
        .await
        .unwrap();

        let listed = text_of(
            &mcp.list_channels(Parameters(ListChannelsRequest { repo: None }))
                .await
                .unwrap(),
        );
        assert!(listed.contains("alpha"));
        assert!(listed.contains("beta"));

        // Substrate-scoped listing filters.
        let scoped = text_of(
            &mcp.list_channels(Parameters(ListChannelsRequest {
                repo: Some(repos[0].display().to_string()),
            }))
            .await
            .unwrap(),
        );
        assert!(scoped.contains("alpha"));
        assert!(!scoped.contains("beta"));
    }

    #[tokio::test]
    async fn ratify_moves_standing() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "junto-dev").await;
        let recorded = mcp
            .record(Parameters(RecordRequest {
                channel: "junto-dev".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                statement: "claim".into(),
                rationale: "because".into(),
                provenance: None,
            }))
            .await
            .unwrap();
        let id = recorded_id(&recorded);

        mcp.ratify(Parameters(ActRequest {
            channel: "junto-dev".into(),
            author: dan(),
            code: code_of(&dirs, dan()),
            target: id,
            rationale: "checked".into(),
        }))
        .await
        .unwrap();

        let rendered = text_of(
            &mcp.view_channel(Parameters(ViewRequest {
                channel: "junto-dev".into(),
            }))
            .await
            .unwrap(),
        );
        assert!(rendered.contains("[ratified]"));
    }

    #[tokio::test]
    async fn propose_approve_opens_the_gate() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "junto-dev").await;
        let proposed = mcp
            .propose(Parameters(ProposeRequest {
                channel: "junto-dev".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                action: "merge slice 7".into(),
                rationale: "tests green".into(),
                provenance: None,
                require_count: Some(1),
                require_all_of: None,
            }))
            .await
            .unwrap();
        let id = recorded_id(&proposed);

        let pending = text_of(
            &mcp.view_channel(Parameters(ViewRequest {
                channel: "junto-dev".into(),
            }))
            .await
            .unwrap(),
        );
        assert!(pending.contains("**proposal** [pending]"));

        mcp.approve(Parameters(ActRequest {
            channel: "junto-dev".into(),
            author: dan(),
            code: code_of(&dirs, dan()),
            target: id,
            rationale: "lgtm".into(),
        }))
        .await
        .unwrap();

        let approved = text_of(
            &mcp.view_channel(Parameters(ViewRequest {
                channel: "junto-dev".into(),
            }))
            .await
            .unwrap(),
        );
        assert!(approved.contains("**proposal** [approved]"));
    }

    #[tokio::test]
    async fn channels_are_isolated_by_name() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "alpha").await;
        open(&mcp, &dirs, "beta").await;
        mcp.record(Parameters(RecordRequest {
            channel: "alpha".into(),
            author: claude(),
            code: code_of(&dirs, claude()),
            statement: "only in alpha".into(),
            rationale: "r".into(),
            provenance: None,
        }))
        .await
        .unwrap();

        let beta = text_of(
            &mcp.view_channel(Parameters(ViewRequest {
                channel: "beta".into(),
            }))
            .await
            .unwrap(),
        );
        assert!(!beta.contains("only in alpha"));
    }

    #[tokio::test]
    async fn bad_inputs_are_invalid_params() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "junto-dev").await;
        // Not a UUID target.
        let err = mcp
            .ratify(Parameters(ActRequest {
                channel: "junto-dev".into(),
                author: dan(),
                code: code_of(&dirs, dan()),
                target: "not-a-uuid".into(),
                rationale: "r".into(),
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("not an entry id"));

        // Both requirement shapes at once.
        let err = mcp
            .propose(Parameters(ProposeRequest {
                channel: "junto-dev".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                action: "a".into(),
                rationale: "r".into(),
                provenance: None,
                require_count: Some(1),
                require_all_of: Some(vec![dan()]),
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("not both"));

        // Malformed digest.
        let err = mcp
            .record(Parameters(RecordRequest {
                channel: "junto-dev".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                statement: "s".into(),
                rationale: "r".into(),
                provenance: Some(vec![ProvenanceParam {
                    uri: "https://x".into(),
                    digest: Some("deadbeef".into()),
                }]),
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("algorithm"));
    }

    // ---- membership & member codes on the write surface (docs/adr/0017) ----

    #[tokio::test]
    async fn non_member_writes_are_refused() {
        let (dirs, mcp) = init_repo();
        // Open without adding Claude: Dan is the only member.
        mcp.open_channel(Parameters(OpenChannelRequest {
            name: "solo".into(),
            author: dan(),
            repo: None,
        }))
        .await
        .unwrap();

        let err = mcp
            .record(Parameters(RecordRequest {
                channel: "solo".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                statement: "s".into(),
                rationale: "r".into(),
                provenance: None,
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("not a member"), "{}", err.message);
    }

    #[tokio::test]
    async fn wrong_or_missing_codes_are_refused() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "junto-dev").await;

        let err = mcp
            .record(Parameters(RecordRequest {
                channel: "junto-dev".into(),
                author: claude(),
                code: Some("WRONG0".into()),
                statement: "s".into(),
                rationale: "r".into(),
                provenance: None,
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("wrong member code"), "{}", err.message);

        let err = mcp
            .record(Parameters(RecordRequest {
                channel: "junto-dev".into(),
                author: claude(),
                code: None,
                statement: "s".into(),
                rationale: "r".into(),
                provenance: None,
            }))
            .await
            .unwrap_err();
        assert!(
            err.message.contains("member code is required"),
            "{}",
            err.message
        );
    }

    #[tokio::test]
    async fn only_the_founder_grants_membership() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "junto-dev").await;

        // Claude is a member, but not the founder.
        let err = mcp
            .add_member(Parameters(AddMemberRequest {
                channel: "junto-dev".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                member: AuthorParam {
                    name: "Interloper".into(),
                    email: "interloper@junto.invalid".into(),
                    kind: AuthorKind::Agent,
                },
            }))
            .await
            .unwrap_err();
        assert!(
            err.message.contains("only the founding member"),
            "{}",
            err.message
        );
    }

    #[tokio::test]
    async fn re_granting_mints_without_a_duplicate_roster_entry() {
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "junto-dev").await;

        // Granting Claude again: no new entry, the existing code applies.
        let result = mcp
            .add_member(Parameters(AddMemberRequest {
                channel: "junto-dev".into(),
                author: dan(),
                code: code_of(&dirs, dan()),
                member: claude(),
            }))
            .await
            .unwrap();
        assert!(
            text_of(&result).contains("already had a member code"),
            "{}",
            text_of(&result)
        );

        let rendered = text_of(
            &mcp.view_channel(Parameters(ViewRequest {
                channel: "junto-dev".into(),
            }))
            .await
            .unwrap(),
        );
        assert_eq!(
            rendered.matches("member added").count(),
            1,
            "one grant entry, not two: {rendered}"
        );
    }

    #[tokio::test]
    async fn act_targets_must_exist_and_bear_the_right_kind() {
        // Finding 5f625741: a fabricated target id must be refused at the
        // interactive surface, not silently recorded as dangling junk.
        let (dirs, mcp) = init_repo();
        open(&mcp, &dirs, "junto-dev").await;
        let recorded = mcp
            .record(Parameters(RecordRequest {
                channel: "junto-dev".into(),
                author: claude(),
                code: code_of(&dirs, claude()),
                statement: "claim".into(),
                rationale: "because".into(),
                provenance: None,
            }))
            .await
            .unwrap();
        let id = recorded_id(&recorded);

        // A well-formed UUID that names no entry is refused.
        let err = mcp
            .ratify(Parameters(ActRequest {
                channel: "junto-dev".into(),
                author: dan(),
                code: code_of(&dirs, dan()),
                target: EntryId::new().to_string(),
                rationale: "r".into(),
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("not an assertion"), "{}", err.message);

        // Approving an assertion (wrong kind) is refused.
        let err = mcp
            .approve(Parameters(ActRequest {
                channel: "junto-dev".into(),
                author: dan(),
                code: code_of(&dirs, dan()),
                target: id.clone(),
                rationale: "r".into(),
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("not a proposal"), "{}", err.message);

        // An unambiguous prefix resolves and the act lands.
        mcp.ratify(Parameters(ActRequest {
            channel: "junto-dev".into(),
            author: dan(),
            code: code_of(&dirs, dan()),
            target: id[..8].to_string(),
            rationale: "checked, by prefix".into(),
        }))
        .await
        .unwrap();
        let rendered = text_of(
            &mcp.view_channel(Parameters(ViewRequest {
                channel: "junto-dev".into(),
            }))
            .await
            .unwrap(),
        );
        assert!(rendered.contains("[ratified]"), "{rendered}");
    }

    #[tokio::test]
    async fn opening_mints_the_founder_code() {
        let (dirs, mcp) = init_repo();
        let result = mcp
            .open_channel(Parameters(OpenChannelRequest {
                name: "fresh".into(),
                author: dan(),
                repo: None,
            }))
            .await
            .unwrap();
        let message = text_of(&result);
        assert!(message.contains("founding member"), "{message}");
        assert!(message.contains("your member code is"), "{message}");
        // The store holds it.
        let dan_member: Member = dan().into();
        let on_file = crate::members::minted_members(member_home(&dirs)).unwrap();
        assert!(on_file.iter().any(|r| r.member.email == dan_member.email));
    }
}
