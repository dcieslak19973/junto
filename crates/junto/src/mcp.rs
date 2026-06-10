//! The MCP write surface — how agents author ledger entries.
//!
//! `junto serve` exposes the kernel's ledger + gate operations as MCP tools
//! over **streamable HTTP** (`docs/adr/0012`), so any MCP-capable agent
//! (Claude Code first) can record decisions, propose gated actions, and sync
//! the record — junto's designed agent integration path ("agents post via
//! MCP", `docs/architecture.md` §Conversation).
//!
//! Channels are addressed by **name**; the name derives the [`ChannelId`]
//! deterministically (`ChannelId::from_name`), so no machine needs a registry.
//!
//! **Known dogfood-era limit — identity is claimed, not verified.** Every tool
//! takes an `author`, and the server records whatever it is told (the kernel
//! has no authn; authorship ≠ authority, `docs/adr/0004`). Fine for a
//! single-user localhost surface; real member identity arrives with the Party
//! work.

use junto_kernel::{
    ApprovalRequirement, ChannelId, ContentDigest, EntryId, EntryPayload, LedgerEntry, Member,
    ProvenanceRef, Timestamp, Uri,
};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use serde::Deserialize;

use crate::render;
use crate::web::SharedLedger;

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
pub struct RecordRequest {
    /// Channel name (e.g. "junto-dev"). Names map to the same channel on
    /// every machine.
    pub channel: String,
    pub author: AuthorParam,
    /// The claim / decision / finding itself.
    pub statement: String,
    /// Why — reasoning and alternatives considered.
    pub rationale: String,
    /// Evidence backing the claim.
    pub provenance: Option<Vec<ProvenanceParam>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ActRequest {
    /// Channel name.
    pub channel: String,
    pub author: AuthorParam,
    /// The id of the entry being acted on (shown by `view_channel`).
    pub target: String,
    /// Why. A rationale, not a checkbox.
    pub rationale: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CorrectRequest {
    /// Channel name.
    pub channel: String,
    pub author: AuthorParam,
    /// The id of the entry being superseded.
    pub target: String,
    /// The corrected claim.
    pub statement: String,
    /// Why the correction was made.
    pub rationale: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ProposeRequest {
    /// Channel name.
    pub channel: String,
    pub author: AuthorParam,
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
pub struct ViewRequest {
    /// Channel name.
    pub channel: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SyncRequest {
    /// Channel name.
    pub channel: String,
    /// Git remote to sync with (name, URL, or path). Defaults to "origin".
    pub remote: Option<String>,
}

/// The MCP handler: the kernel ledger over the git-refs substrate, shared by
/// every connected session (and with the web read routes).
#[derive(Clone)]
pub struct JuntoMcp {
    ledger: SharedLedger,
}

/// Map a kernel error onto an MCP error (kernel messages are already
/// human-readable; nothing sensitive lives in them).
fn internal(err: junto_kernel::Error) -> McpError {
    McpError::internal_error(err.to_string(), None)
}

fn invalid(message: impl Into<String>) -> McpError {
    McpError::invalid_params(message.into(), None)
}

/// Parse an entry id string from a tool argument.
fn parse_entry_id(raw: &str) -> Result<EntryId, McpError> {
    raw.parse()
        .map_err(|_| invalid(format!("'{raw}' is not an entry id (expected a UUID)")))
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
    /// A handler over an existing shared ledger (the host shares one ledger
    /// between the MCP tools and the web read routes).
    pub fn from_ledger(ledger: SharedLedger) -> Self {
        Self { ledger }
    }

    /// Append one entry and report its id.
    async fn append(&self, channel: &str, entry: LedgerEntry) -> Result<CallToolResult, McpError> {
        let id = entry.id;
        self.ledger
            .lock()
            .await
            .append(entry)
            .await
            .map_err(internal)?;
        Ok(text(format!("recorded {id} in channel '{channel}'")))
    }

    /// Build the envelope for a fresh entry authored now.
    fn entry(channel: &str, author: AuthorParam, payload: EntryPayload) -> LedgerEntry {
        LedgerEntry {
            id: EntryId::new(),
            channel: ChannelId::from_name(channel),
            author: author.into(),
            timestamp: Timestamp::now(),
            payload,
        }
    }

    #[tool(
        description = "Record an Assertion — a decision, finding, or claim — in a channel's ledger. It enters with Provisional standing; a member ratifies (or parks/corrects) it later. Give the real why in `rationale`, including alternatives considered, and bind evidence via `provenance`."
    )]
    async fn record(
        &self,
        Parameters(req): Parameters<RecordRequest>,
    ) -> Result<CallToolResult, McpError> {
        let provenance = parse_provenance(req.provenance)?;
        let entry = Self::entry(
            &req.channel,
            req.author,
            EntryPayload::Assertion {
                statement: req.statement,
                rationale: req.rationale,
                provenance,
            },
        );
        self.append(&req.channel, entry).await
    }

    #[tool(
        description = "Ratify a prior entry: accept it as verified, moving its standing to Ratified. Use after the claim has been checked — ratification is the slow, deliberate confirmation, not a reflex."
    )]
    async fn ratify(
        &self,
        Parameters(req): Parameters<ActRequest>,
    ) -> Result<CallToolResult, McpError> {
        let target = parse_entry_id(&req.target)?;
        let entry = Self::entry(
            &req.channel,
            req.author,
            EntryPayload::Ratification {
                target,
                rationale: req.rationale,
            },
        );
        self.append(&req.channel, entry).await
    }

    #[tool(
        description = "Park a prior entry: set it aside as a negative, abandoned, or disproven result. Parked entries are kept forever as institutional memory — say in `rationale` whether it was abandoned or disproven."
    )]
    async fn park(
        &self,
        Parameters(req): Parameters<ActRequest>,
    ) -> Result<CallToolResult, McpError> {
        let target = parse_entry_id(&req.target)?;
        let entry = Self::entry(
            &req.channel,
            req.author,
            EntryPayload::Park {
                target,
                rationale: req.rationale,
            },
        );
        self.append(&req.channel, entry).await
    }

    #[tool(
        description = "Correct a prior entry: supersede it with a restated claim. The original stays in the log (append-only, like an accounting ledger); its standing becomes Superseded."
    )]
    async fn correct(
        &self,
        Parameters(req): Parameters<CorrectRequest>,
    ) -> Result<CallToolResult, McpError> {
        let target = parse_entry_id(&req.target)?;
        let entry = Self::entry(
            &req.channel,
            req.author,
            EntryPayload::Correction {
                target,
                statement: req.statement,
                rationale: req.rationale,
            },
        );
        self.append(&req.channel, entry).await
    }

    #[tool(
        description = "Propose a consequential action for a Gate. The gate's requirement is recorded on the proposal: omit both `require_count` and `require_all_of` for auto-approval, or require N distinct approvers / a specific set of members. Approvals accumulate; one rejection blocks (sticky)."
    )]
    async fn propose(
        &self,
        Parameters(req): Parameters<ProposeRequest>,
    ) -> Result<CallToolResult, McpError> {
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
            &req.channel,
            req.author,
            EntryPayload::Proposal {
                action: req.action,
                rationale: req.rationale,
                provenance,
                requirement,
            },
        );
        self.append(&req.channel, entry).await
    }

    #[tool(
        description = "Approve a proposal. Approvals count once per distinct member; the gate opens when the proposal's recorded requirement is met."
    )]
    async fn approve(
        &self,
        Parameters(req): Parameters<ActRequest>,
    ) -> Result<CallToolResult, McpError> {
        let target = parse_entry_id(&req.target)?;
        let entry = Self::entry(
            &req.channel,
            req.author,
            EntryPayload::Approval {
                target,
                rationale: req.rationale,
            },
        );
        self.append(&req.channel, entry).await
    }

    #[tool(
        description = "Reject a proposal. Rejection is sticky: one rejection blocks the gate regardless of approvals, and a later approval does not revive it."
    )]
    async fn reject(
        &self,
        Parameters(req): Parameters<ActRequest>,
    ) -> Result<CallToolResult, McpError> {
        let target = parse_entry_id(&req.target)?;
        let entry = Self::entry(
            &req.channel,
            req.author,
            EntryPayload::Rejection {
                target,
                rationale: req.rationale,
            },
        );
        self.append(&req.channel, entry).await
    }

    #[tool(
        description = "Project a channel's ledger: every entry in canonical order with derived standings (assertions) and gate statuses (proposals), rendered as markdown. Entry ids shown here are the targets for ratify/park/correct/approve/reject."
    )]
    async fn view_channel(
        &self,
        Parameters(req): Parameters<ViewRequest>,
    ) -> Result<CallToolResult, McpError> {
        let channel = ChannelId::from_name(&req.channel);
        let view = self
            .ledger
            .lock()
            .await
            .project(&channel)
            .await
            .map_err(internal)?;
        Ok(text(render::brief_markdown(&req.channel, &channel, &view)))
    }

    #[tool(
        description = "Sync a channel's record with a git remote (default: origin): fetch every member's entries, reconcile, and push your own. Run after recording so the durable record leaves this machine."
    )]
    async fn sync_channel(
        &self,
        Parameters(req): Parameters<SyncRequest>,
    ) -> Result<CallToolResult, McpError> {
        let remote = req.remote.unwrap_or_else(|| "origin".to_string());
        let channel = ChannelId::from_name(&req.channel);
        // Sync lives on the substrate, not the generic Ledger; reach through.
        self.ledger
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
            "junto's ledger: record decisions/findings (assertions), verify them \
             (ratify/park/correct), gate consequential actions (propose/approve/reject), \
             inspect a channel (view_channel), and sync the durable record through a git \
             remote (sync_channel). Channels are addressed by name. Always pass your own \
             identity as `author` — agents author as themselves, never as their operator."
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

    fn init_repo() -> (TempDir, JuntoMcp) {
        use junto_kernel::Ledger;
        use junto_substrate_git::GitRefsSubstrate;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let dir = tempfile::tempdir().expect("tempdir");
        let ok = StdCommand::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .expect("git init")
            .success();
        assert!(ok);
        let handler = JuntoMcp::from_ledger(Arc::new(Mutex::new(Ledger::new(
            GitRefsSubstrate::open(dir.path().to_path_buf()),
        ))));
        (dir, handler)
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
        let (_dir, mcp) = init_repo();
        let recorded = mcp
            .record(Parameters(RecordRequest {
                channel: "junto-dev".into(),
                author: claude(),
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
    async fn ratify_moves_standing() {
        let (_dir, mcp) = init_repo();
        let recorded = mcp
            .record(Parameters(RecordRequest {
                channel: "junto-dev".into(),
                author: claude(),
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
        let (_dir, mcp) = init_repo();
        let proposed = mcp
            .propose(Parameters(ProposeRequest {
                channel: "junto-dev".into(),
                author: claude(),
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
        let (_dir, mcp) = init_repo();
        mcp.record(Parameters(RecordRequest {
            channel: "alpha".into(),
            author: claude(),
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
        assert!(beta.contains("(no entries)"));
    }

    #[tokio::test]
    async fn bad_inputs_are_invalid_params() {
        let (_dir, mcp) = init_repo();
        // Not a UUID target.
        let err = mcp
            .ratify(Parameters(ActRequest {
                channel: "junto-dev".into(),
                author: dan(),
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
}
