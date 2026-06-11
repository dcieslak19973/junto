//! The Ledger entry — junto's atomic, immutable unit of record.
//!
//! A [`LedgerEntry`] is **append-only and immutable** (`docs/adr/0002`):
//! once written it is never edited or deleted. Mistakes are corrected the way
//! an accounting ledger does it — by appending a *new* entry
//! ([`EntryPayload::Correction`]) that supersedes the target, leaving the
//! original intact and auditable. The current state of a Channel is therefore
//! not stored on any entry; it is **derived by folding the log**
//! ([`crate::Ledger::project`]).
//!
//! There is exactly one envelope ([`LedgerEntry`]) and one closed set of kinds
//! ([`EntryPayload`], `docs/adr/0003`). Verifications (ratify / park / correct) are
//! themselves ledger entries, not a separate event channel.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{EntryId, Member, ProvenanceRef, Timestamp, gate::ApprovalRequirement, ids::ChannelId};

/// One immutable record in a Channel's Ledger.
///
/// The envelope (id, channel, author, timestamp) is uniform across kinds; the
/// [`payload`](LedgerEntry::payload) carries the kind-specific content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Stable, opaque identifier for this entry.
    pub id: EntryId,
    /// The Channel whose Ledger this entry belongs to.
    pub channel: ChannelId,
    /// Who wrote it — human or agent (`docs/adr/0004`).
    pub author: Member,
    /// When it was written; the primary projection sort key.
    pub timestamp: Timestamp,
    /// The kind-specific content.
    pub payload: EntryPayload,
}

/// Which verification act choosing a [`FrameOption`] performs
/// (`docs/adr/0019`). Which acts are *coherent* for an entry kind —
/// ratify/park on assertions, approve/reject on proposals — is a write-
/// surface concern; the kernel stores what it is given (`docs/adr/0004`'s
/// spirit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameAct {
    Ratify,
    Park,
    Approve,
    Reject,
}

/// One articulated position in a [`DecisionFrame`]: choosing it performs
/// `act` with `rationale` as the (editable) draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameOption {
    /// The choice as the verifier reads it, e.g. "ship it".
    pub label: String,
    /// The verification act this option performs.
    pub act: FrameAct,
    /// The drafted rationale the verifier adopts by choosing (and may edit).
    pub rationale: String,
}

/// A decision frame (`docs/adr/0019`): the proposer articulates the
/// verifier's decision space. Durable **including the options not chosen** —
/// alternatives-considered as structure, the richer shape `docs/adr/0003`
/// anticipated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionFrame {
    /// The articulated positions, 2–4 by convention (enforced at the write
    /// surfaces, not here).
    pub options: Vec<FrameOption>,
}

