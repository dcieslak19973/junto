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
//! Domain types are deliberately not modelled yet — the ledger-entry content
//! model is still an open decision (`junto.md`, open item *b*). This crate
//! currently establishes the workspace, the error convention, and the seam.

#![forbid(unsafe_code)]

pub mod error;

pub use error::{Error, Result};

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
