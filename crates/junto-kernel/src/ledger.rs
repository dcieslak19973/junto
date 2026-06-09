//! The Ledger — append plus projection over a [`SubstrateProvider`].
//!
//! The substrate stores entries dumbly; the [`Ledger`] supplies the *meaning*:
//! it orders the log and folds it into a [`ChannelView`] of current standings.
//! This is the event-sourcing projection behind decision #8 — state is never
//! stored on an entry, it is **derived** by replaying the immutable log.
//!
//! Immutability is structural: the only mutating call is [`Ledger::append`];
//! there is no edit or delete anywhere in the API. Corrections are new entries.

use std::collections::HashMap;

use crate::{EntryId, EntryPayload, LedgerEntry, Result, SubstrateProvider, ids::ChannelId};

/// The derived standing of an [`EntryPayload::Assertion`] after folding the log.
///
/// Only assertions have a standing; verification entries
/// (ratify / park / correct) are the *cause* of standing changes, not subjects
/// of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Asserted, not yet acted upon.
    Provisional,
    /// Accepted by a [`EntryPayload::Ratification`].
    Ratified,
    /// Set aside by a [`EntryPayload::Park`] (negative/abandoned result).
    Parked,
    /// Superseded by a [`EntryPayload::Correction`].
    Superseded,
}

/// A point-in-time projection of a Channel's Ledger: the entries in canonical
/// order, plus the current [`Standing`] of each assertion.
#[derive(Debug, Clone)]
pub struct ChannelView {
    /// All entries, in canonical `(timestamp, author.email)` order.
    pub entries: Vec<LedgerEntry>,
    /// Current standing per assertion [`EntryId`].
    pub standings: HashMap<EntryId, Standing>,
}

impl ChannelView {
    /// The standing of a specific assertion, if present.
    #[must_use]
    pub fn standing(&self, id: &EntryId) -> Option<Standing> {
        self.standings.get(id).copied()
    }
}

/// Domain-level access to a Channel's record, layered over a storage backend.
///
/// Generic over the [`SubstrateProvider`] so the same logic runs over the
/// in-memory backend and, later, git-refs (static dispatch; `dyn`-safety is
/// deferred along with the async-trait `Send` question).
#[derive(Debug)]
pub struct Ledger<S: SubstrateProvider> {
    substrate: S,
}

impl<S: SubstrateProvider> Ledger<S> {
    /// Wrap a substrate.
    pub fn new(substrate: S) -> Self {
        Self { substrate }
    }

    /// Append one immutable entry. The sole mutating operation.
    ///
    /// # Errors
    /// Propagates any error from the underlying [`SubstrateProvider`].
    pub async fn append(&mut self, entry: LedgerEntry) -> Result<()> {
        self.substrate.append(entry).await
    }

    /// Project the Channel's log into a [`ChannelView`].
    ///
    /// Entries are sorted by `(timestamp, author.email)` — a deterministic
    /// total order even when wall-clocks collide across authors — then folded:
    /// each assertion starts [`Standing::Provisional`]; a later verification
    /// entry moves its target's standing (ratify → [`Standing::Ratified`],
    /// park → [`Standing::Parked`], correction → [`Standing::Superseded`]).
    /// The last applicable verification in order wins. A verification whose
    /// `target` is unknown is ignored leniently (dangling references are
    /// tolerated for now).
    ///
    /// # Errors
    /// Propagates any error from the underlying [`SubstrateProvider`].
    pub async fn project(&self, channel: &ChannelId) -> Result<ChannelView> {
        let mut entries = self.substrate.entries(channel).await?;
        entries.sort_by(|a, b| {
            a.timestamp
                .cmp(&b.timestamp)
                .then_with(|| a.author.email.cmp(&b.author.email))
        });

        let mut standings: HashMap<EntryId, Standing> = HashMap::new();

        // First pass: every assertion exists, provisionally.
        for entry in &entries {
            if matches!(entry.payload, EntryPayload::Assertion { .. }) {
                standings.insert(entry.id, Standing::Provisional);
            }
        }

        // Second pass: apply verification acts in canonical order. A dangling
        // target (no such assertion) is skipped rather than erroring.
        for entry in &entries {
            let new_standing = match &entry.payload {
                EntryPayload::Assertion { .. } => continue,
                EntryPayload::Ratification { .. } => Standing::Ratified,
                EntryPayload::Park { .. } => Standing::Parked,
                EntryPayload::Correction { .. } => Standing::Superseded,
            };
            if let Some(target) = entry.payload.target()
                && let Some(slot) = standings.get_mut(&target)
            {
                *slot = new_standing;
            }
        }

        Ok(ChannelView { entries, standings })
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        EntryId, EntryPayload, InMemorySubstrate, Ledger, LedgerEntry, Member, Standing, Timestamp,
        ids::ChannelId,
    };