/// The closed set of entry kinds (`docs/adr/0003`).
///
/// An [`Assertion`](EntryPayload::Assertion) is an original claim/decision/
/// finding. The other three are **verification acts** that reference a prior
/// entry by [`EntryId`] and move its standing during projection:
/// [`Ratification`](EntryPayload::Ratification) accepts it,
/// [`Park`](EntryPayload::Park) sets it aside as a negative/abandoned result
/// (`docs/adr/0003` — Park and Falsify are one kind for now), and
/// [`Correction`](EntryPayload::Correction) supersedes it with a restated claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryPayload {
    /// The channel's genesis: the recorded act of *opening* it, binding the
    /// human-facing `name` to the envelope's (minted, globally unique) channel
    /// id (`docs/adr/0014`, `docs/adr/0016`). Canonically the first entry in a
    /// channel's ledger; the opener and the moment live on the envelope like
    /// every other entry. First of the anticipated lifecycle family
    /// (fork / close follow the same pattern when designed).
    ChannelOpened {
        /// The human-facing label — unique within the home substrate, *not*
        /// identity (`docs/adr/0014`).
        name: String,
    },
    /// The founding member grants channel membership to `member`
    /// (`docs/adr/0017`) — the second entry kind in the lifecycle family
    /// (`docs/adr/0016`). Only the founder (the genesis author) can extend the
    /// Party: a `MemberAdded` authored by anyone else is recorded but has no
    /// roster effect during projection.
    MemberAdded {
        /// The member being granted membership — human or agent.
        member: Member,
    },
    /// An original claim, decision, or finding. Alternatives considered live in
    /// `rationale` — or, structurally, in the optional `frame` (`docs/adr/0019`).
    Assertion {
        /// The claim itself.
        statement: String,
        /// Why — reasoning, and any alternatives considered.
        rationale: String,
        /// Evidence backing the claim (`docs/adr/0005`).
        provenance: Vec<ProvenanceRef>,
        /// The proposer's articulation of the verifier's choices
        /// (`docs/adr/0019`). Omitted from the canonical bytes when absent,
        /// so every pre-frame entry's bytes are unchanged.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        frame: Option<DecisionFrame>,
    },
    /// Accepts a prior entry: moves its standing to ratified.
    Ratification {
        /// The entry being ratified.
        target: EntryId,
        /// Why it was accepted.
        rationale: String,
    },
    /// Sets a prior entry aside — a negative or abandoned result, retained in
    /// the log (`docs/adr/0003`).
    Park {
        /// The entry being parked.
        target: EntryId,
        /// Why it was parked.
        rationale: String,
    },
    /// Supersedes a prior entry with a restated claim, leaving the original
    /// intact (`docs/adr/0002`).
    ///
    /// Note: the restated `statement` is recorded but does **not** itself gain
    /// a [`Standing`](crate::Standing) during projection — only the superseded
    /// original is tracked. Surfacing the corrected value as a first-class
    /// standing is deferred (`docs/adr/0003`: keep minimal until a second Playbook
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
        /// The proposer's articulation of the approver's choices
        /// (`docs/adr/0019`). Omitted from the canonical bytes when absent.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        frame: Option<DecisionFrame>,
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

impl LedgerEntry {
    /// The canonical total order of the record: `(timestamp, author email, id)`
    /// (`docs/adr/0010`). The id tie-break makes this a *total* order, so every
    /// replica sorts an identical entry set identically — which matters because
    /// standing is last-applicable-wins during projection.
    ///
    /// This is the **one definition** of record order: [`crate::Ledger::project`]
    /// sorts with it, and substrates that materialize a log in canonical order
    /// (e.g. a sync union-merge) reuse it rather than re-deriving the key.
    #[must_use]
    pub fn canonical_cmp(&self, other: &Self) -> Ordering {
        self.timestamp
            .cmp(&other.timestamp)
            .then_with(|| self.author.email.cmp(&other.author.email))
            .then_with(|| self.id.cmp(&other.id))
    }
}

impl EntryPayload {
    /// The entry this payload acts upon, if it acts on a prior entry.
    ///
    /// Returns `None` for the kinds that target nothing — the *subject* kinds
    /// [`Assertion`](EntryPayload::Assertion) and [`Proposal`](EntryPayload::Proposal),
    /// and the lifecycle acts [`ChannelOpened`](EntryPayload::ChannelOpened) /
    /// [`MemberAdded`](EntryPayload::MemberAdded) — and `Some(target)` for the
    /// acts that reference a prior entry (ratify / park / correct / approve /
    /// reject).
    #[must_use]
    pub fn target(&self) -> Option<EntryId> {
        match self {
            EntryPayload::ChannelOpened { .. }
            | EntryPayload::MemberAdded { .. }
            | EntryPayload::Assertion { .. }
            | EntryPayload::Proposal { .. } => None,
            EntryPayload::Ratification { target, .. }
            | EntryPayload::Park { target, .. }
            | EntryPayload::Correction { target, .. }
            | EntryPayload::Approval { target, .. }
            | EntryPayload::Rejection { target, .. } => Some(*target),
        }
    }
}
