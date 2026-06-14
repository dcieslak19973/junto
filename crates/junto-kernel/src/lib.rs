//! # junto-kernel
//!
//! The **generic, playbook-agnostic core** of junto. See `domain-model.md` for
//! the ubiquitous language and `CLAUDE.md` for the hard constraints.
//!
//! The kernel owns only the nouns that every Playbook shares. **Modelled so
//! far:** Member, Provenance, the Gate engine, the Ledger (and its entries),
//! and Agent Sessions with their Artifacts (as ledger entries — [`session`]).
//! **Planned, not yet modelled:** Channel (today just a [`ChannelId`]),
//! Message, Deliverable, and Event. It contains
//! **no playbook-specific logic and no vendor names**: those live in playbook
//! crates and behind adapter traits, respectively.
//!
//! The first modelled slice is the **Ledger**: an immutable, append-only log of
//! [`LedgerEntry`] values projected into current standings ([`Ledger::project`]),
//! stored behind the [`SubstrateProvider`] seam (in-memory here; git-refs in
//! the `junto-substrate-git` crate). Each entry has a deterministic canonical
//! byte form ([`LedgerEntry::to_canonical_bytes`], see [`serial`]). The
//! ledger-entry content model is locked per the ADRs in `docs/adr/`
//! (`0001`–`0011`).

#![forbid(unsafe_code)]

pub mod entry;
pub mod error;
pub mod gate;
pub mod ids;
pub mod ledger;
pub mod member;
pub mod provenance;
pub mod serial;
pub mod session;
pub mod substrate;
pub mod time;

pub use entry::{DecisionFrame, EntryPayload, FrameAct, FrameOption, LedgerEntry};
pub use error::{Error, Result};
pub use gate::{ApprovalRequirement, GateStatus};
pub use ids::{ChannelId, EntryId};
pub use ledger::{ChannelView, Ledger, Standing};
pub use member::{Member, MemberKind};
pub use provenance::{ContentDigest, ProvenanceRef, Uri};
pub use session::{SessionState, SessionView};
pub use substrate::{InMemorySubstrate, SubstrateProvider};
pub use time::Timestamp;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_displays() {
        let err = Error::Invariant("party must be non-empty".into());
        assert_eq!(
            err.to_string(),
            "kernel invariant violated: party must be non-empty"
        );
    }
}
