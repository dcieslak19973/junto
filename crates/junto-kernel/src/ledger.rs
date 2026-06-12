//! The Ledger — append plus projection over a [`SubstrateProvider`].
//!
//! The substrate stores entries dumbly; the [`Ledger`] supplies the *meaning*:
//! it orders the log and folds it into a [`ChannelView`] of current standings.
//! This is the event-sourcing projection behind `docs/adr/0002` — state is never
//! stored on an entry, it is **derived** by replaying the immutable log.
//!
//! Immutability is structural: the only mutating call is [`Ledger::append`];
//! there is no edit or delete anywhere in the API. Corrections are new entries.

use std::collections::{HashMap, HashSet};

use crate::{
    EntryId, EntryPayload, GateStatus, LedgerEntry, Member, Result, SubstrateProvider,
    gate::ApprovalRequirement,
    ids::ChannelId,
    session::{SessionState, SessionView},
};

/// Whether a proposal's [`ApprovalRequirement`] is satisfied by the set of
/// distinct approver emails seen so far (rejection is handled separately, and
/// dominates). An absent approver set is treated as empty.
fn requirement_met(requirement: &ApprovalRequirement, approvers: Option<&HashSet<&str>>) -> bool {
    match requirement {
        ApprovalRequirement::Auto => true,
        ApprovalRequirement::Count(n) => approvers.map_or(0, HashSet::len) as u32 >= *n,
        ApprovalRequirement::AllOf(members) => members.iter().all(|member| {
            approvers.is_some_and(|approvers| approvers.contains(member.email.as_str()))
        }),
    }
}

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
/// order, plus the derived [`Standing`] of each assertion and
/// [`GateStatus`] of each proposal.
#[derive(Debug, Clone)]
pub struct ChannelView {
    /// The channel's human-facing name, from its `ChannelOpened` genesis entry
    /// (`docs/adr/0014`/`0016`). `None` if no genesis is present (an unopened
    /// dogfood-era channel, or a record synced before its genesis arrived).
    /// If concurrent opens left multiple geneses, the canonically first wins —
    /// deterministic on every replica, like all projection.
    pub name: Option<String>,
    /// All entries, deduplicated by id, in canonical
    /// `(timestamp, author.email, id)` order — including unrecognized ones,
    /// which are never dropped (`docs/adr/0017`).
    pub entries: Vec<LedgerEntry>,
    /// The **Party** — the channel's members (`docs/adr/0017`): the founding
    /// member (the genesis author) first, then members granted by
    /// founder-authored [`EntryPayload::MemberAdded`] entries, in grant order.
    /// Empty when the channel has no genesis (a dogfood-era channel, or a
    /// record synced before its genesis arrived) — in which case membership is
    /// not enforced and every entry projects.
    pub party: Vec<Member>,
    /// Entries whose author is not in the [`party`](ChannelView::party) —
    /// retained and surfaced, but excluded from standings and gate folding
    /// (`docs/adr/0017`: visibility beats mystery). Always empty when `party`
    /// is empty.
    pub unrecognized: HashSet<EntryId>,
    /// Current standing per assertion [`EntryId`].
    pub standings: HashMap<EntryId, Standing>,
    /// Current gate status per proposal [`EntryId`].
    pub gate_status: HashMap<EntryId, GateStatus>,
    /// Current view per Agent Session — keyed by the
    /// [`EntryPayload::SessionStarted`] entry's id, holding the folded
    /// [`SessionState`] and the session's attached artifacts.
    pub sessions: HashMap<EntryId, SessionView>,
    /// Whether the channel is closed (`docs/adr/0022`): the last applicable
    /// [`EntryPayload::ChannelClosed`] / [`EntryPayload::ChannelReopened`]
    /// wins, in canonical order, members only. The record outlives the
    /// inquiry — closed only means "out of the working set".
    pub closed: bool,
}

impl ChannelView {
    /// The standing of a specific assertion, if present.
    #[must_use]
    pub fn standing(&self, id: &EntryId) -> Option<Standing> {
        self.standings.get(id).copied()
    }

    /// The gate status of a specific proposal, if present.
    #[must_use]
    pub fn gate_status(&self, id: &EntryId) -> Option<GateStatus> {
        self.gate_status.get(id).copied()
    }

