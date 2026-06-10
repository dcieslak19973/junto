//! Opaque identifiers for kernel nouns.
//!
//! These are UUID **newtypes** — distinct types so an [`EntryId`] can never be
//! passed where a [`ChannelId`] is expected (make illegal states
//! unrepresentable). The identifiers are deliberately *opaque*: the durable
//! git-refs substrate will content-address entries (git objects already are),
//! so canonical-serialization-derived IDs are deferred to that layer. Until
//! then a random v4 UUID is a sufficient unique handle.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifies a single [`crate::LedgerEntry`] within a channel's Ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntryId(Uuid);

impl EntryId {
    /// Mint a fresh, random identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EntryId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for EntryId {
    type Err = crate::Error;

    /// Parse the `Display` form back into an id — how a surface (e.g. the MCP
    /// tools) turns a user-supplied entry reference into a typed target.
    fn from_str(s: &str) -> crate::Result<Self> {
        Uuid::parse_str(s)
            .map(Self)
            .map_err(|e| crate::Error::Invariant(format!("malformed entry id '{s}': {e}")))
    }
}

/// Identifies a Channel — one unit of inquiry, owning one Ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelId(Uuid);

impl ChannelId {
    /// Mint a fresh, random identifier — the channel's globally unique
    /// identity for life, minted when the channel is opened (`docs/adr/0014`;
    /// the name is a label bound by the genesis entry, not identity).
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ChannelId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::str::FromStr for ChannelId {
    type Err = crate::Error;

    /// Parse the `Display` form back into an id — how a substrate turns a
    /// `refs/junto/<channel-id>/...` path segment, or a surface a
    /// user-supplied id, back into a typed [`ChannelId`].
    fn from_str(s: &str) -> crate::Result<Self> {
        Uuid::parse_str(s)
            .map(Self)
            .map_err(|e| crate::Error::Invariant(format!("malformed channel id '{s}': {e}")))
    }
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        assert_ne!(EntryId::new(), EntryId::new());
        assert_ne!(ChannelId::new(), ChannelId::new());
    }
}
