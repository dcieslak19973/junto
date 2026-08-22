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
    EntryId, EntryPayload, GateStatus, LedgerEntry, Member, Result, SubstrateProvider, Timestamp,
    gate::ApprovalRequirement,
    ids::ChannelId,
    session::{SessionState, SessionView},
    subject::Subject,
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

/// Whether a [`LineageEdge`] is a divergence or a convergence (`docs/adr/0027`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageRelation {
    /// A child channel departed from a parent (the side-quest birth).
    Diverge,
    /// Channels merged by a recorded act (a child back into its parent, or
    /// predecessors into a continuation).
    Converge,
}

/// The way context flows across a [`LineageEdge`], **from this channel's own
/// ledger's point of view** (`docs/adr/0027`). Orthogonal to
/// [`LineageRelation`]: the four entry kinds normalize onto the
/// `(relation, direction)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageDirection {
    /// Context flows **in**: this channel inherits from `other` — its parent on
    /// a `Diverge` (`DivergedFrom`), a predecessor on a `Converge`
    /// (`ConvergenceReceived`).
    Incoming,
    /// Context flows **out**: `other` depends on this channel — a child on a
    /// `Diverge` (`ChildDiverged`), the continuation this channel fed on a
    /// `Converge` (`ConvergedInto`).
    Outgoing,
}

/// One lineage edge as the projection presents it (`docs/adr/0027`): the four
/// distinct entry kinds normalized into a single shape that recall and the
/// lineage strip both consume. Purely **local** — built from this channel's own
/// entries; whether the reciprocal entry exists in `other`'s ledger (a dangling
/// edge) is a cross-channel question the host answers, not this projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineageEdge {
    /// Divergence or convergence.
    pub relation: LineageRelation,
    /// Which way context flows, from this channel's view.
    pub direction: LineageDirection,
    /// The channel at the other end of the edge.
    pub other: ChannelId,
    /// The divergence point — an entry **in the parent** — when this channel is
    /// the child side of a divergence (`DivergedFrom::at`); `None` otherwise.
    pub point: Option<EntryId>,
}

/// One key ever granted signing authority for an email, folded from a
/// membership-granting entry (`docs/adr/0033`). Distinct from [`Member`]:
/// a `Member` answers "who is on the roster", a `KeyGrant` answers "which
/// keys may sign for them" — a member has exactly one roster row but can
/// hold many grants, one per machine/device it enrolled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyGrant {
    /// The granted public key.
    pub key: crate::PublicKey,
    /// The entry that authorized this key — the `ChannelOpened` genesis for
    /// the founder's own key, or the founder-authored `MemberAdded` that
    /// enrolled it.
    pub granted_by: EntryId,
    /// When this grant was retired: the timestamp of the earliest
    /// founder-authored [`EntryPayload::Park`] whose `target` is
    /// [`Self::granted_by`] (`Self::project_keyring`). `None` if no such
    /// park exists.
    pub retired_at: Option<Timestamp>,
}

impl KeyGrant {
    /// Whether this grant was live at `ts`: granted (always true here,
    /// grants have no start bound of their own) and, if retired, not
    /// retired *before* `ts` — `ts` equal to `retired_at` still counts as
    /// active; only a `ts` strictly after `retired_at` does not.
    #[must_use]
    pub fn active_at(&self, ts: Timestamp) -> bool {
        self.retired_at.is_none_or(|retired| ts <= retired)
    }
}

/// Every key ever granted signing authority, keyed by email
/// (`docs/adr/0033`). Deliberately **separate** from [`ChannelView::party`]:
/// the Party is the human-facing roster (one row per member, first-write-wins
/// on email) while the keyring is the machine-facing authorization list (any
/// number of grants per email, one per enrolled device). Folding them into
/// one structure — as the pre-multi-device projection did — forces a choice
/// between "one key per member" and "devices pollute the roster"; keeping
/// them apart avoids that choice entirely. A member's grants are in
/// canonical entry order, so replay is deterministic on every replica.
pub type Keyring = std::collections::HashMap<String, Vec<KeyGrant>>;

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
    /// Every key ever granted signing authority, keyed by email
    /// ([`Keyring`]). **Not** derived from `party`: the Party answers "who is
    /// a member" (one row per email) while this answers "which keys may sign
    /// for them" (any number of grants per email, one per enrolled device).
    /// Empty when `party` is empty (no genesis, no founder, no grants), and
    /// also when the party is non-empty but no member on it has ever carried
    /// a key — a keyless founder yields a non-empty roster with no grants.
    pub keyring: Keyring,
    /// Entries that do not count: their author is not in the
    /// [`party`](ChannelView::party) (`docs/adr/0017`), **or** is in the
    /// party but was revoked as of the entry's own timestamp
    /// (`docs/adr/0035`, `Self::project_unrecognized`) — the party still
    /// lists a revoked member. Retained and surfaced, never dropped, but
    /// excluded from standings and gate folding (visibility beats
    /// mystery). Always empty when `party` is empty.
    pub unrecognized: HashSet<EntryId>,
    /// Recognized entries whose signature is absent, malformed, or does not
    /// verify against any grant in the [`keyring`](ChannelView::keyring)
    /// that was active at the entry's own timestamp (`docs/adr/0033`,
    /// `Self::project_unverified`) — any of a member's devices may sign,
    /// but a device's grant stops covering entries stamped after that
    /// device was retired via a founder `Park`, even though the signature
    /// itself is still cryptographically valid. A surfaced **fact**, never
    /// a drop and never a gate: standings, gates and sessions fold
    /// identically — authorship verification is independent of authority
    /// (`docs/adr/0004`). Entries by a keyless member land here too
    /// (nothing to verify against), so legacy unsigned history reads as
    /// unverified rather than silently trusted.
    pub unverified: HashSet<EntryId>,
    /// Current standing per assertion [`EntryId`].
    pub standings: HashMap<EntryId, Standing>,
    /// Current gate status per proposal [`EntryId`].
    pub gate_status: HashMap<EntryId, GateStatus>,
    /// Per actionable proposal, whether its authorized action has been carried
    /// out — folded last-applicable-wins from [`EntryPayload::GateExecuted`]
    /// entries (`docs/adr/0030`). `Some(true)` = done, `Some(false)` = failed,
    /// absent = not executed (the gap an approved actionable gate must not sit
    /// in silently).
    pub gate_executions: HashMap<EntryId, bool>,
    /// Current view per Agent Session — keyed by the
    /// [`EntryPayload::SessionStarted`] entry's id, holding the folded
    /// [`SessionState`] and the session's attached artifacts.
    pub sessions: HashMap<EntryId, SessionView>,
    /// Whether the channel is closed (`docs/adr/0022`): the last applicable
    /// [`EntryPayload::ChannelClosed`] / [`EntryPayload::ChannelReopened`]
    /// wins, in canonical order, members only. The record outlives the
    /// inquiry — closed only means "out of the working set".
    pub closed: bool,
    /// This channel's **lineage edges** (`docs/adr/0027`), normalized from its
    /// `DivergedFrom` / `ChildDiverged` / `ConvergedInto` /
    /// `ConvergenceReceived` entries (members only), in canonical order. Local
    /// to this channel — the reciprocal entry in `other`'s ledger is the host's
    /// concern, not the projection's.
    pub lineage: Vec<LineageEdge>,
    /// The Subjects this channel is about (spec §1), in canonical attachment
    /// order, each paired with the id of the `SubjectAttached` entry that
    /// introduced it. Detached subjects are folded out; both entries stay in
    /// [`entries`](ChannelView::entries), because the record is append-only.
    /// Members only, like every other fold (`docs/adr/0017`).
    pub subjects: Vec<(EntryId, Subject)>,
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

    /// Whether an approved gate's authorized action has run (`docs/adr/0030`):
    /// `Some(true)` carried out, `Some(false)` failed, `None` not (yet)
    /// executed.
    #[must_use]
    pub fn gate_executed(&self, id: &EntryId) -> Option<bool> {
        self.gate_executions.get(id).copied()
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
    /// A short-lived projection cache (`docs/adr/0002` projections are pure
    /// over the log). Reads re-fold from the substrate, which for the git-refs
    /// backend means spawning `git` per request — costly under the human
    /// surface's click-through. Cached views serve rapid navigation; a local
    /// [`Ledger::append`] invalidates the channel, and a [`PROJECTION_TTL`]
    /// bound keeps sync-fetched changes from lingering stale.
    cache: std::sync::Mutex<HashMap<ChannelId, (std::time::Instant, ChannelView)>>,
}

/// How long a cached [`ChannelView`] is served before re-projecting. Sized to
/// cover human-paced click-around (a few seconds between navigations) so the
/// surface stays snappy without re-shelling-out to git each time. Local writes
/// — including agent writes over the in-process MCP surface — invalidate
/// immediately; only entries fetched from a *remote* by background sync can be
/// this stale, which is acceptable for the human read surface.
const PROJECTION_TTL: std::time::Duration = std::time::Duration::from_secs(15);

