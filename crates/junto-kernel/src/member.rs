//! Members — the authors of ledger entries.
//!
//! A [`Member`] is a participant in a Channel: a **human or an agent**. Agents
//! are first-class authors (`docs/adr/0004`) — the kernel does *not*
//! restrict who may write a [`crate::LedgerEntry`]. Any policy that some action
//! requires a human, or an eval-gated agent, lives at the Gate/Verifier layer,
//! not in authorship.

use serde::{Deserialize, Serialize};

use crate::sign::PublicKey;

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
    /// The member's Ed25519 verifying key (`docs/adr/0033`). Carried on the
    /// membership-granting entries (the genesis author, `MemberAdded`), where
    /// the party projection reads it as the channel keyring. Optional — a
    /// keyless member's entries simply project as `unverified`. Omitted from
    /// the canonical bytes when absent, so pre-0033 entries are unchanged.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub public_key: Option<PublicKey>,
}

impl Member {
    /// Construct a human member.
    #[must_use]
    pub fn human(display_name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            email: email.into(),
            kind: MemberKind::Human,
            public_key: None,
        }
    }

    /// Construct an agent member.
    #[must_use]
    pub fn agent(display_name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            email: email.into(),
            kind: MemberKind::Agent,
            public_key: None,
        }
    }

    /// This member with their verifying key attached — used on the
    /// membership-granting entries that feed the keyring (`docs/adr/0033`).
    #[must_use]
    pub fn with_key(mut self, key: PublicKey) -> Self {
        self.public_key = Some(key);
        self
    }
}
