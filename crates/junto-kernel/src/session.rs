//! Agent Session vocabulary — the live state of one agent execution, and the
//! point-in-time view a projection derives for it.
//!
//! An **Agent Session** (always qualified — bare "session" is reserved, see
//! `docs/domain-model.md`) is one agent execution in a channel: a harness
//! invocation that does work and produces **Artifacts**. Like every other
//! piece of channel state, a session lives in the Ledger as entries
//! (`docs/adr/0016`'s pattern): a
//! [`SessionStarted`](crate::EntryPayload::SessionStarted) subject entry is
//! the session's identity, [`SessionUpdated`](crate::EntryPayload::SessionUpdated)
//! acts move its state, and
//! [`ArtifactAttached`](crate::EntryPayload::ArtifactAttached) acts bind its
//! outputs. The current [`SessionState`] is **never stored** — it is derived
//! by folding the log (see [`crate::Ledger::project`]), last-applicable-wins,
//! exactly like assertion [`Standing`](crate::Standing).
//!
//! This module is just the vocabulary; the fold lives in `ledger.rs`, which
//! owns projection.

use serde::{Deserialize, Serialize};

use crate::EntryId;
use crate::anchor::CommitOid;

/// The live state of an Agent Session (`docs/domain-model.md`).
///
/// A session starts [`Working`](SessionState::Working) implicitly — the
/// `SessionStarted` entry carries no state — and moves only via
/// [`SessionUpdated`](crate::EntryPayload::SessionUpdated) acts.
/// [`Done`](SessionState::Done) and [`Error`](SessionState::Error) are
/// terminal by convention, but the kernel does not enforce that: a later
/// update wins, consistent with last-applicable-wins everywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// The agent is actively working.
    Working,
    /// The agent is stuck and needs something (input, access, a decision).
    Blocked,
    /// The agent has proposed a gated action and is waiting on the gate.
    AwaitingApproval,
    /// The session finished its work.
    Done,
    /// The session failed.
    Error,
}

/// The commit range one Agent Session's work landed in — `base` exclusive,
/// `head` inclusive, as
/// [`SessionCommitted`](crate::EntryPayload::SessionCommitted) recorded it.
///
/// The two oids travel together so "a base without a head" cannot be
/// represented: a range either exists in full or not at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRange {
    /// The commit the work starts from, exclusive.
    pub base: CommitOid,
    /// The commit the work reached, inclusive.
    pub head: CommitOid,
    /// The branch it landed on, when git could say.
    pub branch: Option<String>,
}

/// A point-in-time view of one Agent Session, derived during projection:
/// its current [`SessionState`], the ids of the
/// [`ArtifactAttached`](crate::EntryPayload::ArtifactAttached) entries that
/// bound outputs to it, in canonical order, and the commit range its work
/// landed in if one was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    /// The session's current state after folding its updates.
    pub state: SessionState,
    /// Entry ids of the artifacts attached to this session, in canonical
    /// order. The artifact content lives wherever its provenance points —
    /// never in the ledger.
    pub artifacts: Vec<EntryId>,
    /// The commit range this session's work landed in, folded
    /// last-applicable-wins from its
    /// [`SessionCommitted`](crate::EntryPayload::SessionCommitted) acts.
    ///
    /// `None` is the honest reading rather than a gap: the session has
    /// recorded no commits, either because it produced none or because it
    /// ran before the range was recorded at all.
    pub commits: Option<CommitRange>,
}