impl<S: SubstrateProvider> Ledger<S> {
    /// Wrap a substrate.
    pub fn new(substrate: S) -> Self {
        Self {
            substrate,
            cache: std::sync::Mutex::new(HashMap::new()),
        }
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
        let channel = entry.channel;
        self.substrate.append(entry).await?;
        // Invalidate the channel's cached projection so the writer sees their
        // own write immediately (the TTL covers entries that arrive by sync).
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(&channel);
        }
        Ok(())
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
        // Fast path: a fresh cached projection avoids re-folding the log (and,
        // for the git-refs backend, re-spawning `git`) on rapid navigation.
        if let Ok(cache) = self.cache.lock()
            && let Some((at, view)) = cache.get(channel)
            && at.elapsed() < PROJECTION_TTL
        {
            return Ok(view.clone());
        }
        self.project_uncached(channel).await
    }

    /// [`Self::project`], but always re-folds from the substrate — skips
    /// the cache-read fast path (still writes the fresh result into the
    /// shared cache afterward, so it also refreshes every other reader's
    /// next cache hit, not just this call's).
    ///
    /// For every reader except one, [`Self::project`]'s
    /// [`PROJECTION_TTL`] staleness is an accepted trade for avoiding a
    /// re-fold on rapid navigation (see that method's docs) — the human
    /// read surface tolerates it. The live-plane websocket handshake
    /// (`junto::live_ws::live_session`) is the one exception this exists
    /// for: an operator's `revoke-member`/`retire-device` runs as a
    /// SEPARATE process from a long-running `junto serve` — a separate
    /// `Host`, a separate `Ledger`, a separate in-memory cache — so that
    /// process's cache invalidation on append never reaches the serving
    /// process's cache at all. Without this, a just-revoked member could
    /// still authenticate a new live connection for up to
    /// [`PROJECTION_TTL`] after the revoking command already returned.
    /// One fresh read per connection is negligible.
    ///
    /// # Errors
    /// Propagates any error from the underlying [`SubstrateProvider`].
    pub async fn project_fresh(&self, channel: &ChannelId) -> Result<ChannelView> {
        self.project_uncached(channel).await
    }

    async fn project_uncached(&self, channel: &ChannelId) -> Result<ChannelView> {
        let mut entries = self.substrate.entries(channel).await?;
        entries.sort_by(LedgerEntry::canonical_cmp);
        // Keep the first occurrence of each id (in canonical order), so a
        // double-appended entry projects as one.
        let mut seen = HashSet::new();
        entries.retain(|entry| seen.insert(entry.id));

        // `party` and `keyring` are folded from the full `entries` list
        // below, never from `recognized` — deliberately: recognition is
        // *derived from* the keyring (`Self::project_unrecognized` needs
        // it to compute a cutoff), so gating the keyring on recognition
        // would be circular. Consequence: a revoked member's own later
        // `MemberAdded`/`Park` entries still take full effect on the
        // roster and on other members' grants — a revoked founder, for
        // instance, keeps founder authority to add members and grant
        // keys. Only the folds below that consume `recognized`
        // (unverified, standings, gates, sessions, lineage,
        // genesis/name, close/reopen) are gated by revocation.
        // Re-rooting a compromised founder's authority is unaddressed
        // here and is not asked for by this plan.
        let party = Self::project_party(&entries);
        let keyring = match party.first() {
            Some(founder) => Self::project_keyring(&entries, &founder.email),
            None => Keyring::new(),
        };
        // Membership is set-based, not temporal (`docs/adr/0017`): an
        // entry's author being in the Party does not depend on where the
        // granting entry falls in canonical order. But membership alone is
        // no longer sufficient for the recognized-based projections below
        // (unverified, standings, gates, sessions, lineage, name,
        // close/reopen) — a revoked member's post-cutoff entries stop
        // counting toward those, even though they remain in the Party
        // (`docs/adr/0035`, Task 3; see `Self::project_unrecognized`). An
        // unrecognized entry never reaches `Self::project_unverified`
        // below at all, since that fold only sees `recognized` — this is
        // exactly why two Task 2 tests needed a second, never-parked
        // device to stay meaningful once their sole grant was retired.
        let unrecognized = Self::project_unrecognized(&party, &keyring, &entries);
        let recognized: Vec<&LedgerEntry> = entries
            .iter()
            .filter(|entry| !unrecognized.contains(&entry.id))
            .collect();
        let unverified = Self::project_unverified(&party, &keyring, &recognized);

        let standings = Self::project_standings(&recognized);
        let gate_status = Self::project_gates(&recognized);
        let gate_executions = Self::project_gate_executions(&recognized);
        let sessions = Self::project_sessions(&recognized);
        let lineage = Self::project_lineage(&recognized);
        let subjects = Self::project_subjects(&recognized);
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

        let view = ChannelView {
            name,
            entries,
            party,
            keyring,
            unrecognized,
            unverified,
            standings,
            gate_status,
            gate_executions,
            sessions,
            closed,
            lineage,
            subjects,
        };
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(*channel, (std::time::Instant::now(), view.clone()));
        }
        Ok(view)
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

    /// Fold the **keyring** out of an ordered entry list (`docs/adr/0033`):
    /// every key ever granted signing authority, keyed by email, together
    /// with when each grant was retired. Distinct from [`Self::project_party`]
    /// on purpose — see [`Keyring`]'s doc comment. The genesis author's key
    /// is granted by the canonically *first* `ChannelOpened` entry, and by
    /// that entry alone — mirroring `project_party`'s founder, which is
    /// fixed the same way. Any later `ChannelOpened` grants nothing, full
    /// stop; this does **not** rest on whether that later entry ends up
    /// `unrecognized` (`Self::project`) — a second genesis re-authored by an
    /// email already on the roster (the founder's own re-open, or an added
    /// member's) is ordinarily still *recognized*: party membership alone
    /// decides that, independent of any revocation cutoff
    /// (`Self::project_unrecognized`) — yet must still not grant. Without a hard
    /// first-genesis-only rule, any peer could inject a signing key for an
    /// arbitrary email by appending a `ChannelOpened`. After the genesis, a
    /// `MemberAdded` contributes a grant iff its author is the founder
    /// (grant authority is the founder's alone) and the added member
    /// carries a key (keyless members stay keyless). Grants accumulate in
    /// the caller's entry order, which is canonical, so a member's grant
    /// list is deterministic on every replica.
    ///
    /// A second pass retires grants: a founder-authored [`EntryPayload::Park`]
    /// whose `target` is a grant's [`KeyGrant::granted_by`] retires that
    /// grant as of the park's own `timestamp` — [`Self::project_unverified`]
    /// then stops it from verifying entries stamped after that timestamp;
    /// entries stamped at or before are unaffected. A `Park` fails to
    /// retire for one of two distinct reasons: authored by anyone but the
    /// founder, it is dropped by the `entry.author.email == founder_email`
    /// guard below *before* the retirement map is ever consulted — that
    /// guard is load-bearing, not redundant, since its `target` is very
    /// often a real `granted_by` (a member parking its own device); or,
    /// authored by the founder but targeting an entry that granted no key,
    /// it reaches the map but matches no `granted_by` there, the same
    /// leniency dangling targets get elsewhere in this file. Several parks
    /// on the same grant: the *earliest* timestamp wins, so a grant's
    /// retirement can only move earlier, never later, regardless of
    /// entry-append order.
    fn project_keyring(entries: &[LedgerEntry], founder_email: &str) -> Keyring {
        let mut keyring: Keyring = HashMap::new();
        let mut genesis_seen = false;
        for entry in entries {
            match &entry.payload {
                EntryPayload::ChannelOpened { .. } => {
                    // Only the canonically first genesis grants a key —
                    // regardless of whether *it* carries one, so a keyless
                    // first genesis can't let a later, keyed one claim
                    // founder authority.
                    if genesis_seen {
                        continue;
                    }
                    genesis_seen = true;
                    if let Some(key) = entry.author.public_key.clone() {
                        keyring
                            .entry(entry.author.email.clone())
                            .or_default()
                            .push(KeyGrant {
                                key,
                                granted_by: entry.id,
                                retired_at: None,
                            });
                    }
                }
                EntryPayload::MemberAdded { member } => {
                    if entry.author.email == founder_email
                        && let Some(key) = member.public_key.clone()
                    {
                        keyring
                            .entry(member.email.clone())
                            .or_default()
                            .push(KeyGrant {
                                key,
                                granted_by: entry.id,
                                retired_at: None,
                            });
                    }
                }
                _ => {}
            }
        }

        // Earliest founder-authored Park per targeted grant. A park
        // targeting a non-granting entry never matches any `granted_by`
        // below, so it is ignored without a separate dangling-target check.
        let mut retirements: HashMap<EntryId, Timestamp> = HashMap::new();
        for entry in entries {
            if let EntryPayload::Park { target, .. } = &entry.payload
                && entry.author.email == founder_email
            {
                retirements
                    .entry(*target)
                    .and_modify(|earliest| *earliest = (*earliest).min(entry.timestamp))
                    .or_insert(entry.timestamp);
            }
        }
        if !retirements.is_empty() {
            for grant in keyring.values_mut().flatten() {
                if let Some(&retired_at) = retirements.get(&grant.granted_by) {
                    grant.retired_at = Some(retired_at);
                }
            }
        }

        keyring
    }

    /// Fold which entries are **unrecognized** out of an ordered entry list
    /// (`docs/adr/0017`, amended by `docs/adr/0035`, Task 3): an entry
    /// counts iff its author is in the Party — set-based, not temporal,
    /// wherever the grant falls in canonical order — **and** its author
    /// has not been revoked as of the entry's own `timestamp`. No genesis
    /// ⇒ empty Party ⇒ no enforcement.
    ///
    /// Revocation is derived from the keyring, never from Party
    /// membership: the member is **never** removed from
    /// [`ChannelView::party`] here, or anywhere — doing so would mark
    /// every entry that author ever wrote unrecognized and erase their
    /// history from every downstream projection. Instead, for each email
    /// with at least one grant: if *every* grant for it is retired, the
    /// email's cutoff is the *latest* [`KeyGrant::retired_at`] among
    /// them — the moment after which the email held **no** valid key at
    /// all, across any of its devices — and if any grant is still active,
    /// there is no cutoff — partial retirement (one lost device among
    /// several) must not offboard the person. Using the earliest
    /// retirement instead would be wrong: a member who retires one device
    /// and keeps working from another (still no cutoff, by the rule
    /// above) would have that dormant early retirement reach back and
    /// unrecognize the legitimate work once their *other* device is later
    /// retired too — the boundary is supposed to mark when the person
    /// actually went dark, not when they first lost any one device. An
    /// email with no grant at all (a keyless member) likewise has no
    /// cutoff: there is nothing for a founder to park, so this mechanism
    /// cannot revoke them. An entry from a cutoff email is unrecognized
    /// iff its `timestamp` is *strictly after* the cutoff — mirroring
    /// [`KeyGrant::active_at`]'s inclusive boundary, an entry stamped
    /// exactly at the cutoff still counts, and nothing stamped at or
    /// before it is rewritten.
    ///
    /// This closes the gap [`Self::project_unverified`] leaves open on its
    /// own: an unverified entry is still *recognized*, so it still carries
    /// standings, closes gates, and appears in sessions and lineage.
    /// Retiring every one of a member's keys would, without this, leave
    /// them merely flagged there rather than excluded from them. It does
    /// **not** touch the roster or a revoked member's own future grant or
    /// membership authority — see the note above `Self::project_party`'s
    /// call site for that exact boundary.
    fn project_unrecognized(
        party: &[Member],
        keyring: &Keyring,
        entries: &[LedgerEntry],
    ) -> HashSet<EntryId> {
        if party.is_empty() {
            return HashSet::new();
        }
        let member_emails: HashSet<&str> =
            party.iter().map(|member| member.email.as_str()).collect();
        // Single traversal, panic-free: `try_fold` short-circuits to
        // `None` (via the inner `Option::map` yielding `None`) the moment
        // it meets a grant that is still active, giving "no cutoff" for
        // that email exactly as intended — partial retirement must not
        // offboard the person. Otherwise the accumulator tracks the
        // *latest* `retired_at` seen so far, so once every grant has been
        // folded (none of them active) the result is the latest retirement
        // across all of the email's grants — the moment it lost its last
        // valid key.
        let cutoffs: HashMap<&str, Timestamp> = keyring
            .iter()
            .filter_map(|(email, grants)| {
                let cutoff = grants
                    .iter()
                    .try_fold(None::<Timestamp>, |latest, grant| {
                        grant.retired_at.map(|retired_at| {
                            Some(latest.map_or(retired_at, |l| l.max(retired_at)))
                        })
                    })
                    .flatten()?;
                Some((email.as_str(), cutoff))
            })
            .collect();
        entries
            .iter()
            .filter(|entry| {
                !member_emails.contains(entry.author.email.as_str())
                    || cutoffs
                        .get(entry.author.email.as_str())
                        .is_some_and(|&cutoff| entry.timestamp > cutoff)
            })
            .map(|entry| entry.id)
            .collect()
    }

    /// Which recognized entries fail authorship verification
    /// (`docs/adr/0033`, Task 2): an entry verifies iff **some** grant for
    /// its author's email is active at the entry's own `timestamp`
    /// ([`KeyGrant::active_at`]) and the entry's signature matches that
    /// grant's key ([`LedgerEntry::verifies_with`]) — any of a member's
    /// devices may sign, and a retired device stops verifying only entries
    /// stamped after its retirement; entries stamped at or before are
    /// unaffected (`KeyGrant::active_at` is inclusive at the boundary).
    /// An entry is **unverified** when its author's email has no such
    /// grant. With an empty Party (no genesis) nothing is marked —
    /// consistent with membership not being enforced there either.
    fn project_unverified(
        party: &[Member],
        keyring: &Keyring,
        recognized: &[&LedgerEntry],
    ) -> HashSet<EntryId> {
        if party.is_empty() {
            return HashSet::new();
        }
        recognized
            .iter()
            .filter(|entry| {
                !keyring
                    .get(entry.author.email.as_str())
                    .is_some_and(|grants| {
                        grants.iter().any(|grant| {
                            grant.active_at(entry.timestamp) && entry.verifies_with(&grant.key)
                        })
                    })
            })
            .map(|entry| entry.id)
            .collect()
    }

    /// Fold this channel's **lineage edges** out of an ordered list of
    /// *recognized* entries (`docs/adr/0027`): the four edge kinds normalize
    /// onto `(relation, direction, other, point)`. Local only — each entry is
    /// one end of an edge; the reciprocal in `other`'s ledger is the host's
    /// concern. Order follows canonical entry order.
    fn project_lineage(entries: &[&LedgerEntry]) -> Vec<LineageEdge> {
        entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                EntryPayload::DivergedFrom { parent, at } => Some(LineageEdge {
                    relation: LineageRelation::Diverge,
                    direction: LineageDirection::Incoming,
                    other: *parent,
                    point: *at,
                }),
                EntryPayload::ChildDiverged { child } => Some(LineageEdge {
                    relation: LineageRelation::Diverge,
                    direction: LineageDirection::Outgoing,
                    other: *child,
                    point: None,
                }),
                EntryPayload::ConvergedInto { target } => Some(LineageEdge {
                    relation: LineageRelation::Converge,
                    direction: LineageDirection::Outgoing,
                    other: *target,
                    point: None,
                }),
                EntryPayload::ConvergenceReceived { source } => Some(LineageEdge {
                    relation: LineageRelation::Converge,
                    direction: LineageDirection::Incoming,
                    other: *source,
                    point: None,
                }),
                _ => None,
            })
            .collect()
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
                | EntryPayload::DivergedFrom { .. }
                | EntryPayload::ChildDiverged { .. }
                | EntryPayload::ConvergedInto { .. }
                | EntryPayload::ConvergenceReceived { .. }
                | EntryPayload::Assertion { .. }
                | EntryPayload::Proposal { .. }
                | EntryPayload::Approval { .. }
                | EntryPayload::Rejection { .. }
                | EntryPayload::GateExecuted { .. }
                | EntryPayload::SessionStarted { .. }
                | EntryPayload::SessionUpdated { .. }
                | EntryPayload::ArtifactAttached { .. }
                | EntryPayload::SubjectAttached { .. }
                | EntryPayload::SubjectDetached { .. } => continue,
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
    /// Fold per-proposal execution status out of `GateExecuted` entries
    /// (`docs/adr/0030`), last-applicable-wins in canonical order — so a later
    /// successful retry supersedes an earlier failure. Keyed by the target
    /// proposal id; absent means never executed.
    fn project_gate_executions(entries: &[&LedgerEntry]) -> HashMap<EntryId, bool> {
        let mut executions = HashMap::new();
        for entry in entries {
            if let EntryPayload::GateExecuted {
                target, success, ..
            } = &entry.payload
            {
                executions.insert(*target, *success);
            }
        }
        executions
    }

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

    /// Fold the live Subjects out of an ordered list of *recognized* entries.
    /// Two passes: collect the attachments in canonical order, then drop the
    /// ones any `SubjectDetached` targets, regardless of the detachment's own
    /// position relative to its target. This is deliberately
    /// **order-insensitive** — unlike `project_sessions`, which only applies
    /// a `SessionUpdated` that comes after its session's start — because a
    /// detachment withdraws its target outright: replicas must agree on the
    /// live set even when clocks skew or two entries' timestamps collide,
    /// and tie-breaking on canonical order would let that agreement drift.
    fn project_subjects(entries: &[&LedgerEntry]) -> Vec<(EntryId, Subject)> {
        let mut attached: Vec<(EntryId, Subject)> = Vec::new();
        let mut detached: HashSet<EntryId> = HashSet::new();
        for entry in entries {
            match &entry.payload {
                EntryPayload::SubjectAttached { subject } => {
                    attached.push((entry.id, subject.clone()));
                }
                EntryPayload::SubjectDetached { target } => {
                    detached.insert(*target);
                }
                _ => {}
            }
        }
        attached
            .into_iter()
            .filter(|(id, _)| !detached.contains(id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ApprovalRequirement, EntryId, EntryPayload, GateStatus, InMemorySubstrate, KeyGrant,
        Ledger, LedgerEntry, LineageDirection, LineageEdge, LineageRelation, Member, SessionState,
        Standing, Subject, SubjectKind, Timestamp, Uri, ids::ChannelId,
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
            signature: None,
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
    async fn detaching_a_subject_removes_it_but_keeps_both_entries() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let dan = Member::human("Dan", "dan@example.com");

        // One author throughout: `project_subjects` folds recognized entries
        // only, and the genesis author is the founding member (ADR 0017).
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                1,
                EntryPayload::ChannelOpened {
                    name: "subjects".into(),
                },
            ))
            .await
            .expect("append genesis");

        let repo = Subject::new(
            SubjectKind::Repo,
            Uri::new("git+https://example.com/a.git").expect("valid uri"),
        );
        let doc = Subject::new(
            SubjectKind::Document,
            Uri::new("file:///notes/spec.md").expect("valid uri"),
        );

        let repo_attach = EntryId::new();
        ledger
            .append(entry(
                repo_attach,
                channel,
                dan.clone(),
                2,
                EntryPayload::SubjectAttached {
                    subject: repo.clone(),
                },
            ))
            .await
            .expect("attach repo");
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                3,
                EntryPayload::SubjectAttached {
                    subject: doc.clone(),
                },
            ))
            .await
            .expect("attach doc");
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                4,
                EntryPayload::SubjectDetached {
                    target: repo_attach,
                },
            ))
            .await
            .expect("detach repo");

        let view = ledger.project(&channel).await.expect("project");
        let subjects: Vec<_> = view.subjects.iter().map(|(_, s)| s.clone()).collect();
        assert_eq!(subjects, vec![doc], "the detached repo must not project");
        assert_eq!(
            view.entries.len(),
            4,
            "append-only: genesis plus three entries all stay in the log"
        );
    }

    #[tokio::test]
    async fn a_channel_with_no_subjects_projects_an_empty_list() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let dan = Member::human("Dan", "dan@example.com");
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan,
                1,
                EntryPayload::ChannelOpened {
                    name: "empty".into(),
                },
            ))
            .await
            .expect("append genesis");
        let view = ledger.project(&channel).await.expect("project");
        assert!(view.subjects.is_empty());
    }

    #[tokio::test]
    async fn subjects_project_in_canonical_order_paired_with_their_attaching_entry_id() {
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let dan = Member::human("Dan", "dan@example.com");

        ledger
            .append(entry(
                EntryId::new(),
                channel,
                dan.clone(),
                1,
                EntryPayload::ChannelOpened {
                    name: "subjects".into(),
                },
            ))
            .await
            .expect("append genesis");

        let repo = Subject::new(
            SubjectKind::Repo,
            Uri::new("git+https://example.com/a.git").expect("valid uri"),
        );
        let doc = Subject::new(
            SubjectKind::Document,
            Uri::new("file:///notes/spec.md").expect("valid uri"),
        );

        let repo_attach = EntryId::new();
        ledger
            .append(entry(
                repo_attach,
                channel,
                dan.clone(),
                2,
                EntryPayload::SubjectAttached {
                    subject: repo.clone(),
                },
            ))
            .await
            .expect("attach repo");

        let doc_attach = EntryId::new();
        ledger
            .append(entry(
                doc_attach,
                channel,
                dan,
                3,
                EntryPayload::SubjectAttached {
                    subject: doc.clone(),
                },
            ))
            .await
            .expect("attach doc");

        let view = ledger.project(&channel).await.expect("project");
        assert_eq!(
            view.subjects,
            vec![(repo_attach, repo), (doc_attach, doc)],
            "each subject must pair with the id of the entry that attached it, \
             in canonical attachment order"
        );
    }

    /// `docs/adr/0033` — verification is a projection fact. A channel whose
    /// keyed founder signs projects `verified`; a tampered or unsigned entry
    /// lands in `unverified` but still folds (never a drop, never a gate).
    #[tokio::test]
    async fn unverified_is_a_surfaced_fact_not_a_drop() {
        let key = crate::SigningKey::from_secret_bytes([9; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        // Signed genesis by the keyed founder.
        let mut genesis = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        genesis.sign(&key).unwrap();

        // A signed assertion verifies; a tampered one and an unsigned one do not.
        let signed_id = EntryId::new();
        let mut signed = entry(signed_id, channel, dan.clone(), 2, assertion("signed"));
        signed.sign(&key).unwrap();

        let tampered_id = EntryId::new();
        let mut tampered = entry(tampered_id, channel, dan.clone(), 3, assertion("original"));
        tampered.sign(&key).unwrap();
        if let EntryPayload::Assertion { statement, .. } = &mut tampered.payload {
            *statement = "forged".into();
        }

        let unsigned_id = EntryId::new();
        let unsigned = entry(unsigned_id, channel, dan.clone(), 4, assertion("unsigned"));

        for e in [genesis.clone(), signed, tampered, unsigned] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(!view.unverified.contains(&genesis.id));
        assert!(!view.unverified.contains(&signed_id));
        assert!(view.unverified.contains(&tampered_id));
        assert!(view.unverified.contains(&unsigned_id));
        // Never a drop: all three assertions still fold into standings.
        for id in [signed_id, tampered_id, unsigned_id] {
            assert_eq!(view.standing(&id), Some(Standing::Provisional));
        }
    }

    /// `docs/adr/0033` — a keyless member's entries are unverified (nothing to
    /// verify against), and an empty Party marks nothing, consistent with
    /// membership not being enforced without a genesis.
    #[tokio::test]
    async fn keyless_member_is_unverified_and_no_party_marks_nothing() {
        let dan = Member::human("Dan", "dan@example.com"); // no key
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        // No genesis: nothing is marked.
        let floating = EntryId::new();
        ledger
            .append(entry(floating, channel, dan.clone(), 1, assertion("pre")))
            .await
            .unwrap();
        let view = ledger.project(&channel).await.unwrap();
        assert!(view.unverified.is_empty());

        // With a genesis by the keyless founder, entries are unverified.
        let genesis = EntryId::new();
        ledger
            .append(entry(
                genesis,
                channel,
                dan.clone(),
                0,
                EntryPayload::ChannelOpened { name: "ch".into() },
            ))
            .await
            .unwrap();
        let view = ledger.project(&channel).await.unwrap();
        assert!(view.unverified.contains(&genesis));
        assert!(view.unverified.contains(&floating));
    }

    /// `docs/adr/0033` — the keyring comes from the membership-granting
    /// entries: a member added with a key verifies with it; signing with a
    /// *different* member's key does not verify.
    #[tokio::test]
    async fn keyring_is_per_member_from_member_added() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let agent_key = crate::SigningKey::from_secret_bytes([2; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let agent = Member::agent("Worker", "worker@agents.junto").with_key(agent_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let mut genesis = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        genesis.sign(&founder_key).unwrap();
        let mut grant = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: agent.clone(),
            },
        );
        grant.sign(&founder_key).unwrap();

        // The agent signs with its own key — verifies. Signing with the
        // operator's key would not: the keys are per member, not shared.
        let own_id = EntryId::new();
        let mut own = entry(own_id, channel, agent.clone(), 3, assertion("mine"));
        own.sign(&agent_key).unwrap();

        let crossed_id = EntryId::new();
        let mut crossed = entry(crossed_id, channel, agent.clone(), 4, assertion("crossed"));
        crossed.sign(&founder_key).unwrap();

        for e in [genesis, grant, own, crossed] {
            ledger.append(e).await.unwrap();
        }
        let view = ledger.project(&channel).await.unwrap();
        assert!(!view.unverified.contains(&own_id));
        assert!(view.unverified.contains(&crossed_id));
    }

    /// The keyring accumulates every key ever granted for an email, not just
    /// the latest — the founder's genesis key plus a second device key
    /// enrolled later via `MemberAdded` both land in `dan@x.com`'s grants.
    #[tokio::test]
    async fn keyring_unions_multiple_grants_for_one_email() {
        let k1 = crate::SigningKey::from_secret_bytes([1; 32]);
        let k2 = crate::SigningKey::from_secret_bytes([2; 32]);
        let dan = Member::human("Dan", "dan@x.com").with_key(k1.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis_id = EntryId::new();
        let genesis = entry(
            genesis_id,
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        // The founder's own second device, enrolled via a self-authored
        // MemberAdded carrying k2 (see spec "Enrollment flow").
        let grant_id = EntryId::new();
        let grant = entry(
            grant_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: dan.clone().with_key(k2.public_key()),
            },
        );

        for e in [genesis, grant] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        let grants = view.keyring.get("dan@x.com").expect("dan has grants");
        assert_eq!(grants.len(), 2, "genesis key plus the enrolled device key");
        assert_eq!(
            grants[0].key,
            k1.public_key(),
            "grants accumulate in canonical entry order"
        );
        assert_eq!(grants[0].granted_by, genesis_id);
        assert_eq!(grants[1].key, k2.public_key());
        assert_eq!(grants[1].granted_by, grant_id);
    }

    /// A `MemberAdded` authored by a non-founder member carrying a key must
    /// not appear in the keyring — grant authority is the founder's alone.
    #[tokio::test]
    async fn non_founder_member_added_contributes_no_key() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let outsider_key = crate::SigningKey::from_secret_bytes([2; 32]);
        let smuggled_key = crate::SigningKey::from_secret_bytes([3; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let outsider =
            Member::human("Outsider", "outsider@example.com").with_key(outsider_key.public_key());
        let smuggled =
            Member::human("Smuggled", "smuggled@example.com").with_key(smuggled_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        // First, the founder legitimately adds the outsider to the roster...
        let add_outsider = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: outsider.clone(),
            },
        );
        // ...then the outsider (not the founder) tries to grant a key to a
        // third party. Grant authority is the founder's alone.
        let smuggled_grant = entry(
            EntryId::new(),
            channel,
            outsider.clone(),
            3,
            EntryPayload::MemberAdded {
                member: smuggled.clone(),
            },
        );

        for e in [genesis, add_outsider, smuggled_grant] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(!view.keyring.contains_key("smuggled@example.com"));
    }

    /// `member.public_key == None` means no grant for that email, even
    /// though the founder authored the `MemberAdded`.
    #[tokio::test]
    async fn keyless_member_added_contributes_no_grant() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let keyless = Member::human("Keyless", "keyless@example.com"); // no key
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        let add_keyless = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: keyless.clone(),
            },
        );

        for e in [genesis, add_keyless] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(!view.keyring.contains_key("keyless@example.com"));
        // Still on the roster — keyless is a party fact, not a keyring one.
        assert!(view.party.iter().any(|m| m.email == "keyless@example.com"));
    }

    /// Two `MemberAdded` entries for the SAME email: the party still holds
    /// ONE row (first-write-wins, ledger.rs `project_party`) while the
    /// keyring holds two grants. This is the decision that keeps devices out
    /// of the roster.
    #[tokio::test]
    async fn party_projection_is_unchanged_by_the_keyring() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let k1 = crate::SigningKey::from_secret_bytes([2; 32]);
        let k2 = crate::SigningKey::from_secret_bytes([3; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        // Same email, two different devices, both authored by the founder.
        let member = Member::human("Mia", "mia@example.com");
        let first_device = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: member.clone().with_key(k1.public_key()),
            },
        );
        let second_device = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            3,
            EntryPayload::MemberAdded {
                member: member.clone().with_key(k2.public_key()),
            },
        );

        for e in [genesis, first_device, second_device] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(
            view.party
                .iter()
                .filter(|m| m.email == "mia@example.com")
                .count(),
            1,
            "party dedups by email, first-write-wins"
        );
        let grants: &Vec<KeyGrant> = view.keyring.get("mia@example.com").expect("grants");
        assert_eq!(grants.len(), 2, "keyring holds a grant per device");
        assert_eq!(grants[0].key, k1.public_key());
        assert_eq!(grants[1].key, k2.public_key());
    }

    /// `docs/adr/0011` union-merge can leave two `ChannelOpened` entries in
    /// the log; `project_party` resolves that to the canonically first
    /// author as founder, and the second genesis is `unrecognized`
    /// (`Self::project`). The keyring must track that exactly: only the
    /// first genesis's key is granted. A naive implementation that grants
    /// from every `ChannelOpened` would let any peer inject a signing key
    /// for an arbitrary email by appending a second genesis — this is the
    /// regression that guards against it.
    #[tokio::test]
    async fn only_the_canonically_first_genesis_grants_a_key() {
        let alice_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let bob_key = crate::SigningKey::from_secret_bytes([2; 32]);
        let alice = Member::human("Alice", "alice@example.com").with_key(alice_key.public_key());
        let bob = Member::human("Bob", "bob@example.com").with_key(bob_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        // Bob's genesis is appended first but sorts canonically second
        // (later timestamp) — the same pattern as
        // `duplicate_geneses_resolve_to_the_canonically_first`.
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                bob.clone(),
                2,
                EntryPayload::ChannelOpened {
                    name: "later".into(),
                },
            ))
            .await
            .unwrap();
        ledger
            .append(entry(
                EntryId::new(),
                channel,
                alice.clone(),
                1,
                EntryPayload::ChannelOpened {
                    name: "first".into(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert_eq!(
            view.party.first().map(|m| m.email.as_str()),
            Some("alice@example.com"),
            "alice's genesis is canonically first"
        );
        assert!(
            view.keyring.contains_key("alice@example.com"),
            "the canonical founder's key is granted"
        );
        assert!(
            !view.keyring.contains_key("bob@example.com"),
            "the rejected second genesis must not seed a grant"
        );
    }

    /// `docs/adr/0033` (Task 2) — a founder-authored `Park` targeting the
    /// entry that granted a key retires that grant *at the park's own
    /// timestamp*, not from the beginning of time: an entry signed with the
    /// retired key still verifies if it is stamped before the park or at
    /// exactly the park's own timestamp (`KeyGrant::active_at` is
    /// inclusive at the boundary), and fails to verify only once stamped
    /// strictly after it. Retiring a device does not rewrite history.
    ///
    /// The agent carries a *second*, never-parked device so the person as
    /// a whole is never fully revoked (`docs/adr/0035`, Task 3) — with one
    /// grant still active there is no cutoff (decision: partial retirement
    /// must not offboard the person), which keeps this test isolated to
    /// the verification-layer boundary it is named for, rather than
    /// entries silently moving from `unverified` to `unrecognized`.
    #[tokio::test]
    async fn a_retired_grant_verifies_before_its_park_and_not_after() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let agent_key = crate::SigningKey::from_secret_bytes([2; 32]);
        let agent_key2 = crate::SigningKey::from_secret_bytes([3; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let agent = Member::agent("Worker", "worker@agents.junto").with_key(agent_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let mut genesis = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        genesis.sign(&founder_key).unwrap();

        let grant_id = EntryId::new();
        let mut grant = entry(
            grant_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: agent.clone(),
            },
        );
        grant.sign(&founder_key).unwrap();

        // A second device, never parked, so the person keeps an active
        // grant throughout — no revocation cutoff applies to them.
        let mut second_device = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            3,
            EntryPayload::MemberAdded {
                member: agent.clone().with_key(agent_key2.public_key()),
            },
        );
        second_device.sign(&founder_key).unwrap();

        let before_id = EntryId::new();
        let mut before = entry(
            before_id,
            channel,
            agent.clone(),
            4,
            assertion("before park"),
        );
        before.sign(&agent_key).unwrap();

        let park = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            5,
            EntryPayload::Park {
                target: grant_id,
                rationale: "device lost".into(),
            },
        );

        let at_park_id = EntryId::new();
        let mut at_park = entry(at_park_id, channel, agent.clone(), 5, assertion("at park"));
        at_park.sign(&agent_key).unwrap();

        let after_id = EntryId::new();
        let mut after = entry(after_id, channel, agent.clone(), 6, assertion("after park"));
        after.sign(&agent_key).unwrap();

        for e in [genesis, grant, second_device, before, park, at_park, after] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        let grants = view
            .keyring
            .get("worker@agents.junto")
            .expect("agent has a grant");
        assert_eq!(
            grants[0].retired_at,
            Some(Timestamp::from_millis(5)),
            "the grant is retired at the park's own timestamp"
        );
        assert!(
            !view.unverified.contains(&before_id),
            "signed before the park, the grant was still active"
        );
        assert!(
            !view.unverified.contains(&at_park_id),
            "signed at exactly the park's timestamp, active_at is inclusive"
        );
        assert!(
            view.unverified.contains(&after_id),
            "signed after the park, the grant is retired"
        );
    }

    /// `docs/adr/0033` (Task 2) — only the founder may retire a grant,
    /// mirroring the grant rule itself (`project_keyring`'s `MemberAdded`
    /// branch). A `Park` from anyone else targeting a granting entry has no
    /// effect on the keyring.
    #[tokio::test]
    async fn a_non_founder_park_does_not_retire_a_grant() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let agent_key = crate::SigningKey::from_secret_bytes([2; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let agent = Member::agent("Worker", "worker@agents.junto").with_key(agent_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let mut genesis = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        genesis.sign(&founder_key).unwrap();

        let grant_id = EntryId::new();
        let mut grant = entry(
            grant_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: agent.clone(),
            },
        );
        grant.sign(&founder_key).unwrap();

        // The agent — not the founder — tries to park its own grant.
        let non_founder_park = entry(
            EntryId::new(),
            channel,
            agent.clone(),
            3,
            EntryPayload::Park {
                target: grant_id,
                rationale: "not my call".into(),
            },
        );

        let later_id = EntryId::new();
        let mut later = entry(later_id, channel, agent.clone(), 4, assertion("still mine"));
        later.sign(&agent_key).unwrap();

        for e in [genesis, grant, non_founder_park, later] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        let grants = view
            .keyring
            .get("worker@agents.junto")
            .expect("agent has a grant");
        assert_eq!(
            grants[0].retired_at, None,
            "a non-founder park has no authority to retire a grant"
        );
        assert!(!view.unverified.contains(&later_id));
    }

    /// `docs/adr/0033` (Task 2) — several parks can target the same grant;
    /// the *earliest* park's timestamp is the retirement point, never a
    /// later one. This is the discriminating regression test: under a
    /// last-write-wins fold (`retirements.insert` unconditionally,
    /// dropping the `.min()`), `retired_at` would land on the later park
    /// (ts 6) instead of the earlier one (ts 4), and an entry stamped at ts
    /// 5 — after the earlier park but before the later one — would wrongly
    /// still verify.
    ///
    /// The agent carries a *second*, never-parked device so the person as
    /// a whole is never fully revoked (`docs/adr/0035`, Task 3) — with one
    /// grant still active there is no cutoff, which keeps this test
    /// isolated to the verification-layer earliest-wins fold it is named
    /// for, rather than `between_id` moving from `unverified` to
    /// `unrecognized`.
    #[tokio::test]
    async fn earliest_park_wins_when_two_target_the_same_grant() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let agent_key = crate::SigningKey::from_secret_bytes([2; 32]);
        let agent_key2 = crate::SigningKey::from_secret_bytes([3; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let agent = Member::agent("Worker", "worker@agents.junto").with_key(agent_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let mut genesis = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        genesis.sign(&founder_key).unwrap();

        let grant_id = EntryId::new();
        let mut grant = entry(
            grant_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: agent.clone(),
            },
        );
        grant.sign(&founder_key).unwrap();

        // A second device, never parked, so the person keeps an active
        // grant throughout — no revocation cutoff applies to them.
        let mut second_device = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            3,
            EntryPayload::MemberAdded {
                member: agent.clone().with_key(agent_key2.public_key()),
            },
        );
        second_device.sign(&founder_key).unwrap();

        let earlier_park = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            4,
            EntryPayload::Park {
                target: grant_id,
                rationale: "device lost".into(),
            },
        );
        let later_park = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            6,
            EntryPayload::Park {
                target: grant_id,
                rationale: "redundant, filed twice".into(),
            },
        );

        // Stamped between the two parks: unverified iff the earlier (ts 4)
        // one, not the later (ts 6) one, decides the retirement.
        let between_id = EntryId::new();
        let mut between = entry(between_id, channel, agent.clone(), 5, assertion("between"));
        between.sign(&agent_key).unwrap();

        for e in [
            genesis,
            grant,
            second_device,
            earlier_park,
            later_park,
            between,
        ] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        let grants = view
            .keyring
            .get("worker@agents.junto")
            .expect("agent has a grant");
        assert_eq!(
            grants[0].retired_at,
            Some(Timestamp::from_millis(4)),
            "the earlier park decides retirement, not the later one"
        );
        assert!(
            view.unverified.contains(&between_id),
            "stamped after the earlier park, before the later one: unverified under earliest-wins"
        );
    }

    /// `docs/adr/0033` (Task 2) — an email can hold several active grants
    /// (one per enrolled device); an entry verifies if its signature
    /// matches ANY of them, not just the first.
    #[tokio::test]
    async fn an_entry_verifies_against_any_active_grant() {
        let k1 = crate::SigningKey::from_secret_bytes([1; 32]);
        let k2 = crate::SigningKey::from_secret_bytes([2; 32]);
        let dan = Member::human("Dan", "dan@x.com").with_key(k1.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let mut genesis = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        genesis.sign(&k1).unwrap();

        // Dan's own second device, enrolled via a self-authored MemberAdded
        // carrying k2.
        let second_device = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: dan.clone().with_key(k2.public_key()),
            },
        );

        let via_k1_id = EntryId::new();
        let mut via_k1 = entry(
            via_k1_id,
            channel,
            dan.clone(),
            3,
            assertion("from device one"),
        );
        via_k1.sign(&k1).unwrap();

        let via_k2_id = EntryId::new();
        let mut via_k2 = entry(
            via_k2_id,
            channel,
            dan.clone(),
            4,
            assertion("from device two"),
        );
        via_k2.sign(&k2).unwrap();

        for e in [genesis, second_device, via_k1, via_k2] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(!view.unverified.contains(&via_k1_id));
        assert!(!view.unverified.contains(&via_k2_id));
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
    async fn projection_cache_reflects_appends_immediately() {
        // Prime the cache with an empty projection, then append: the next
        // project must see the new entry. If append failed to invalidate, the
        // within-TTL cached empty view would (wrongly) still be served.
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let alice = Member::human("Alice", "alice@example.com");
        assert!(ledger.project(&channel).await.unwrap().entries.is_empty());
        let id = EntryId::new();
        ledger
            .append(entry(id, channel, alice, 1, assertion("hello")))
            .await
            .unwrap();
        let view = ledger.project(&channel).await.unwrap();
        assert!(
            view.entries.iter().any(|e| e.id == id),
            "append invalidates the cached projection"
        );
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
    async fn gate_execution_folds_last_applicable_wins() {
        let dan = Member::human("Dan", "dan@example.com");
        let (mut ledger, channel) = opened_channel(&dan).await;
        let proposal = EntryId::new();
        // A proposal with no GateExecuted yet: not executed.
        ledger
            .append(entry(
                proposal,
                channel,
                dan.clone(),
                1,
                EntryPayload::Proposal {
                    action: "open the PR".into(),
                    rationale: "ready".into(),
                    provenance: vec![],
                    requirement: ApprovalRequirement::Count(1),
                    frame: None,
                    kind: Some("code-pr.open-pr".into()),
                },
            ))
            .await
            .unwrap();
        assert_eq!(
            ledger
                .project(&channel)
                .await
                .unwrap()
                .gate_executed(&proposal),
            None,
            "no GateExecuted yet"
        );

        // A failure, then a successful retry — last-applicable wins.
        for (ts, success, note) in [(2, false, "push denied"), (3, true, "pr #1")] {
            ledger
                .append(entry(
                    EntryId::new(),
                    channel,
                    dan.clone(),
                    ts,
                    EntryPayload::GateExecuted {
                        target: proposal,
                        success,
                        note: note.into(),
                    },
                ))
                .await
                .unwrap();
        }
        assert_eq!(
            ledger
                .project(&channel)
                .await
                .unwrap()
                .gate_executed(&proposal),
            Some(true),
            "the later successful execution supersedes the earlier failure"
        );
    }

    #[tokio::test]
    async fn lineage_edges_project_normalized_by_relation_and_direction() {
        let dan = Member::human("Dan", "dan@example.com");
        let (mut ledger, channel) = opened_channel(&dan).await;
        let parent = ChannelId::new();
        let child = ChannelId::new();
        let target = ChannelId::new();
        let source = ChannelId::new();
        let split_point = EntryId::new();

        for (ts, payload) in [
            (
                1,
                EntryPayload::DivergedFrom {
                    parent,
                    at: Some(split_point),
                },
            ),
            (2, EntryPayload::ChildDiverged { child }),
            (3, EntryPayload::ConvergedInto { target }),
            (4, EntryPayload::ConvergenceReceived { source }),
        ] {
            ledger
                .append(entry(EntryId::new(), channel, dan.clone(), ts, payload))
                .await
                .unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        let edges = &view.lineage;
        assert_eq!(edges.len(), 4);

        // Child side of a divergence: inherits from the parent, carries the point.
        assert!(edges.contains(&LineageEdge {
            relation: LineageRelation::Diverge,
            direction: LineageDirection::Incoming,
            other: parent,
            point: Some(split_point),
        }));
        // Parent side: a child depends on us; no point.
        assert!(edges.contains(&LineageEdge {
            relation: LineageRelation::Diverge,
            direction: LineageDirection::Outgoing,
            other: child,
            point: None,
        }));
        // Source side of a convergence: we feed the target.
        assert!(edges.contains(&LineageEdge {
            relation: LineageRelation::Converge,
            direction: LineageDirection::Outgoing,
            other: target,
            point: None,
        }));
        // Target side: a predecessor's context flows into us.
        assert!(edges.contains(&LineageEdge {
            relation: LineageRelation::Converge,
            direction: LineageDirection::Incoming,
            other: source,
            point: None,
        }));
    }

    #[tokio::test]
    async fn lineage_edges_from_non_members_do_not_project() {
        let dan = Member::human("Dan", "dan@example.com");
        let stranger = Member::agent("Stranger", "stranger@example.com");
        let (mut ledger, channel) = opened_channel(&dan).await;

        ledger
            .append(entry(
                EntryId::new(),
                channel,
                stranger,
                1,
                EntryPayload::ChildDiverged {
                    child: ChannelId::new(),
                },
            ))
            .await
            .unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert!(
            view.lineage.is_empty(),
            "a non-member's lineage edge is unrecognized and does not project"
        );
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
            kind: None,
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

    /// Task 3 (`docs/adr/0035`) — retiring a member's only key must stop
    /// their later entries from *counting*, not just from verifying: an
    /// entry stamped strictly after the retirement of the author's last
    /// active grant is unrecognized, mirroring `KeyGrant::active_at`'s
    /// inclusive boundary — an entry stamped *at exactly* the cutoff still
    /// counts, only strictly-after entries do not.
    #[tokio::test]
    async fn revoked_members_post_cutoff_entries_are_unrecognized() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let agent_key = crate::SigningKey::from_secret_bytes([2; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let agent = Member::agent("Worker", "worker@agents.junto").with_key(agent_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis_entry = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        let grant_id = EntryId::new();
        let grant = entry(
            grant_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: agent.clone(),
            },
        );
        let before_id = EntryId::new();
        let before = entry(
            before_id,
            channel,
            agent.clone(),
            3,
            assertion("before cutoff"),
        );
        let park = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            4,
            EntryPayload::Park {
                target: grant_id,
                rationale: "device lost".into(),
            },
        );
        let at_cutoff_id = EntryId::new();
        let at_cutoff = entry(
            at_cutoff_id,
            channel,
            agent.clone(),
            4,
            assertion("at cutoff"),
        );
        let after_id = EntryId::new();
        let after = entry(
            after_id,
            channel,
            agent.clone(),
            5,
            assertion("after cutoff"),
        );

        for e in [genesis_entry, grant, before, park, at_cutoff, after] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(
            !view.unrecognized.contains(&before_id),
            "stamped before the cutoff, still counts"
        );
        assert!(
            !view.unrecognized.contains(&at_cutoff_id),
            "stamped at exactly the cutoff, the boundary is inclusive — still counts"
        );
        assert!(
            view.unrecognized.contains(&after_id),
            "stamped after the cutoff, no longer counts"
        );
    }

    /// Final fix wave, finding 2 — ADR 0035's "re-admitting a revoked
    /// member" consequence, pinned so a future change to either half
    /// (the cutoff fold or the re-grant path) is deliberate, not
    /// accidental. Composed: revoke a member (every grant retired), an
    /// entry they write in the gap is `unrecognized` — then re-admit the
    /// SAME email with a fresh grant, and that SAME entry (never
    /// rewritten, never re-signed) flips back to recognized, because
    /// `project_unrecognized`'s cutoff fold sees an active grant again
    /// and stops reporting a cutoff at all. This is NOT the per-grant
    /// start bound the branch review considered and rejected — nothing
    /// here gives the re-grant its own lower bound, which is exactly why
    /// the gap entry (stamped well before the re-grant) still counts.
    #[tokio::test]
    async fn re_enrolling_a_revoked_member_restores_their_gap_window_to_recognized() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let agent_key = crate::SigningKey::from_secret_bytes([2; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let agent = Member::agent("Worker", "worker@agents.junto").with_key(agent_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis_entry = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        let grant_id = EntryId::new();
        let grant = entry(
            grant_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: agent.clone(),
            },
        );
        let park = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            3,
            EntryPayload::Park {
                target: grant_id,
                rationale: "device lost".into(),
            },
        );
        let gap_id = EntryId::new();
        let gap = entry(
            gap_id,
            channel,
            agent.clone(),
            4,
            assertion("written after revocation, before re-admission"),
        );

        for e in [genesis_entry, grant, park, gap] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(
            view.unrecognized.contains(&gap_id),
            "revoked and not yet re-admitted: the gap-window entry is unrecognized"
        );

        // Re-admit the SAME email — `Host::add_member`'s deliberate
        // re-grant of a previously retired key.
        let regrant = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            5,
            EntryPayload::MemberAdded { member: agent },
        );
        ledger.append(regrant).await.unwrap();

        let view = ledger.project(&channel).await.unwrap();
        assert!(
            !view.unrecognized.contains(&gap_id),
            "re-admission restores the SAME gap-window entry to recognized — ADR 0035's \
             documented consequence, not a bug: recognition is derived from the live \
             keyring, not stamped on the entry, so the cutoff it was written under no \
             longer exists once any grant for the email is active again"
        );
    }

    /// Task 3 (`docs/adr/0035`) — the strongest guard: revoking a member
    /// must not rewrite the history they already made. An assertion they
    /// wrote before the cutoff keeps its standing, and a ratification they
    /// gave before the cutoff still resolves its target, even though by the
    /// time the projection is built the author is fully revoked.
    #[tokio::test]
    async fn a_revoked_members_pre_cutoff_contributions_still_count() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let agent_key = crate::SigningKey::from_secret_bytes([2; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let agent = Member::agent("Worker", "worker@agents.junto").with_key(agent_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis_entry = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        let grant_id = EntryId::new();
        let grant = entry(
            grant_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: agent.clone(),
            },
        );
        let agent_claim_id = EntryId::new();
        let agent_claim = entry(
            agent_claim_id,
            channel,
            agent.clone(),
            3,
            assertion("agent's own claim"),
        );
        let claim_id = EntryId::new();
        let claim = entry(
            claim_id,
            channel,
            dan.clone(),
            4,
            assertion("needs ratification"),
        );
        let ratification_id = EntryId::new();
        let ratification = entry(
            ratification_id,
            channel,
            agent.clone(),
            5,
            EntryPayload::Ratification {
                target: claim_id,
                rationale: "looks right".into(),
            },
        );
        let park = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            6,
            EntryPayload::Park {
                target: grant_id,
                rationale: "offboarded".into(),
            },
        );

        for e in [genesis_entry, grant, agent_claim, claim, ratification, park] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(
            !view.unrecognized.contains(&agent_claim_id),
            "an assertion made before the cutoff keeps counting"
        );
        assert_eq!(
            view.standing(&agent_claim_id),
            Some(Standing::Provisional),
            "and keeps its standing"
        );
        assert!(
            !view.unrecognized.contains(&ratification_id),
            "a ratification given before the cutoff still counts"
        );
        assert_eq!(
            view.standing(&claim_id),
            Some(Standing::Ratified),
            "and still resolves its target — revocation did not rewrite history"
        );
    }

    /// Task 3 (`docs/adr/0035`) — two grants, one retired: retiring one
    /// device must not offboard the person. With an active grant remaining
    /// there is no cutoff, so entries stamped long after the partial
    /// retirement still count.
    #[tokio::test]
    async fn a_partially_retired_member_is_not_revoked() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let k1 = crate::SigningKey::from_secret_bytes([2; 32]);
        let k2 = crate::SigningKey::from_secret_bytes([3; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis_entry = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        let member = Member::human("Mia", "mia@example.com");
        let device1_id = EntryId::new();
        let device1 = entry(
            device1_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: member.clone().with_key(k1.public_key()),
            },
        );
        let device2 = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            3,
            EntryPayload::MemberAdded {
                member: member.clone().with_key(k2.public_key()),
            },
        );
        let park_device1 = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            4,
            EntryPayload::Park {
                target: device1_id,
                rationale: "lost device 1".into(),
            },
        );
        let later_id = EntryId::new();
        let later = entry(
            later_id,
            channel,
            member.clone(),
            100,
            assertion("long after the partial retirement"),
        );

        for e in [genesis_entry, device1, device2, park_device1, later] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(
            !view.unrecognized.contains(&later_id),
            "one retired device must not offboard the person"
        );
    }

    /// Task 9c (`docs/adr/0035`) — the defect this fix closes. Alice holds
    /// two grants: she retires grant B (enrolled *second*, at ts 3) at T1
    /// while grant A (enrolled *first*, at ts 2) is still active — no
    /// cutoff yet, matching `a_partially_retired_member_is_not_revoked` —
    /// and keeps working from grant A until it, too, is retired at T2
    /// (T1 < T2). Grant A sits *first* in the keyring's per-email
    /// `Vec<KeyGrant>` (canonical entry order, `docs/adr/0033`) yet
    /// carries the *later* retirement — deliberately: a fold that
    /// regressed to "whichever grant the loop visits last wins" instead
    /// of the true maximum would compute grant B's earlier T1 here (the
    /// last-visited grant), the same wrong answer the original
    /// earliest-wins defect gave, and this test would still catch it.
    /// Under the defective `.min()` fold the cutoff would land on T1 and
    /// retroactively unrecognize everything she wrote between T1 and T2;
    /// the corrected latest-retirement rule must not.
    #[tokio::test]
    async fn entries_between_two_distinct_grant_retirements_still_count() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let k1 = crate::SigningKey::from_secret_bytes([2; 32]);
        let k2 = crate::SigningKey::from_secret_bytes([3; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis_entry = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        let member = Member::human("Alice", "alice@example.com");
        let device_a_id = EntryId::new();
        let device_a = entry(
            device_a_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: member.clone().with_key(k1.public_key()),
            },
        );
        let device_b_id = EntryId::new();
        let device_b = entry(
            device_b_id,
            channel,
            dan.clone(),
            3,
            EntryPayload::MemberAdded {
                member: member.clone().with_key(k2.public_key()),
            },
        );
        let park_device_b = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            4,
            EntryPayload::Park {
                target: device_b_id,
                rationale: "grant B retired first, earlier".into(),
            },
        );
        let between_id = EntryId::new();
        let between = entry(
            between_id,
            channel,
            member.clone(),
            5,
            assertion("written from grant A, between the two retirements"),
        );
        let park_device_a = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            6,
            EntryPayload::Park {
                target: device_a_id,
                rationale: "grant A retired second, later".into(),
            },
        );

        for e in [
            genesis_entry,
            device_a,
            device_b,
            park_device_b,
            between,
            park_device_a,
        ] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(
            !view.unrecognized.contains(&between_id),
            "written while grant A was still active — grant B's earlier retirement must not \
             retroactively unrecognize it"
        );
    }

    /// Task 9c (`docs/adr/0035`) — companion to
    /// `entries_between_two_distinct_grant_retirements_still_count`: once
    /// the *later* of the two grants (T2) is also retired, an entry stamped
    /// after T2 is unrecognized. This is the half of the rule the old
    /// `.min()` fold already got right by accident (it happened to also
    /// treat post-T2 entries as unrecognized) — kept as an explicit
    /// regression guard against a fix that swings too far the other way and
    /// stops enforcing any cutoff at all. Uses the same non-degenerate
    /// ordering as its companion — grant A is enrolled first but retired
    /// second (later) — so the maximum is not merely whichever grant the
    /// fold visits last.
    #[tokio::test]
    async fn an_entry_after_the_latest_of_two_retirements_is_unrecognized() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let k1 = crate::SigningKey::from_secret_bytes([2; 32]);
        let k2 = crate::SigningKey::from_secret_bytes([3; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis_entry = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        let member = Member::human("Alice", "alice@example.com");
        let device_a_id = EntryId::new();
        let device_a = entry(
            device_a_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: member.clone().with_key(k1.public_key()),
            },
        );
        let device_b_id = EntryId::new();
        let device_b = entry(
            device_b_id,
            channel,
            dan.clone(),
            3,
            EntryPayload::MemberAdded {
                member: member.clone().with_key(k2.public_key()),
            },
        );
        let park_device_b = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            4,
            EntryPayload::Park {
                target: device_b_id,
                rationale: "grant B retired first, earlier".into(),
            },
        );
        let park_device_a = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            6,
            EntryPayload::Park {
                target: device_a_id,
                rationale: "grant A retired second, later".into(),
            },
        );
        let after_id = EntryId::new();
        let after = entry(
            after_id,
            channel,
            member.clone(),
            7,
            assertion("written after both grants are retired"),
        );

        for e in [
            genesis_entry,
            device_a,
            device_b,
            park_device_b,
            park_device_a,
            after,
        ] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(
            view.unrecognized.contains(&after_id),
            "both grants are retired and this entry is stamped after the later of the two"
        );
    }

    /// Task 9c (`docs/adr/0035`) — the at-cutoff boundary must hold against
    /// the *later* of two retirements, not the earlier one: an entry
    /// stamped exactly at T2 still counts, mirroring
    /// `revoked_members_post_cutoff_entries_are_unrecognized`'s
    /// single-grant boundary case but exercised across two grants so a
    /// regression back to the earlier retirement as the boundary would be
    /// caught here even though it is the same email. Same non-degenerate
    /// ordering as the other two-grant fixtures: grant A is enrolled first
    /// but retired second (later).
    #[tokio::test]
    async fn at_cutoff_boundary_holds_against_the_latest_of_two_retirements() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let k1 = crate::SigningKey::from_secret_bytes([2; 32]);
        let k2 = crate::SigningKey::from_secret_bytes([3; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis_entry = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        let member = Member::human("Alice", "alice@example.com");
        let device_a_id = EntryId::new();
        let device_a = entry(
            device_a_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: member.clone().with_key(k1.public_key()),
            },
        );
        let device_b_id = EntryId::new();
        let device_b = entry(
            device_b_id,
            channel,
            dan.clone(),
            3,
            EntryPayload::MemberAdded {
                member: member.clone().with_key(k2.public_key()),
            },
        );
        let park_device_b = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            4,
            EntryPayload::Park {
                target: device_b_id,
                rationale: "grant B retired first, earlier".into(),
            },
        );
        let park_device_a = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            6,
            EntryPayload::Park {
                target: device_a_id,
                rationale: "grant A retired second, later".into(),
            },
        );
        let at_cutoff_id = EntryId::new();
        let at_cutoff = entry(
            at_cutoff_id,
            channel,
            member.clone(),
            6,
            assertion("stamped exactly at the later retirement"),
        );

        for e in [
            genesis_entry,
            device_a,
            device_b,
            park_device_b,
            park_device_a,
            at_cutoff,
        ] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(
            !view.unrecognized.contains(&at_cutoff_id),
            "stamped at exactly the later retirement — the boundary is inclusive, still counts"
        );
    }

    /// Task 9c (`docs/adr/0035`) — guards the half of the fold that was
    /// already right: with two grants and only one retired, there is still
    /// no cutoff at all, not merely a cutoff pinned to the active grant.
    /// Distinct from `a_partially_retired_member_is_not_revoked` (which
    /// this complements) only in being written explicitly for Task 9c's
    /// fold change, so a future edit to the "any grant active ⇒ no cutoff"
    /// branch is caught by more than one test.
    #[tokio::test]
    async fn partial_retirement_across_two_grants_still_yields_no_cutoff() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let k1 = crate::SigningKey::from_secret_bytes([2; 32]);
        let k2 = crate::SigningKey::from_secret_bytes([3; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis_entry = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        let member = Member::human("Alice", "alice@example.com");
        let device_a_id = EntryId::new();
        let device_a = entry(
            device_a_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: member.clone().with_key(k1.public_key()),
            },
        );
        let device_b = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            3,
            EntryPayload::MemberAdded {
                member: member.clone().with_key(k2.public_key()),
            },
        );
        let park_device_a = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            4,
            EntryPayload::Park {
                target: device_a_id,
                rationale: "laptop retired".into(),
            },
        );
        let after_id = EntryId::new();
        let after = entry(
            after_id,
            channel,
            member.clone(),
            5,
            assertion("written after the laptop's retirement, desktop still active"),
        );

        for e in [genesis_entry, device_a, device_b, park_device_a, after] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(
            !view.unrecognized.contains(&after_id),
            "one active grant among two must still mean no cutoff"
        );
    }

    /// Task 3 (`docs/adr/0035`) — regression guard on `docs/adr/0017` for
    /// everyone who is *not* revoked: with no founder-authored `Park`
    /// against any of their grants, recognition stays purely set-based —
    /// membership, not timing, still decides. Neither clock skew before the
    /// grant nor a much later timestamp introduces a cutoff.
    #[tokio::test]
    async fn an_unrevoked_members_recognition_is_still_set_based() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let agent_key = crate::SigningKey::from_secret_bytes([2; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let agent = Member::agent("Worker", "worker@agents.junto").with_key(agent_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let early_id = EntryId::new();
        let early = entry(
            early_id,
            channel,
            agent.clone(),
            1,
            assertion("written before the grant"),
        );
        let genesis_entry = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            2,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        let grant = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            3,
            EntryPayload::MemberAdded {
                member: agent.clone(),
            },
        );
        let late_id = EntryId::new();
        let late = entry(
            late_id,
            channel,
            agent.clone(),
            1000,
            assertion("much later, still no cutoff"),
        );

        for e in [early, genesis_entry, grant, late] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(
            !view.unrecognized.contains(&early_id),
            "clock skew before the grant still counts (docs/adr/0017)"
        );
        assert!(
            !view.unrecognized.contains(&late_id),
            "an active, never-parked grant imposes no cutoff"
        );
    }

    /// Task 3 (`docs/adr/0035`) — the rejected alternative, pinned so
    /// nobody "fixes" this later: recognition is party-set membership, so
    /// removing a revoked member from the party would mark every entry
    /// they ever authored unrecognized and erase their history from every
    /// downstream projection. Revocation retires keys; it never touches
    /// the roster.
    #[tokio::test]
    async fn a_revoked_member_remains_in_the_party() {
        let founder_key = crate::SigningKey::from_secret_bytes([1; 32]);
        let agent_key = crate::SigningKey::from_secret_bytes([2; 32]);
        let dan = Member::human("Dan", "dan@example.com").with_key(founder_key.public_key());
        let agent = Member::agent("Worker", "worker@agents.junto").with_key(agent_key.public_key());
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();

        let genesis_entry = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            1,
            EntryPayload::ChannelOpened { name: "ch".into() },
        );
        let grant_id = EntryId::new();
        let grant = entry(
            grant_id,
            channel,
            dan.clone(),
            2,
            EntryPayload::MemberAdded {
                member: agent.clone(),
            },
        );
        let park = entry(
            EntryId::new(),
            channel,
            dan.clone(),
            3,
            EntryPayload::Park {
                target: grant_id,
                rationale: "offboarded".into(),
            },
        );

        for e in [genesis_entry, grant, park] {
            ledger.append(e).await.unwrap();
        }

        let view = ledger.project(&channel).await.unwrap();
        assert!(
            view.party.iter().any(|m| m.email == "worker@agents.junto"),
            "revocation retires keys; it must never remove the member from the party"
        );
    }
}
