//! The Ledger entry — junto's atomic, immutable unit of record.
//!
//! A [`LedgerEntry`] is **append-only and immutable** (domain decision #8):
//! once written it is never edited or deleted. Mistakes are corrected the way
//! an accounting ledger does it — by appending a *new* entry
//! ([`EntryPayload::Correction`]) that supersedes the target, leaving the
//! original intact and auditable. The current state of a Channel is therefore
//! not stored on any entry; it is **derived by folding the log**
//! ([`crate::Ledger::project`]).
//!
//! There is exactly one envelope ([`LedgerEntry`]) and one closed set of kinds
//! ([`EntryPayload`], decision #9). Verifications (ratify / park / correct) are
//! themselves ledger entries, not a separate event channel.

use crate::{EntryId, Member, ProvenanceRef, Timestamp, gate::ApprovalRequirement, ids::ChannelId};

/// One immutable record in a Channel's Ledger.
///
/// The envelope (id, channel, author, timestamp) is uniform across kinds; the
/// [`payload`](LedgerEntry::payload) carries the kind-specific content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    /// Stable, opaque identifier for this entry.
    pub id: EntryId,
    /// The Channel whose Ledger this entry belongs to.
    pub channel: ChannelId,
    /// Who wrote it — human or agent (decision #11).
    pub author: Member,
    /// When it was written; the primary projection sort key.
    pub timestamp: Timestamp,
    /// The kind-specific content.
    pub payload: EntryPayload,
}

/// The closed set of entry kinds (decision #9).
///
/// An [`Assertion`](EntryPayload::Assertion) is an original claim/decision/
/// finding. The other three are **verification acts** that reference a prior
/// entry by [`EntryId`] and move its standing during projection:
/// [`Ratification`](EntryPayload::Ratification) accepts it,
/// [`Park`](EntryPayload::Park) sets it aside as a negative/abandoned result
/// (decision #13 — Park and Falsify are one kind for now), and
/// [`Correction`](EntryPayload::Correction) supersedes it with a restated claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryPayload {
    /// An original claim, decision, or finding. Alternatives considered live in
    /// `rationale` until a second Playbook proves a richer shape (decision #12).
    Assertion {
        /// The claim itself.
        statement: String,
        /// Why — reasoning, and any alternatives considered.
        rationale: String,
        /// Evidence backing the claim (decision #14).
        provenance: Vec<ProvenanceRef>,
    },
    /// Accepts a prior entry: moves its standing to ratified.
    Ratification {
        /// The entry being ratified.
        target: EntryId,
        /// Why it was accepted.
        rationale: String,
    },
    /// Sets a prior entry aside — a negative or abandoned result, retained in
    /// the log (decision #13).
    Park {
        /// The entry being parked.
        target: EntryId,
        /// Why it was parked.
        rationale: String,
    },
    /// Supersedes a prior entry with a restated claim, leaving the original
    /// intact (decision #8).
    ///
    /// Note: the restated `statement` is recorded but does **not** itself gain
    /// a [`Standing`](crate::Standing) during projection — only the superseded
    /// original is tracked. Surfacing the corrected value as a first-class
    /// standing is deferred (decision #12: keep minimal until a second Playbook
    /// proves the shape).
    Correction {
        /// The entry being superseded.
        target: EntryId,
        /// The corrected claim.
        statement: String,
        /// Why the correction was made.
        rationale: String,
    },
    /// A consequential action awaiting a Gate. Like an
    /// [`Assertion`](EntryPayload::Assertion) it is a *subject* (targets
    /// nothing); its [`GateStatus`](crate::GateStatus) is derived by folding the
    /// [`Approval`](EntryPayload::Approval)/[`Rejection`](EntryPayload::Rejection)
    /// entries that reference it against its `requirement`.
    Proposal {
        /// A generic, repo-agnostic descriptor of the action being proposed
        /// (so a research-persona gate behaves identically to a code-PR gate).
        action: String,
        /// Why the action is proposed.
        rationale: String,
        /// Evidence backing the proposal.
        provenance: Vec<ProvenanceRef>,
        /// What the gate requires before approval — recorded here so the gate
        /// is auditable from the log alone.
        requirement: ApprovalRequirement,
    },
    /// Approves a [`Proposal`](EntryPayload::Proposal). Distinct from
    /// [`Ratification`](EntryPayload::Ratification): approve/reject pass-or-block
    /// an action *before* it happens; ratify confirms a recorded claim *after*.
    Approval {
        /// The proposal being approved.
        target: EntryId,
        /// Why it was approved.
        rationale: String,
    },
    /// Rejects a [`Proposal`](EntryPayload::Proposal). Reject is *sticky* — one
    /// rejection blocks the gate regardless of approvals.
    Rejection {
        /// The proposal being rejected.
        target: EntryId,
        /// Why it was rejected.
        rationale: String,
    },
}

impl EntryPayload {
    /// The entry this payload acts upon, if it acts on a prior entry.
    ///
    /// Returns `None` for the *subject* kinds — [`Assertion`](EntryPayload::Assertion)
    /// and [`Proposal`](EntryPayload::Proposal), which target nothing — and
    /// `Some(target)` for the acts that reference a prior entry (ratify / park /
    /// correct / approve / reject).
    #[must_use]
    pub fn target(&self) -> Option<EntryId> {
        match self {
            EntryPayload::Assertion { .. } | EntryPayload::Proposal { .. } => None,
            EntryPayload::Ratification { target, .. }
            | EntryPayload::Park { target, .. }
            | EntryPayload::Correction { target, .. }
            | EntryPayload::Approval { target, .. }
            | EntryPayload::Rejection { target, .. } => Some(*target),
        }
    }
}
