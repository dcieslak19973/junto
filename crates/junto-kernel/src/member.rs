//! Members — the authors of ledger entries.
//!
//! A [`Member`] is a participant in a Channel: a **human or an agent**. Agents
//! are first-class authors (`docs/adr/0004`) — the kernel does *not*
//! restrict who may write a [`crate::LedgerEntry`]. Any policy that some action
//! requires a human, or an eval-gated agent, lives at the Gate/Verifier layer,
//! not in authorship.

use serde::{Deserialize, Serialize};

/// Whether a [`Member`] is a person or an automated agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemberKind {
    /// A human participant.
    Human,
    /// An automated agent — a first-class peer, not a tool.
    Agent,
}

/// A participant who can author ledger entries.
///
/// `email` is the **stable identity and sort key**: it disambiguates authors
/// when two entries share a [`crate::Timestamp`] during projection, and is the
/// natural partition key for the author-partitioned git-refs substrate.
/// `display_name` is presentation-only and may change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Member {
    /// Human-readable name; presentation-only, may change over time.
    pub display_name: String,
    /// Stable identity used for ordering and ref partitioning.
    pub email: String,
    /// Human or agent.
    pub kind: MemberKind,
}

impl Member {
    /// Construct a human member.
    #[must_use]
    pub fn human(display_name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            email: email.into(),
            kind: MemberKind::Human,
        }
    }

    /// Construct an agent member.
    #[must_use]
    pub fn agent(display_name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            email: email.into(),
            kind: MemberKind::Agent,
        }
    }
}
