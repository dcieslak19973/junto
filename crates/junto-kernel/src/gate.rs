//! Gate vocabulary — what a checkpoint requires, and where it stands.
//!
//! A **Gate** is the checkpoint a *consequential action* must pass before it
//! happens. The kernel hosts a generic Gate *engine* but deliberately does
//! **not** decide which path a gate takes — that routing decision is the single
//! most playbook-specific thing (constraint #5) and belongs to a future
//! **Rubric** layer (importable, addressable routing rules resolved via a
//! provider, not bound to any repo). The kernel only *executes a requirement it
//! is handed*: it records the proposal + approvals as ledger entries and
//! derives the [`GateStatus`] by folding them (see [`crate::Ledger::project`]).
//!
//! This module is just the vocabulary; the fold lives in `ledger.rs`, which
//! owns projection.

use serde::{Deserialize, Serialize};

use crate::Member;

/// What a Gate requires before its proposed action is approved.
///
/// A Rubric compiles its routing presets (auto / single-approver / full-review
/// / hard-gated) down to one of these; the kernel never sees the preset names.
/// The requirement is recorded on the [`Proposal`](crate::EntryPayload::Proposal)
/// entry, so a gate's outcome is auditable from the log alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalRequirement {
    /// No approval needed — the action is auto-approved.
    Auto,
    /// Any `n` distinct members approving is sufficient (e.g. "needs 2
    /// approvals"). Approvals from the *same* member do not stack.
    Count(u32),
    /// Every listed member must approve (e.g. "needs both Alice and Bob").
    /// Membership is matched by [`Member::email`] — the stable identity.
    AllOf(Vec<Member>),
}

/// The derived standing of a [`Proposal`](crate::EntryPayload::Proposal) after
/// folding its approvals and rejections against its
/// [`ApprovalRequirement`].
///
/// Like [`Standing`](crate::Standing), this is **never stored** — it is
/// computed from the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateStatus {
    /// Proposed, requirement not yet met, not rejected.
    Pending,
    /// The requirement is satisfied.
    Approved,
    /// Blocked by a rejection. Reject is *sticky* — a single rejection blocks
    /// the gate regardless of approvals (an administrative override to undo a
    /// rejection is a deferred future kind).
    Rejected,
}
