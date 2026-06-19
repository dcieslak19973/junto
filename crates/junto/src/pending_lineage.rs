//! The machine-local **pending-lineage queue** (`docs/adr/0028`).
//!
//! A lineage edge (`docs/adr/0027`) is two entries, one in each endpoint's
//! ledger. The *near* side always writes; the *far* side is attempted and, on
//! any failure (the far channel not registered here, not yet synced, the
//! author's membership not yet visible, transient IO), parked here to be
//! reconciled later. Each parked item is the **fully-formed far-side
//! `LedgerEntry`** — its canonical bytes and id are already fixed, so a retry
//! that lands writes the *identical* entry and the ledger's content-addressed
//! dedup (`docs/adr/0010`) makes re-attempts harmless.
//!
//! Stored as **NDJSON** — one entry's canonical bytes (`docs/adr/0008`) per
//! line — beside `members.toml` / `substrates.toml`. This is operational state,
//! never ledger content: it does not sync, and losing it loses only *pending*
//! far-side writes (every near side is already durable in its ledger).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use junto_kernel::LedgerEntry;

/// The queue file under a junto home.
fn queue_path(home: &Path) -> PathBuf {
    home.join("pending-lineage.ndjson")
}

/// Every parked far-side entry, in file order. A missing file is an empty
/// queue, not an error.
pub fn pending(home: &Path) -> Result<Vec<LedgerEntry>> {
    let path = queue_path(home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            LedgerEntry::from_canonical_bytes(line.as_bytes())
                .with_context(|| format!("parsing a pending-lineage entry in {}", path.display()))
        })
        .collect()
}

/// Park a far-side entry for later reconciliation — idempotent by entry id, so
/// re-issuing the same edge does not double-queue it.
pub fn enqueue(home: &Path, entry: &LedgerEntry) -> Result<()> {
    let mut queue = pending(home)?;
    if queue.iter().any(|parked| parked.id == entry.id) {
        return Ok(());
    }
    queue.push(entry.clone());
    rewrite(home, &queue)
}

/// Replace the queue with `entries` (used by reconciliation to drop the ones
/// that landed or expired). An empty queue removes the file.
pub fn rewrite(home: &Path, entries: &[LedgerEntry]) -> Result<()> {
    let path = queue_path(home);
    if entries.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        return Ok(());
    }
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    let mut body = String::new();
    for entry in entries {
        let bytes = entry
            .to_canonical_bytes()
            .context("serializing a pending-lineage entry")?;
        body.push_str(&String::from_utf8(bytes).context("canonical bytes are not utf-8")?);
        body.push('\n');
    }
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use junto_kernel::{ChannelId, EntryId, EntryPayload, Member, Timestamp};

    fn far_entry(channel: ChannelId) -> LedgerEntry {
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author: Member::human("Dan", "dan@example.com"),
            timestamp: Timestamp::now(),
            payload: EntryPayload::ConvergenceReceived {
                source: ChannelId::new(),
            },
        }
    }

    #[test]
    fn missing_file_is_an_empty_queue() {
        let home = tempfile::tempdir().unwrap();
        assert!(pending(home.path()).unwrap().is_empty());
    }

    #[test]
    fn enqueue_then_load_round_trips() {
        let home = tempfile::tempdir().unwrap();
        let a = far_entry(ChannelId::new());
        let b = far_entry(ChannelId::new());
        enqueue(home.path(), &a).unwrap();
        enqueue(home.path(), &b).unwrap();

        let loaded = pending(home.path()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], a);
        assert_eq!(loaded[1], b);
    }

    #[test]
    fn enqueue_is_idempotent_by_id() {
        let home = tempfile::tempdir().unwrap();
        let a = far_entry(ChannelId::new());
        enqueue(home.path(), &a).unwrap();
        enqueue(home.path(), &a).unwrap();
        assert_eq!(pending(home.path()).unwrap().len(), 1);
    }

    #[test]
    fn rewrite_empty_removes_the_file() {
        let home = tempfile::tempdir().unwrap();
        let a = far_entry(ChannelId::new());
        enqueue(home.path(), &a).unwrap();
        rewrite(home.path(), &[]).unwrap();
        assert!(pending(home.path()).unwrap().is_empty());
        assert!(!queue_path(home.path()).exists());
    }
}
