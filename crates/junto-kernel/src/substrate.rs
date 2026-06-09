//! The substrate seam — dumb, append-only entry storage.
//!
//! A [`SubstrateProvider`] is the boundary between the kernel's domain logic
//! and *where entries physically live*. It does the minimum: append an entry,
//! and hand back **the complete set of entries** for a channel. It does **no**
//! folding or interpretation — that is the [`crate::Ledger`]'s job. Keeping the
//! substrate dumb is what lets the same domain logic run over the in-memory
//! backend (here) and, later, the git-refs backend unchanged.
//!
//! **Ordering is not part of the contract.** A backend returns entries in
//! whatever order is natural for it; [`crate::Ledger::project`] imposes the
//! canonical `(timestamp, author.email)` order. (The git-refs backend
//! partitions storage by author, so it cannot cheaply reconstruct a global
//! append order — and does not need to.)
//!
//! The trait is `async` because the durable backend will do real I/O
//! (`git push`/`fetch`). [`InMemorySubstrate`] does none, but implements the
//! same async contract so tests exercise the real shape.

use std::collections::HashMap;

use crate::{LedgerEntry, Result, ids::ChannelId};

/// Storage boundary for ledger entries, scoped by Channel.
///
/// Implementations are *append-only*: there is no edit or delete. The
/// in-memory impl never fails, but the contract returns [`Result`] for the
/// future git-refs backend, whose pushes and fetches can.
// We accept `async fn` directly in the trait. The resulting future is not
// `Send`-bound, which is fine here: the in-memory backend is exercised on a
// single-threaded test runtime. Revisit (e.g. `#[trait_variant]` or a `Send`
// bound) if/when a multi-threaded runtime drives a real substrate.
#[allow(async_fn_in_trait)]
pub trait SubstrateProvider {
    /// Append one entry to its channel's log.
    async fn append(&mut self, entry: LedgerEntry) -> Result<()>;

    /// The complete set of entries for `channel`, in no particular order
    /// (the [`crate::Ledger`] sorts them; duplicates are tolerated — the
    /// projection deduplicates by [`crate::EntryId`]). Returns an empty vec
    /// for an unknown channel.
    async fn entries(&self, channel: &ChannelId) -> Result<Vec<LedgerEntry>>;
}

/// An in-memory [`SubstrateProvider`] backed by a per-channel append log.
///
/// This is the permanent test and local-dev backend — not a throwaway. It
/// pressure-tests the domain model before git-refs encoding and Windows
/// file-locking can complicate it.
#[derive(Debug, Default)]
pub struct InMemorySubstrate {
    by_channel: HashMap<ChannelId, Vec<LedgerEntry>>,
}

impl InMemorySubstrate {
    /// An empty substrate.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SubstrateProvider for InMemorySubstrate {
    async fn append(&mut self, entry: LedgerEntry) -> Result<()> {
        self.by_channel
            .entry(entry.channel)
            .or_default()
            .push(entry);
        Ok(())
    }

    async fn entries(&self, channel: &ChannelId) -> Result<Vec<LedgerEntry>> {
        Ok(self.by_channel.get(channel).cloned().unwrap_or_default())
    }
}