    /// The view of a specific Agent Session, if present.
    #[must_use]
    pub fn session(&self, id: &EntryId) -> Option<&SessionView> {
        self.sessions.get(id)
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

    /// Mutable access to the wrapped substrate, for backend-specific
    /// operations the generic ledger does not model (e.g. the git-refs
    /// backend's `sync`). Appends still go through [`Ledger::append`].
    pub fn substrate_mut(&mut self) -> &mut S {
        &mut self.substrate
    }

    /// Read access to the wrapped substrate — e.g. enumerating its channels
    /// for discovery ([`SubstrateProvider::channels`]).
    pub fn substrate(&self) -> &S {
        &self.substrate
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
    /// Entries are **deduplicated by [`EntryId`]** (the substrate may hold the
    /// same entry twice — a retried append, or the same author's log synced
    /// from two remotes) and sorted by `(timestamp, author.email, id)` — a
    /// deterministic total order even when wall-clocks collide, including two
    /// entries from the *same* author in the same millisecond. Determinism
    /// matters because standing is last-applicable-wins: replicas that
    /// disagreed on order would disagree on standing. The log is then folded
    /// into two derived views:
    ///
    /// - **Assertion [`Standing`]:** each assertion starts
    ///   [`Standing::Provisional`]; a later verification moves its target
    ///   (ratify → [`Standing::Ratified`], park → [`Standing::Parked`],
    ///   correction → [`Standing::Superseded`]); the last applicable one wins.
    /// - **Proposal [`GateStatus`]:** each proposal starts [`GateStatus::Pending`]
    ///   ([`GateStatus::Approved`] immediately if its requirement is
    ///   [`ApprovalRequirement::Auto`]); approvals accumulate by *distinct
    ///   author email* and a rejection is *sticky*. Any rejection ⇒
    ///   [`GateStatus::Rejected`]; otherwise the requirement decides
    ///   approved-vs-pending.
    ///
    /// An act whose `target` is unknown is ignored leniently (dangling
    /// references are tolerated for now).
    ///
    /// # Errors
    /// Propagates any error from the underlying [`SubstrateProvider`].
    pub async fn project(&self, channel: &ChannelId) -> Result<ChannelView> {
        let mut entries = self.substrate.entries(channel).await?;
        entries.sort_by(LedgerEntry::canonical_cmp);
        // Keep the first occurrence of each id (in canonical order), so a
        // double-appended entry projects as one.
        let mut seen = HashSet::new();
        entries.retain(|entry| seen.insert(entry.id));

        let party = Self::project_party(&entries);
        // The membership check is set-based, not temporal (`docs/adr/0017`):
        // an entry counts iff its author is in the Party, wherever the grant
        // falls in canonical order. No genesis ⇒ no Party ⇒ no enforcement.
        let unrecognized: HashSet<EntryId> = if party.is_empty() {
            HashSet::new()
        } else {
            let member_emails: HashSet<&str> =
                party.iter().map(|member| member.email.as_str()).collect();
            entries
                .iter()
                .filter(|entry| !member_emails.contains(entry.author.email.as_str()))
                .map(|entry| entry.id)
                .collect()
        };
        let recognized: Vec<&LedgerEntry> = entries
            .iter()
            .filter(|entry| !unrecognized.contains(&entry.id))
            .collect();

        let standings = Self::project_standings(&recognized);
        let gate_status = Self::project_gates(&recognized);
        let sessions = Self::project_sessions(&recognized);
        // The current name: the (canonically first) genesis binding, unless a
        // later Correction targeting the genesis superseded it — rename is a
        // corrective entry, not mutable metadata (docs/adr/0014/0016).
        let genesis = recognized.iter().find_map(|entry| match &entry.payload {
            EntryPayload::ChannelOpened { name } => Some((entry.id, name.clone())),
            _ => None,
        });
        let name = genesis.map(|(genesis_id, mut name)| {
            for entry in &recognized {
                if let EntryPayload::Correction {
                    target, statement, ..
                } = &entry.payload
                    && *target == genesis_id
                {
                    name = statement.clone();
                }
            }
            name
        });
        // Closed: the last applicable lifecycle act wins (docs/adr/0022).
        let closed = recognized
            .iter()
            .rev()
            .find_map(|entry| match &entry.payload {
                EntryPayload::ChannelClosed { .. } => Some(true),
                EntryPayload::ChannelReopened { .. } => Some(false),
                _ => None,
            })
            .unwrap_or(false);

        Ok(ChannelView {
            name,
            entries,
            party,
            unrecognized,
            standings,
            gate_status,
            sessions,
            closed,
        })
    }

    /// Fold the **Party** out of an ordered entry list (`docs/adr/0017`): the
    /// genesis author is the founding member; founder-authored
    /// [`EntryPayload::MemberAdded`] entries extend the roster (re-adding an
    /// existing member is a no-op). A `MemberAdded` from anyone else has no
    /// roster effect. With concurrent geneses the canonically first wins,
    /// consistent with the name projection. No genesis ⇒ empty Party.
    fn project_party(entries: &[LedgerEntry]) -> Vec<Member> {
        let Some(founder) = entries.iter().find_map(|entry| {
            matches!(entry.payload, EntryPayload::ChannelOpened { .. })
                .then(|| entry.author.clone())
        }) else {
            return Vec::new();
        };

        let mut emails: HashSet<String> = HashSet::from([founder.email.clone()]);
        let mut party = vec![founder.clone()];
        for entry in entries {
            if let EntryPayload::MemberAdded { member } = &entry.payload
                && entry.author.email == founder.email
                && emails.insert(member.email.clone())
            {
                party.push(member.clone());
            }
        }
        party
    }

    /// Fold the assertion standings out of an ordered list of *recognized*
    /// entries (refs, because projection filters the canonical list by
    /// membership without cloning).
    fn project_standings(entries: &[&LedgerEntry]) -> HashMap<EntryId, Standing> {
        let mut standings: HashMap<EntryId, Standing> = HashMap::new();

        // Every assertion exists, provisionally.
        for entry in entries {
            if matches!(entry.payload, EntryPayload::Assertion { .. }) {
                standings.insert(entry.id, Standing::Provisional);
            }
        }

        // Apply verification acts in canonical order; a dangling target (no
        // such assertion) is skipped rather than erroring.
        for entry in entries {
            let new_standing = match &entry.payload {
                EntryPayload::Ratification { .. } => Standing::Ratified,
                EntryPayload::Park { .. } => Standing::Parked,
                EntryPayload::Correction { .. } => Standing::Superseded,
                // Not standing-bearing acts.
                EntryPayload::ChannelOpened { .. }
                | EntryPayload::MemberAdded { .. }
                | EntryPayload::ChannelClosed { .. }
                | EntryPayload::ChannelReopened { .. }
                | EntryPayload::Assertion { .. }
                | EntryPayload::Proposal { .. }
                | EntryPayload::Approval { .. }
                | EntryPayload::Rejection { .. }
                | EntryPayload::SessionStarted { .. }
                | EntryPayload::SessionUpdated { .. }
                | EntryPayload::ArtifactAttached { .. } => continue,
            };
            if let Some(target) = entry.payload.target()
                && let Some(slot) = standings.get_mut(&target)
            {
                *slot = new_standing;
            }
        }

        standings
    }

    /// Fold the proposal gate statuses out of an ordered list of *recognized*
    /// entries — so only members' approvals and rejections count
    /// (`docs/adr/0017`).
    fn project_gates(entries: &[&LedgerEntry]) -> HashMap<EntryId, GateStatus> {
        // Per proposal: its requirement, the distinct emails that approved it,
        // and whether it has been rejected.
        let mut requirements: HashMap<EntryId, &ApprovalRequirement> = HashMap::new();
        let mut approvers: HashMap<EntryId, HashSet<&str>> = HashMap::new();
        let mut rejected: HashSet<EntryId> = HashSet::new();

        for entry in entries {
            if let EntryPayload::Proposal { requirement, .. } = &entry.payload {
                requirements.insert(entry.id, requirement);
                approvers.entry(entry.id).or_default();
            }
        }

        // Accumulate approvals (by distinct author email) and rejections,
        // ignoring acts whose target is not a known proposal.
        for entry in entries {
            match &entry.payload {
                EntryPayload::Approval { target, .. } if requirements.contains_key(target) => {
                    approvers
                        .entry(*target)
                        .or_default()
                        .insert(entry.author.email.as_str());
                }
                EntryPayload::Rejection { target, .. } if requirements.contains_key(target) => {
                    rejected.insert(*target);
                }
                _ => {}
            }
        }

        requirements
            .into_iter()
            .map(|(id, requirement)| {
                let status = if rejected.contains(&id) {
                    GateStatus::Rejected
                } else if requirement_met(requirement, approvers.get(&id)) {
                    GateStatus::Approved
                } else {
                    GateStatus::Pending
                };
                (id, status)
            })
            .collect()
    }

    /// Fold the Agent Session views out of an ordered list of *recognized*
    /// entries. Each [`EntryPayload::SessionStarted`] begins a session in
    /// [`SessionState::Working`]; [`EntryPayload::SessionUpdated`] moves its
    /// state (last-applicable-wins, like standings); each
    /// [`EntryPayload::ArtifactAttached`] appends its own entry id to the
    /// session's artifact list, in canonical order. Acts targeting an unknown
    /// session are skipped leniently, like every other dangling reference.
    fn project_sessions(entries: &[&LedgerEntry]) -> HashMap<EntryId, SessionView> {
        let mut sessions: HashMap<EntryId, SessionView> = HashMap::new();

        for entry in entries {
            if matches!(entry.payload, EntryPayload::SessionStarted { .. }) {
                sessions.insert(
                    entry.id,
                    SessionView {
                        state: SessionState::Working,
                        artifacts: Vec::new(),
                    },
                );
            }
        }

        for entry in entries {
            match &entry.payload {
                EntryPayload::SessionUpdated { target, state, .. } => {
                    if let Some(session) = sessions.get_mut(target) {
                        session.state = *state;
                    }
                }
                EntryPayload::ArtifactAttached { target, .. } => {
                    if let Some(session) = sessions.get_mut(target) {
                        session.artifacts.push(entry.id);
                    }
                }
                _ => {}
            }
        }

        sessions
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ApprovalRequirement, EntryId, EntryPayload, GateStatus, InMemorySubstrate, Ledger,
        LedgerEntry, Member, SessionState, Standing, Timestamp, ids::ChannelId,
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
            frame: None,
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
    async fn same_author_same_millisecond_orders_by_id() {
        // Two verification acts by one author in the same millisecond: the
        // entry id is the final tie-break, so every replica projects the same
        // standing regardless of the order the substrate returned them in.
        let (lo, hi) = {
            let (a, b) = (EntryId::new(), EntryId::new());
            if a < b { (a, b) } else { (b, a) }
        };
        let alice = Member::human("Alice", "alice@example.com");
        let claim = EntryId::new();
        let park = |channel| {
            entry(
                hi, // the larger id: applies last in canonical order, so it wins
                channel,
                alice.clone(),
                2,
                EntryPayload::Park {
                    target: claim,
                    rationale: "set aside".into(),
                },
            )
        };
        let ratify = |channel| {
            entry(
                lo,
                channel,
                alice.clone(),
                2,
                EntryPayload::Ratification {
                    target: claim,
                    rationale: "confirmed".into(),
                },
            )
        };

        // Append in both orders; the projection must agree.
        for flipped in [false, true] {
            let mut ledger = Ledger::new(InMemorySubstrate::new());
            let channel = ChannelId::new();
            ledger
                .append(entry(claim, channel, alice.clone(), 1, assertion("h")))
                .await
                .unwrap();
            let (first, second) = if flipped {
                (ratify(channel), park(channel))
            } else {
                (park(channel), ratify(channel))
            };
            ledger.append(first).await.unwrap();
            ledger.append(second).await.unwrap();

            let view = ledger.project(&channel).await.unwrap();
            assert_eq!(
                view.standing(&claim),
                Some(Standing::Parked),
                "standing must not depend on substrate return order (flipped={flipped})"
            );
        }
    }

    #[tokio::test]
    async fn duplicate_appends_project_once() {
        // The same entry appended twice (a retried append, or a future sync
        // unioning overlapping logs) must project as one entry.
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let claim = entry(EntryId::new(), channel, alice, 1, assertion("once"));
        ledger.append(claim.clone()).await.unwrap();
        ledger.append(claim.clone()).await.unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.standing(&claim.id), Some(Standing::Provisional));
    }

    #[tokio::test]
    async fn cross_kind_acts_are_ignored() {
        // A Ratification targeting a Proposal, and an Approval targeting an
        // Assertion, both act on the wrong kind: neither moves anything.
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let claim = EntryId::new();
        let prop = EntryId::new();
        ledger
            .append(entry(claim, channel, alice.clone(), 1, assertion("h")))
            .await
            .unwrap();
        ledger
            .append(entry(
                prop,
                channel,
                alice.clone(),
                2,
                proposal(ApprovalRequirement::Count(1)),
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                alice.clone(),
                3,
                EntryPayload::Ratification {
                    target: prop, // wrong kind: proposals have no Standing
                    rationale: "misdirected".into(),
                },
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                alice,
                4,
                EntryPayload::Approval {
                    target: claim, // wrong kind: assertions have no GateStatus
                    rationale: "misdirected".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.standing(&claim), Some(Standing::Provisional));
        assert_eq!(view.gate_status(&prop), Some(GateStatus::Pending));
        assert!(view.standing(&prop).is_none());
        assert!(view.gate_status(&claim).is_none());
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

    #[tokio::test]
    async fn genesis_yields_channel_name_and_bears_no_standing() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let genesis = EntryId::new();
        ledger
            .append(entry(
                genesis,
                channel,
                alice,
                1,
                EntryPayload::ChannelOpened {
                    name: "slice-8".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.name.as_deref(), Some("slice-8"));
        // A lifecycle act is not an assertion: it carries no standing.
        assert_eq!(view.standing(&genesis), None);
    }

    #[tokio::test]
    async fn unopened_channel_has_no_name() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                alice,
                1,
                assertion("recorded before any genesis"),
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.name, None);
    }

    #[tokio::test]
    async fn close_and_reopen_fold_last_applicable_wins() {
        let dan = Member::human("Dan", "dan@example.com");
        let stranger = Member::agent("Stranger", "stranger@example.com");
        let (mut ledger, channel) = opened_channel(&dan).await;

        // Open by default.
        let view = ledger.project(&channel).await.unwrap();
        assert!(!view.closed);

        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                1,
                EntryPayload::ChannelClosed {
                    rationale: "done".into(),
                },
            ))
            .await
            .unwrap();
        let view = ledger.project(&channel).await.unwrap();
        assert!(view.closed);

        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                2,
                EntryPayload::ChannelReopened {
                    rationale: "it resumed".into(),
                },
            ))
            .await
            .unwrap();
        let view = ledger.project(&channel).await.unwrap();
        assert!(!view.closed);

        // A stranger's close has no effect (docs/adr/0017).
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                stranger,
                3,
                EntryPayload::ChannelClosed {
                    rationale: "drive-by".into(),
                },
            ))
            .await
            .unwrap();
        let view = ledger.project(&channel).await.unwrap();
        assert!(!view.closed);
    }

    #[tokio::test]
    async fn correcting_the_genesis_renames_the_channel() {
        // Rename is a corrective entry superseding the genesis binding
        // (docs/adr/0016) — last applicable wins, and a non-member's attempt
        // has no effect.
        let dan = Member::human("Dan", "dan@example.com");
        let stranger = Member::agent("Stranger", "stranger@example.com");
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let genesis = EntryId::new();
        ledger
            .append(entry(
                genesis,
                channel,
                dan.clone(),
                0,
                EntryPayload::ChannelOpened {
                    name: "first-name".into(),
                },
            ))
            .await
            .unwrap();
        for (millis, new_name) in [(1, "second-name"), (2, "third-name")] {
            ledger
                .append(entry(
                    EntryId::new(),
                    channel,
                    dan.clone(),
                    millis,
                    EntryPayload::Correction {
                        target: genesis,
                        statement: new_name.into(),
                        rationale: "renamed".into(),
                    },
                ))
                .await
                .unwrap();
        }
        // A stranger's rename does not count (docs/adr/0017).
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                stranger,
                3,
                EntryPayload::Correction {
                    target: genesis,
                    statement: "hijacked".into(),
                    rationale: "drive-by".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.name.as_deref(), Some("third-name"));
    }

    #[tokio::test]
    async fn duplicate_geneses_resolve_to_the_canonically_first() {
        // Two machines opened the "same" channel concurrently and union-merged
        // (docs/adr/0011): the canonically first genesis wins, deterministically.
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let bob = Member::human("Bob", "bob@example.com");
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                bob,
                2,
                EntryPayload::ChannelOpened {
                    name: "later-name".into(),
                },
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                alice,
                1,
                EntryPayload::ChannelOpened {
                    name: "first-name".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.name.as_deref(), Some("first-name"));
    }

    // --- Gate engine ---

    fn proposal(requirement: ApprovalRequirement) -> EntryPayload {
        EntryPayload::Proposal {
            action: "push the diff".into(),
            rationale: "ready".into(),
            provenance: Vec::new(),
            frame: None,
            requirement,
        }
    }

    #[tokio::test]
    async fn auto_requirement_approves_with_no_approvals() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let agent = Member::agent("Bot", "bot@junto.local");
        let prop = EntryId::new();
        ledger
            .append(entry(
                prop,
                channel,
                agent,
                1,
                proposal(ApprovalRequirement::Auto),
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.gate_status(&prop), Some(GateStatus::Approved));
    }

    #[tokio::test]
    async fn count_requires_that_many_distinct_approvals() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let bob = Member::human("Bob", "bob@example.com");
        let prop = EntryId::new();
        ledger
            .append(entry(
                prop,
                channel,
                alice.clone(),
                1,
                proposal(ApprovalRequirement::Count(2)),
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                alice,
                2,
                EntryPayload::Approval {
                    target: prop,
                    rationale: "ok".into(),
                },
            ))
            .await
            .unwrap();

        // One approval — still short of two.
        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.gate_status(&prop), Some(GateStatus::Pending));

        // A second, distinct approver satisfies it.
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                bob,
                3,
                EntryPayload::Approval {
                    target: prop,
                    rationale: "ok".into(),
                },
            ))
            .await
            .unwrap();
        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.gate_status(&prop), Some(GateStatus::Approved));
    }

    #[tokio::test]
    async fn count_does_not_stack_same_member() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let prop = EntryId::new();
        ledger
            .append(entry(
                prop,
                channel,
                alice.clone(),
                1,
                proposal(ApprovalRequirement::Count(2)),
            ))
            .await
            .unwrap();
        // Alice approves twice — distinct-member rule means this counts once.
        for ts in [2, 3] {
            ledger
                .append(entry(
                    EntryId::new(),
                    channel,
                    alice.clone(),
                    ts,
                    EntryPayload::Approval {
                        target: prop,
                        rationale: "ok".into(),
                    },
                ))
                .await
                .unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.gate_status(&prop), Some(GateStatus::Pending));
    }

    #[tokio::test]
    async fn all_of_requires_every_named_member() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let bob = Member::human("Bob", "bob@example.com");
        let prop = EntryId::new();
        ledger
            .append(entry(
                prop,
                channel,
                alice.clone(),
                1,
                proposal(ApprovalRequirement::AllOf(vec![alice.clone(), bob.clone()])),
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                alice,
                2,
                EntryPayload::Approval {
                    target: prop,
                    rationale: "ok".into(),
                },
            ))
            .await
            .unwrap();

        // Only Alice so far.
        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.gate_status(&prop), Some(GateStatus::Pending));

        ledger
            .append(entry(
                EntryId::new(),
                channel,
                bob,
                3,
                EntryPayload::Approval {
                    target: prop,
                    rationale: "ok".into(),
                },
            ))
            .await
            .unwrap();
        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.gate_status(&prop), Some(GateStatus::Approved));
    }

    #[tokio::test]
    async fn rejection_is_sticky_even_with_enough_approvals() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let bob = Member::human("Bob", "bob@example.com");
        let prop = EntryId::new();
        ledger
            .append(entry(
                prop,
                channel,
                alice.clone(),
                1,
                proposal(ApprovalRequirement::Count(1)),
            ))
            .await
            .unwrap();
        // Enough approvals to satisfy Count(1)...
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                alice,
                2,
                EntryPayload::Approval {
                    target: prop,
                    rationale: "ok".into(),
                },
            ))
            .await
            .unwrap();
        // ...but a rejection blocks regardless.
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                bob,
                3,
                EntryPayload::Rejection {
                    target: prop,
                    rationale: "unsafe".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.gate_status(&prop), Some(GateStatus::Rejected));
    }

    #[tokio::test]
    async fn approval_after_rejection_does_not_revive() {
        // reject@2 then approve@3 — stickiness is order-independent: you cannot
        // undo a rejection by approving. This is exactly the behaviour that
        // motivates the deferred admin-override kind (domain-model #17).
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        let bob = Member::human("Bob", "bob@example.com");
        let prop = EntryId::new();
        ledger
            .append(entry(
                prop,
                channel,
                alice.clone(),
                1,
                proposal(ApprovalRequirement::Count(1)),
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                bob,
                2,
                EntryPayload::Rejection {
                    target: prop,
                    rationale: "unsafe".into(),
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
                EntryPayload::Approval {
                    target: prop,
                    rationale: "lgtm".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.gate_status(&prop), Some(GateStatus::Rejected));
    }

    #[tokio::test]
    async fn dangling_approval_is_ignored() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        // An approval whose target is no known proposal.
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                alice,
                1,
                EntryPayload::Approval {
                    target: EntryId::new(),
                    rationale: "ok".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert!(view.gate_status.is_empty());
        assert_eq!(view.entries.len(), 1);
    }

    #[tokio::test]
    async fn proposals_are_channel_scoped() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel_a = ChannelId::new();
        let channel_b = ChannelId::new();
        let agent = Member::agent("Bot", "bot@junto.local");
        let prop_a = EntryId::new();
        ledger
            .append(entry(
                prop_a,
                channel_a,
                agent,
                1,
                proposal(ApprovalRequirement::Auto),
            ))
            .await
            .unwrap();

        let view_b = ledger.project(&channel_b).await.unwrap();
        assert!(view_b.gate_status(&prop_a).is_none());
        let view_a = ledger.project(&channel_a).await.unwrap();
        assert_eq!(view_a.gate_status(&prop_a), Some(GateStatus::Approved));
    }

    // ---- the Party & membership filter (docs/adr/0017) ----

    fn genesis(name: &str) -> EntryPayload {
        EntryPayload::ChannelOpened { name: name.into() }
    }

    fn member_added(member: &Member) -> EntryPayload {
        EntryPayload::MemberAdded {
            member: member.clone(),
        }
    }

    /// A channel with a genesis by `founder` at t=0.
    async fn opened_channel(founder: &Member) -> (Ledger<InMemorySubstrate>, ChannelId) {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                founder.clone(),
                0,
                genesis("party-test"),
            ))
            .await
            .unwrap();
        (ledger, channel)
    }

    #[tokio::test]
    async fn founder_comes_from_genesis_and_grants_extend_the_party() {
        let dan = Member::human("Dan", "dan@example.com");
        let agent = Member::agent("Agent", "agent@example.com");
        let (mut ledger, channel) = opened_channel(&dan).await;
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                1,
                member_added(&agent),
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.party, vec![dan, agent]);
        assert!(view.unrecognized.is_empty());
    }

    #[tokio::test]
    async fn non_member_acts_are_unrecognized_and_do_not_count() {
        let dan = Member::human("Dan", "dan@example.com");
        let stranger = Member::agent("Stranger", "stranger@example.com");
        let (mut ledger, channel) = opened_channel(&dan).await;

        let claim = EntryId::new();
        ledger
            .append(entry(claim, channel, dan.clone(), 1, assertion("x holds")))
            .await
            .unwrap();
        // A stranger's ratification must not move the standing...
        let stray = EntryId::new();
        ledger
            .append(entry(
                stray,
                channel,
                stranger.clone(),
                2,
                EntryPayload::Ratification {
                    target: claim,
                    rationale: "drive-by".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.standing(&claim), Some(Standing::Provisional));
        // ...but the entry is retained and surfaced, never dropped.
        assert!(view.entries.iter().any(|e| e.id == stray));
        assert!(view.unrecognized.contains(&stray));
    }

    #[tokio::test]
    async fn non_member_approval_does_not_open_a_gate() {
        let dan = Member::human("Dan", "dan@example.com");
        let stranger = Member::agent("Stranger", "stranger@example.com");
        let (mut ledger, channel) = opened_channel(&dan).await;

        let prop = EntryId::new();
        ledger
            .append(entry(
                prop,
                channel,
                dan.clone(),
                1,
                proposal(ApprovalRequirement::Count(1)),
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                stranger,
                2,
                EntryPayload::Approval {
                    target: prop,
                    rationale: "lgtm".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.gate_status(&prop), Some(GateStatus::Pending));
    }

    #[tokio::test]
    async fn member_added_by_non_founder_has_no_roster_effect() {
        let dan = Member::human("Dan", "dan@example.com");
        let agent = Member::agent("Agent", "agent@example.com");
        let interloper = Member::agent("Interloper", "interloper@example.com");
        let (mut ledger, channel) = opened_channel(&dan).await;
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                1,
                member_added(&agent),
            ))
            .await
            .unwrap();
        // A member who is not the founder cannot extend the party.
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                agent.clone(),
                2,
                member_added(&interloper),
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.party, vec![dan, agent]);
    }

    #[tokio::test]
    async fn membership_check_is_set_based_not_temporal() {
        // An entry written *before* its author's grant still counts
        // (docs/adr/0017: convergence over strictness; clock skew between
        // machines must not invalidate real work).
        let dan = Member::human("Dan", "dan@example.com");
        let agent = Member::agent("Agent", "agent@example.com");
        let (mut ledger, channel) = opened_channel(&dan).await;

        let early = EntryId::new();
        ledger
            .append(entry(
                early,
                channel,
                agent.clone(),
                1,
                assertion("written before the grant"),
            ))
            .await
            .unwrap();
        ledger
            .append(entry(EntryId::new(), channel, dan, 2, member_added(&agent)))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert!(view.unrecognized.is_empty());
        assert_eq!(view.standing(&early), Some(Standing::Provisional));
    }

    #[tokio::test]
    async fn re_adding_a_member_is_idempotent() {
        let dan = Member::human("Dan", "dan@example.com");
        let agent = Member::agent("Agent", "agent@example.com");
        let (mut ledger, channel) = opened_channel(&dan).await;
        for millis in [1, 2] {
            ledger
                .append(entry(
                    EntryId::new(),
                    channel,
                    dan.clone(),
                    millis,
                    member_added(&agent),
                ))
                .await
                .unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.party, vec![dan, agent]);
    }

    // ---- Agent Sessions & Artifacts ----

    fn session_started(intent: &str) -> EntryPayload {
        EntryPayload::SessionStarted {
            intent: intent.into(),
        }
    }

    #[tokio::test]
    async fn a_started_session_is_working_with_no_artifacts() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let agent = Member::agent("Coder", "coder@junto.local");
        let session = EntryId::new();
        ledger
            .append(entry(
                session,
                channel,
                agent,
                1,
                session_started("fix the flaky test"),
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        let session_view = view.session(&session).expect("session projected");
        assert_eq!(session_view.state, SessionState::Working);
        assert!(session_view.artifacts.is_empty());
        // A session is not an assertion: it carries no standing.
        assert_eq!(view.standing(&session), None);
    }

    #[tokio::test]
    async fn session_updates_move_state_last_applicable_wins() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let agent = Member::agent("Coder", "coder@junto.local");
        let session = EntryId::new();
        ledger
            .append(entry(
                session,
                channel,
                agent.clone(),
                1,
                session_started("work"),
            ))
            .await
            .unwrap();
        for (millis, state) in [(2, SessionState::Blocked), (3, SessionState::Done)] {
            ledger
                .append(entry(
                    EntryId::new(),
                    channel,
                    agent.clone(),
                    millis,
                    EntryPayload::SessionUpdated {
                        target: session,
                        state,
                        note: "moving on".into(),
                    },
                ))
                .await
                .unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.session(&session).unwrap().state, SessionState::Done);
    }

    #[tokio::test]
    async fn artifacts_attach_to_their_session_in_canonical_order() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let agent = Member::agent("Coder", "coder@junto.local");
        let session = EntryId::new();
        ledger
            .append(entry(
                session,
                channel,
                agent.clone(),
                1,
                session_started("work"),
            ))
            .await
            .unwrap();
        let diff = EntryId::new();
        let log = EntryId::new();
        // Append out of order; projection must list them canonically.
        ledger
            .append(entry(
                log,
                channel,
                agent.clone(),
                3,
                EntryPayload::ArtifactAttached {
                    target: session,
                    kind: "log".into(),
                    description: "test run".into(),
                    provenance: Vec::new(),
                },
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                diff,
                channel,
                agent,
                2,
                EntryPayload::ArtifactAttached {
                    target: session,
                    kind: "diff".into(),
                    description: "the fix".into(),
                    provenance: Vec::new(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.session(&session).unwrap().artifacts, vec![diff, log]);
    }

    #[tokio::test]
    async fn dangling_session_acts_are_ignored() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let agent = Member::agent("Coder", "coder@junto.local");
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                agent.clone(),
                1,
                EntryPayload::SessionUpdated {
                    target: EntryId::new(),
                    state: SessionState::Done,
                    note: "no such session".into(),
                },
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                agent,
                2,
                EntryPayload::ArtifactAttached {
                    target: EntryId::new(),
                    kind: "diff".into(),
                    description: "orphan".into(),
                    provenance: Vec::new(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert!(view.sessions.is_empty());
        assert_eq!(view.entries.len(), 2);
    }

    #[tokio::test]
    async fn non_member_session_update_does_not_move_state() {
        let dan = Member::human("Dan", "dan@example.com");
        let stranger = Member::agent("Stranger", "stranger@example.com");
        let (mut ledger, channel) = opened_channel(&dan).await;
        let session = EntryId::new();
        ledger
            .append(entry(session, channel, dan, 1, session_started("work")))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                stranger,
                2,
                EntryPayload::SessionUpdated {
                    target: session,
                    state: SessionState::Error,
                    note: "drive-by".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(view.session(&session).unwrap().state, SessionState::Working);
    }

    #[tokio::test]
    async fn no_genesis_means_no_enforcement() {
        // Legacy fallback: a channel without a genesis (dogfood-era, or synced
        // before its genesis arrived) projects everything.
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let anyone = Member::agent("Anyone", "anyone@example.com");
        let claim = EntryId::new();
        ledger
            .append(entry(claim, channel, anyone, 1, assertion("still counts")))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert!(view.party.is_empty());
        assert!(view.unrecognized.is_empty());
        assert_eq!(view.standing(&claim), Some(Standing::Provisional));
    }
}
