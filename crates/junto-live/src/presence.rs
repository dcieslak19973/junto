//! [`Presence`] — ephemeral membership: who is watching this live session right now.
//!
//! Presence is deliberately separate from [`crate::LiveDoc`]. The document is a
//! durable CRDT snapshot of a live session's conversation and annotations — it
//! gets archived when the session ends. Watchers come and go: they join, watch
//! for a time, and leave. That transient liveness has no place in a permanent
//! record.
//!
//! [`Presence`] wraps loro's [`EphemeralStore`](loro::awareness::EphemeralStore), a
//! last-write-wins keyed store with automatic timeout-based expiry. A watcher
//! sends a heartbeat every 10 seconds; if 30 seconds pass without a heartbeat,
//! that watcher is assumed gone and is forgotten. The store syncs on its own
//! channel, entirely separate from document updates.

use loro::awareness::EphemeralStore;

/// Ephemeral membership: tracks which emails are currently watching this live
/// session.
///
/// loro's [`EphemeralStore`] is confined to this module's private field; `Presence`
/// exposes only `String` and `Vec<u8>` in its public API.
#[derive(Debug)]
pub struct Presence {
    store: EphemeralStore,
}

impl Presence {
    /// Start a new, empty presence store with a 30-second timeout.
    ///
    /// Watchers expire and are forgotten if 30 seconds pass without a
    /// heartbeat. See the module docs.
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: EphemeralStore::new(30_000),
        }
    }

    #[cfg(test)]
    fn with_timeout(ms: i64) -> Self {
        Self {
            store: EphemeralStore::new(ms),
        }
    }

    /// Mark `email` as watching this session (heartbeat).
    ///
    /// Each call resets the expiry timer for this watcher. The value stored
    /// is a fixed `true` — the key (email) is all that matters.
    pub fn set_watching(&self, email: &str) {
        self.store.set(email, true);
    }

    /// Return a sorted list of emails currently watching, excluding expired
    /// entries.
    ///
    /// Expired entries are only purged when this method is called (they do
    /// not expire automatically), so this method calls `remove_outdated()`
    /// first to clean up.
    #[must_use]
    pub fn watchers(&self) -> Vec<String> {
        self.store.remove_outdated();
        let mut watchers: Vec<String> = self
            .store
            .get_all_states()
            .keys()
            .map(|s| s.to_string())
            .collect();
        watchers.sort();
        watchers
    }

    /// Serialize all non-expired presence state to bytes.
    ///
    /// The bytes include the timestamp of each entry; the receiving replica will
    /// respect those timestamps and discard entries that have already expired.
    #[must_use]
    pub fn encode_all(&self) -> Vec<u8> {
        self.store.encode_all()
    }

    /// Merge presence state from a remote replica.
    ///
    /// Applies serialized state from another replica (see [`Presence::encode_all`]).
    /// Last-write-wins: if both replicas set the same email, the one with the
    /// later timestamp wins.
    ///
    /// # Errors
    /// Returns `Err` if the input is malformed (not valid loro ephemeral-store
    /// bytes). The error message is a `String` (loro's `Box<str>` is mapped to
    /// `String` to keep loro types out of this crate's public API).
    pub fn apply(&self, data: &[u8]) -> Result<(), String> {
        self.store.apply(data).map_err(|e| e.to_string())
    }
}

impl Default for Presence {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_merges_between_stores() {
        let a = Presence::new();
        let b = Presence::new();
        a.set_watching("dan@x.com");
        b.apply(&a.encode_all()).unwrap();
        assert_eq!(b.watchers(), vec!["dan@x.com".to_string()]);
    }

    #[test]
    fn watchers_sorted_and_deduped() {
        let a = Presence::new();
        a.set_watching("z@x.com");
        a.set_watching("a@x.com");
        a.set_watching("z@x.com");
        assert_eq!(
            a.watchers(),
            vec!["a@x.com".to_string(), "z@x.com".to_string()]
        );
    }

    #[test]
    fn entries_expire_after_timeout() {
        let a = Presence::with_timeout(50);
        a.set_watching("temp@x.com");
        assert_eq!(a.watchers(), vec!["temp@x.com".to_string()]);

        // Sleep past timeout: 50ms timeout + 200ms sleep = well past expiry
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(a.watchers(), vec![] as Vec<String>);
    }
}
