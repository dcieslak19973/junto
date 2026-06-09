//! Wall-clock timestamps for ledger ordering.
//!
//! [`Timestamp`] is a newtype over epoch-milliseconds. It is the **primary
//! sort key** when projecting a Ledger (ties broken by author identity — see
//! [`crate::Ledger::project`]), which is why it derives `Ord`.
//!
//! Wall-clock is deliberate for this slice: it is simple and good enough for an
//! in-memory backend. A logical/Lamport clock (the git-bug approach) is the
//! likely successor once concurrent cross-machine writes over the git-refs
//! substrate make wall-clock skew a real ordering hazard.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A point in time as milliseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The current wall-clock time.
    ///
    /// Clamps to `0` for the (unreachable in practice) pre-1970 case rather
    /// than panicking — the kernel never `unwrap`s.
    #[must_use]
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Self(millis)
    }

    /// Construct a timestamp from an explicit epoch-millis value (mainly for
    /// deterministic tests).
    #[must_use]
    pub fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// The underlying epoch-millis value.
    #[must_use]
    pub fn as_millis(self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_by_millis() {
        assert!(Timestamp::from_millis(1) < Timestamp::from_millis(2));
    }
}
