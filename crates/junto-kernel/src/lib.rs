//! # junto-kernel
//!
//! The **generic, playbook-agnostic core** of junto. See `domain-model.md` for
//! the ubiquitous language and `CLAUDE.md` for the hard constraints.
//!
//! The kernel owns only the nouns that every Playbook shares — Channel,
//! Member/Party, Message, Artifact, Provenance, Agent Session, the Gate engine,
//! the Ledger (and its entries), Outcome, and Event. It contains **no
//! playbook-specific logic and no vendor names**: those live in playbook crates
//! and behind adapter traits, respectively.
//!
//! The first modelled slice is the **Ledger**: an immutable, append-only log of
//! [`LedgerEntry`] values projected into current standings ([`Ledger::project`]),
//! stored behind the [`SubstrateProvider`] seam (in-memory today, git-refs
//! later). The ledger-entry content model is locked per `domain-model.md`
//! decisions #8–#14.

#![forbid(unsafe_code)]

pub mod entry;
pub mod error;
pub mod ids;
pub mod ledger;
pub mod member;
pub mod provenance;
pub mod substrate;
pub mod time;

pub use entry::{EntryPayload, LedgerEntry};
pub use error::{Error, Result};
pub use ids::{ChannelId, EntryId};
pub use ledger::{ChannelView, Ledger, Standing};
pub use member::{Member, MemberKind};
pub use provenance::{ContentDigest, ProvenanceRef, Uri};
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
