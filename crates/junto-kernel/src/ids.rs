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

/// The fixed UUIDv5 namespace for [`ChannelId::from_name`]. **Load-bearing:**
/// changing this value changes the id of every named channel ever derived, so
/// it must never change.
const CHANNEL_NAME_NAMESPACE: Uuid = uuid::uuid!("6a1727b8-9c4e-5d0f-8b3a-2e7d94c1f05b");

impl ChannelId {
    /// Mint a fresh, random identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Derive the id of a **named** channel (UUIDv5 over a fixed namespace).
    ///
    /// Deterministic: the same name yields the same id on every machine, so
    /// people and agents can address a channel by a human name ("junto-dev")
    /// with no registry to sync. This is the dogfood-era convention for
    /// channel identity; a modelled Channel noun may later carry richer
    /// metadata, but ids derived here must remain stable.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        Self(Uuid::new_v5(&CHANNEL_NAME_NAMESPACE, name.as_bytes()))
    }
}

impl Default for ChannelId {
    fn default() -> Self {
        Self::new()
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

    #[test]
    fn named_channels_are_deterministic_and_distinct() {
        // Same name → same id (on any machine, any day); different names differ.
        assert_eq!(
            ChannelId::from_name("junto-dev"),
            ChannelId::from_name("junto-dev")
        );
        assert_ne!(
            ChannelId::from_name("junto-dev"),
            ChannelId::from_name("junto-design")
        );
        // Pin the derivation itself: a changed namespace or scheme would orphan
        // every named channel ever synced, so the exact id is part of the
        // contract (see CHANNEL_NAME_NAMESPACE).
        assert_eq!(
            ChannelId::from_name("junto-dev").to_string(),
            "c441363c-a731-51a1-a9e6-a1f10b89b522"
        );
    }
}