    /// Build an entry with explicit id/timestamp/author for deterministic tests.
    fn entry(
        id: EntryId,
        channel: ChannelId,
        author: Member,
        millis: i64,
        payload: EntryPayload,
    ) -> LedgerEntry {
        LedgerEntry {
            id,
            channel,
            author,
            timestamp: Timestamp::from_millis(millis),
            payload,
        }
    }

    fn assertion(statement: &str) -> EntryPayload {
        EntryPayload::Assertion {
            statement: statement.into(),
            rationale: "because".into(),
            provenance: Vec::new(),
        }
    }

    #[tokio::test]
    async fn assertion_is_provisional() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let id = EntryId::new();
        ledger
            .append(entry(id, channel, alice, 1, assertion("the sky is blue")))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.standing(&id), Some(Standing::Provisional));
    }

    #[tokio::test]
    async fn ratification_marks_ratified() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let claim = EntryId::new();
        ledger
            .append(entry(
                claim,
                channel,
                alice.clone(),
                1,
                assertion("x holds"),
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                alice,
                2,
                EntryPayload::Ratification {
                    target: claim,
                    rationale: "verified".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.standing(&claim), Some(Standing::Ratified));
    }

    #[tokio::test]
    async fn park_marks_parked_and_is_retained() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let agent = Member::agent("Researcher", "agent@junto.local");
        let claim = EntryId::new();
        ledger
            .append(entry(claim, channel, agent.clone(), 1, assertion("h1")))
            .await
            .unwrap();
        let park_id = EntryId::new();
        ledger
            .append(entry(
                park_id,
                channel,
                agent,
                2,
                EntryPayload::Park {
                    target: claim,
                    rationale: "disproven".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.standing(&claim), Some(Standing::Parked));
        // The negative result is kept in the log, not deleted (#13).
        assert!(view.entries.iter().any(|e| e.id == park_id));
    }

    #[tokio::test]
    async fn correction_supersedes_original_which_remains() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let original = EntryId::new();
        ledger
            .append(entry(
                original,
                channel,
                alice.clone(),
                1,
                assertion("2+2=5"),
            ))
            .await
            .unwrap();
        let correction = EntryId::new();
        ledger
            .append(entry(
                correction,
                channel,
                alice,
                2,
                EntryPayload::Correction {
                    target: original,
                    statement: "2+2=4".into(),
                    rationale: "arithmetic".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.standing(&original), Some(Standing::Superseded));
        // Original entry is untouched in the log; correction is its own entry.
        assert!(view.entries.iter().any(|e| e.id == original));
        assert!(view.entries.iter().any(|e| e.id == correction));
    }

    #[tokio::test]
    async fn projection_orders_by_timestamp_then_author() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let bob = Member::human("Bob", "bob@example.com");

        // Append out of order; equal-timestamp pair must tiebreak on email.
        let a_late = EntryId::new();
        let b_early = EntryId::new();
        let a_tie = EntryId::new();
        let b_tie = EntryId::new();
        ledger
            .append(entry(a_late, channel, alice.clone(), 10, assertion("a@10")))
            .await
            .unwrap();
        ledger
            .append(entry(b_early, channel, bob.clone(), 5, assertion("b@5")))
            .await
            .unwrap();
        // Same timestamp 7: alice@ sorts before bob@.
        ledger
            .append(entry(b_tie, channel, bob, 7, assertion("b@7")))
            .await
            .unwrap();
        ledger
            .append(entry(a_tie, channel, alice, 7, assertion("a@7")))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        let order: Vec<EntryId> = view.entries.iter().map(|e| e.id).collect();
        assert_eq!(order, vec![b_early, a_tie, b_tie, a_late]);
    }

    #[tokio::test]
    async fn last_applicable_verification_wins() {
        // Two verifications on the same claim: the later one (by canonical
        // order) decides the standing. Park@2 then Ratification@3 → Ratified.
        // Pins the override semantics flagged "refine later" in the plan so a
        // future projection refactor can't silently change them.
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let claim = EntryId::new();
        ledger
            .append(entry(claim, channel, alice.clone(), 1, assertion("h")))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                alice.clone(),
                2,
                EntryPayload::Park {
                    target: claim,
                    rationale: "set aside".into(),
                },
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                alice,
                3,
                EntryPayload::Ratification {
                    target: claim,
                    rationale: "revived".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.standing(&claim), Some(Standing::Ratified));
    }

    #[tokio::test]
    async fn channels_are_scoped() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel_a = ChannelId::new();
        let channel_b = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let in_a = EntryId::new();
        ledger
            .append(entry(
                in_a,
                channel_a,
                alice.clone(),
                1,
                assertion("only in A"),
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel_b,
                alice,
                1,
                assertion("only in B"),
            ))
            .await
            .unwrap();

        let view_a = ledger.project(&channel_a).await.unwrap();
        assert_eq!(view_a.entries.len(), 1);
        assert_eq!(view_a.entries[0].id, in_a);

        let view_b = ledger.project(&channel_b).await.unwrap();
        assert_eq!(view_b.entries.len(), 1);
        assert_ne!(view_b.entries[0].id, in_a);
    }
}
