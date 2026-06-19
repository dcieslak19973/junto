//! The singleton host (`docs/adr/0015`): one process per machine/user serving
//! every **registered home substrate**.
//!
//! The machine-local registry (`<junto-home>/substrates.toml`) only says which
//! repos hold records on this machine — channel identity and name → id
//! bindings live in the substrates themselves (`docs/adr/0014`), so losing the
//! registry loses nothing durable.
//!
//! Channel addressing (`docs/adr/0014`/`0016`): a channel's id is minted at
//! open time; its name is a label bound by the `ChannelOpened` genesis entry,
//! unique only within its home substrate. The host resolves a bare name across
//! all registered substrates — ambiguity is an error asking for
//! qualification — and a raw id always resolves.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use junto_kernel::{
    ChannelId, ChannelView, EntryId, EntryPayload, GateStatus, Ledger, LedgerEntry, Member,
    Standing, SubstrateProvider, Timestamp,
};
use junto_substrate_git::GitRefsSubstrate;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// One open ledger, shared by the MCP tools and the web routes. The mutex
/// serializes appends (read-modify-write on the underlying git ref).
pub type SharedLedger = Arc<Mutex<Ledger<GitRefsSubstrate>>>;

/// The user's junto directory: `$JUNTO_HOME` if set (tests, unusual setups),
/// else `~/.junto`.
pub fn junto_home() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("JUNTO_HOME") {
        return Ok(PathBuf::from(home));
    }
    std::env::home_dir()
        .map(|home| home.join(".junto"))
        .context("no home directory; set JUNTO_HOME")
}

/// The serialized shape of `<junto-home>/substrates.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    /// Repos holding `refs/junto/*` records on this machine.
    #[serde(default)]
    substrates: Vec<PathBuf>,
}

/// The registry file's path under a junto home.
fn registry_path(junto_home: &Path) -> PathBuf {
    junto_home.join("substrates.toml")
}

/// The registered substrate repos, in registration order. A missing registry
/// file is an empty registry, not an error.
pub fn registered_substrates(junto_home: &Path) -> Result<Vec<PathBuf>> {
    let path = registry_path(junto_home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let file: RegistryFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(file.substrates)
}

/// Register `repo` as a home substrate (idempotent; canonicalizes the path).
pub fn register_substrate(junto_home: &Path, repo: &Path) -> Result<()> {
    let repo = dunce::canonicalize(repo)
        .with_context(|| format!("substrate repo {} not found", repo.display()))?;
    let mut substrates = registered_substrates(junto_home)?;
    if substrates.contains(&repo) {
        return Ok(());
    }
    substrates.push(repo);
    std::fs::create_dir_all(junto_home)
        .with_context(|| format!("creating {}", junto_home.display()))?;
    let file = RegistryFile { substrates };
    std::fs::write(
        registry_path(junto_home),
        toml::to_string_pretty(&file).context("serializing substrate registry")?,
    )
    .with_context(|| format!("writing {}", registry_path(junto_home).display()))?;
    Ok(())
}

/// The machine user's identity from a repo's git config (repo-local first,
/// global fallback — git's own precedence) — the default author for
/// human-initiated acts (`junto open`, the web verification forms). This is
/// deliberately *not* identity management: identity stays claimed
/// (`docs/adr/0012`); git config is just the sensible machine-user default.
pub fn git_user(repo: &Path) -> Result<Member> {
    let get = |key: &str| -> Result<String> {
        let mut command = std::process::Command::new("git");
        command.args(["-C", &repo.display().to_string(), "config", key]);
        // Terminal-less: no flashed console window (runs on every human act).
        crate::launch::no_console_window(&mut command);
        let out = command.output().context("running git config")?;
        if !out.status.success() {
            bail!("git config {key} is unset");
        }
        Ok(String::from_utf8(out.stdout)
            .context("git config output not utf-8")?
            .trim()
            .to_string())
    };
    Ok(Member::human(get("user.name")?, get("user.email")?))
}

/// Where a [`Host`] finds its substrates.
enum Substrates {
    /// The machine registry under this junto home, re-read on each use so
    /// `junto init` in another process shows up without a host restart.
    Registry(PathBuf),
    /// A fixed set — `junto serve --repo <path>` (single-substrate dev mode)
    /// and tests.
    Fixed(Vec<PathBuf>),
}

/// One channel as discovery sees it (`list_channels`, the index page).
#[derive(Debug, Clone)]
pub struct ChannelSummary {
    pub id: ChannelId,
    /// `None` for a channel with no `ChannelOpened` genesis (pre-0014 records).
    pub name: Option<String>,
    /// The home substrate repo holding this channel's record.
    pub substrate: PathBuf,
    pub entry_count: usize,
    pub last_activity: Option<Timestamp>,
    /// When the channel began — its first entry (genesis). Places the
    /// channel's divergence point on the lineage strip's time axis.
    pub first_activity: Option<Timestamp>,
    /// Pending proposals — the "needs your attention" signal.
    pub open_gates: usize,
    /// The Party's size (`docs/adr/0017`); 0 for pre-genesis channels.
    pub members: usize,
    /// A one-line preview of the most recent entry — the resumption cue on
    /// the index ("where was I?").
    pub latest: Option<String>,
    /// Whether the channel is closed (`docs/adr/0022`) — out of the working
    /// set; the surfaces demote it.
    pub closed: bool,
    /// The channel's milestone entries (recent-most last) — settled decisions,
    /// attached artifacts, and open gates — for plotting as nodes along the
    /// channel's track on the lineage strip. Curated (not every entry) so a
    /// busy channel's line stays legible.
    pub milestones: Vec<Milestone>,
    /// The channel this one diverged from (`docs/adr/0027`), if any — the
    /// lineage strip attaches its branch to the parent's track here, instead
    /// of the baseline.
    pub parent: Option<ChannelId>,
    /// The channel this one converged into (`docs/adr/0027`), if any — the
    /// strip draws the merge-back into that target's track.
    pub converged_into: Option<ChannelId>,
}

/// One notable event on a channel's track — a settled decision, an attached
/// artifact, or an open gate — plotted as a node on the lineage strip at the
/// time it happened.
#[derive(Debug, Clone)]
pub struct Milestone {
    /// When it happened — places the node along the strip's time axis.
    pub at: Timestamp,
    /// What kind of node to draw.
    pub kind: MilestoneKind,
    /// A short label for the node's tooltip.
    pub label: String,
}

/// The kind of a [`Milestone`] — drives the node's shape/colour on the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MilestoneKind {
    /// A settled decision (a ratified assertion, or a correction that is the
    /// live text of settled territory).
    Decision,
    /// An attached artifact (`docs/adr/0020`).
    Artifact,
    /// An open gate (a pending proposal) awaiting a member.
    Gate,
}

/// What kind of act an [`AttentionItem`] awaits (`docs/attention.md`:
/// gates first — they block the proposer — then verification debt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionKind {
    /// A pending proposal awaiting approve/reject; its author is blocked.
    Gate,
    /// A provisional assertion awaiting ratify/park.
    Verification,
}

/// One act awaiting a member on the focus board.
#[derive(Debug, Clone)]
pub struct AttentionItem {
    pub kind: AttentionKind,
    /// The pending proposal or provisional assertion itself.
    pub entry: LedgerEntry,
}

/// One inquiry's group on the focus board — items are never interleaved
/// across inquiries (`docs/attention.md`: switching is the cost).
#[derive(Debug, Clone)]
pub struct AttentionGroup {
    pub channel: ChannelId,
    pub name: Option<String>,
    /// Gates first, then verifications; oldest first within each (longest
    /// waiting at the top).
    pub items: Vec<AttentionItem>,
}

impl AttentionGroup {
    /// Whether this inquiry has a blocked proposer (its urgency tier).
    pub fn has_gates(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.kind == AttentionKind::Gate)
    }
}

/// The result of resolving a user-supplied channel reference.
pub enum Resolution {
    /// Exactly one channel matched, in its home substrate's ledger.
    Resolved {
        /// The home substrate repo (e.g. for deriving a default author from
        /// its git config).
        substrate: PathBuf,
        ledger: SharedLedger,
        id: ChannelId,
    },
    /// No registered substrate has a channel by that name or id.
    NotFound,
    /// The name exists in more than one substrate; the caller must qualify.
    Ambiguous(Vec<PathBuf>),
}

/// The singleton host's shared state.
pub struct Host {
    substrates: Substrates,
    /// Where the machine-local member-code store lives (`docs/adr/0017`);
    /// `None` means the user's junto home, resolved per use.
    member_home_override: Option<PathBuf>,
    /// Ledgers opened so far, keyed by substrate repo path — cached so each
    /// repo has one append-serializing mutex for the host's lifetime.
    ledgers: Mutex<HashMap<PathBuf, SharedLedger>>,
    /// In-memory live-progress feeds for running Agent Sessions
    /// (`docs/adr/0023`) — ephemeral, never part of the record.
    live: crate::launch::LiveSessions,
}

impl Host {
    /// A host over the machine registry under `junto_home` (`docs/adr/0015`).
    pub fn from_registry(junto_home: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            substrates: Substrates::Registry(junto_home),
            member_home_override: None,
            ledgers: Mutex::new(HashMap::new()),
            live: crate::launch::LiveSessions::default(),
        })
    }

    /// A host over a fixed substrate set (single-repo dev mode, tests).
    pub fn fixed(repos: Vec<PathBuf>) -> Arc<Self> {
        Self::fixed_with_member_home(repos, None)
    }

    /// [`Host::fixed`] with an explicit member-code store location, so tests
    /// never touch the real `~/.junto`.
    pub fn fixed_with_member_home(repos: Vec<PathBuf>, member_home: Option<PathBuf>) -> Arc<Self> {
        // Canonicalize up front so path equality (e.g. open_channel's
        // membership check) is not defeated by symlinks or Windows' \\?\
        // prefix; a path that doesn't resolve is kept as-is and will fail
        // loudly when first used.
        let repos = repos
            .into_iter()
            .map(|repo| dunce::canonicalize(&repo).unwrap_or(repo))
            .collect();
        Arc::new(Self {
            substrates: Substrates::Fixed(repos),
            member_home_override: member_home,
            ledgers: Mutex::new(HashMap::new()),
            live: crate::launch::LiveSessions::default(),
        })
    }

    /// The in-memory live-progress feeds for running Agent Sessions
    /// (`docs/adr/0023`). The web SSE endpoint subscribes; a launched turn
    /// publishes. Ephemeral — never the durable record.
    pub fn live(&self) -> &crate::launch::LiveSessions {
        &self.live
    }

    /// Where this host's member-code store lives (`docs/adr/0017`): the
    /// machine registry's junto home, unless overridden (tests).
    fn member_home(&self) -> Result<PathBuf> {
        if let Some(home) = &self.member_home_override {
            return Ok(home.clone());
        }
        match &self.substrates {
            Substrates::Registry(junto_home) => Ok(junto_home.clone()),
            Substrates::Fixed(_) => junto_home(),
        }
    }

    /// The current substrate repos this host serves.
    pub fn substrate_paths(&self) -> Result<Vec<PathBuf>> {
        match &self.substrates {
            Substrates::Registry(junto_home) => registered_substrates(junto_home),
            Substrates::Fixed(repos) => Ok(repos.clone()),
        }
    }

    /// The (cached) ledger over one substrate repo.
    pub async fn ledger_for(&self, repo: &Path) -> Result<SharedLedger> {
        let repo = dunce::canonicalize(repo)
            .with_context(|| format!("substrate repo {} not found", repo.display()))?;
        let mut ledgers = self.ledgers.lock().await;
        Ok(ledgers
            .entry(repo.clone())
            .or_insert_with(|| Arc::new(Mutex::new(Ledger::new(GitRefsSubstrate::open(repo)))))
            .clone())
    }

    /// One projection sweep serving everything the index page needs: channel
    /// summaries *and* the focus board's attention groups (`docs/attention.md`:
    /// every act awaiting a member, grouped by inquiry — gate-bearing inquiries
    /// first, recency within a tier; within a group gates before verifications,
    /// oldest first). Projection is the expensive step (git reads per channel),
    /// so the page must pay for it once, not once per concern.
    pub async fn overview(&self) -> Result<(Vec<ChannelSummary>, Vec<AttentionGroup>)> {
        let mut summaries = Vec::new();
        let mut groups = Vec::new();
        for repo in self.substrate_paths()? {
            let ledger = self.ledger_for(&repo).await?;
            let guard = ledger.lock().await;
            for id in guard.substrate().channels().await? {
                let view = guard.project(&id).await?;
                // A closed channel demands no attention (docs/adr/0022) —
                // its summary still lists, demoted, for the archive view.
                if !view.closed {
                    let group = attention_for_view(&id, &view);
                    if !group.items.is_empty() {
                        groups.push(group);
                    }
                }
                summaries.push(summarize(&id, &view, &repo));
            }
        }
        // Urgency tiers: gate-bearing inquiries first; recency within a tier
        // (the inquiry whose need arose latest leads, matching resumption).
        groups.sort_by_key(|group| {
            let latest = group.items.iter().map(|item| item.entry.timestamp).max();
            (
                std::cmp::Reverse(group.has_gates()),
                std::cmp::Reverse(latest),
            )
        });
        Ok((summaries, groups))
    }

    /// Every channel across every served substrate, projected into summaries.
    pub async fn inventory(&self) -> Result<Vec<ChannelSummary>> {
        Ok(self.overview().await?.0)
    }

    /// Resolve a channel reference — a name bound by a genesis entry, or a raw
    /// channel id — to its home substrate and id (`docs/adr/0014`).
    pub async fn resolve(&self, channel: &str) -> Result<Resolution> {
        // A raw id resolves directly: ids are globally unique, so the first
        // substrate containing it is *the* substrate.
        if let Ok(id) = channel.parse::<ChannelId>() {
            for repo in self.substrate_paths()? {
                let ledger = self.ledger_for(&repo).await?;
                let known = ledger.lock().await.substrate().channels().await?;
                if known.contains(&id) {
                    return Ok(Resolution::Resolved {
                        substrate: repo,
                        ledger,
                        id,
                    });
                }
            }
            return Ok(Resolution::NotFound);
        }

        let mut matches = Vec::new();
        for summary in self.inventory().await? {
            if summary.name.as_deref() == Some(channel) {
                matches.push(summary);
            }
        }
        match matches.len() {
            0 => Ok(Resolution::NotFound),
            1 => {
                let hit = matches.remove(0);
                let ledger = self.ledger_for(&hit.substrate).await?;
                Ok(Resolution::Resolved {
                    substrate: hit.substrate,
                    ledger,
                    id: hit.id,
                })
            }
            _ => Ok(Resolution::Ambiguous(
                matches.into_iter().map(|hit| hit.substrate).collect(),
            )),
        }
    }

    /// Open a channel (`docs/adr/0014`/`0016`): mint its id (or accept a
    /// declared one — the grandfathering path for pre-0014 records), enforce
    /// name uniqueness within the home substrate, and append the
    /// `ChannelOpened` genesis entry. The opener is the **founding member**
    /// (`docs/adr/0017`), so their member code is minted alongside.
    ///
    /// `repo`: the home substrate; may be omitted when the host serves exactly
    /// one.
    pub async fn open_channel(
        &self,
        repo: Option<&Path>,
        name: &str,
        opened_by: Member,
        declared_id: Option<ChannelId>,
    ) -> Result<OpenedChannel> {
        if name.trim().is_empty() {
            bail!("channel name must not be empty");
        }
        if name.parse::<ChannelId>().is_ok() {
            bail!("channel name must not look like a channel id");
        }

        let substrates = self.substrate_paths()?;
        let repo = match (repo, substrates.as_slice()) {
            (Some(repo), _) => {
                let repo = dunce::canonicalize(repo)
                    .with_context(|| format!("substrate repo {} not found", repo.display()))?;
                if !substrates.contains(&repo) {
                    bail!(
                        "{} is not a registered home substrate (run `junto init` there first)",
                        repo.display()
                    );
                }
                repo
            }
            (None, [only]) => only.clone(),
            (None, []) => bail!("no home substrates registered (run `junto init` in a repo first)"),
            (None, _) => bail!(
                "several home substrates are registered; say which one should hold this channel"
            ),
        };

        let ledger = self.ledger_for(&repo).await?;
        // Hold the ledger lock across the uniqueness check *and* the append so
        // two concurrent opens of the same name cannot both pass the check.
        let mut guard = ledger.lock().await;
        for id in guard.substrate().channels().await? {
            let view = guard.project(&id).await?;
            if view.name.as_deref() == Some(name) {
                bail!(
                    "channel '{name}' already exists in {} (id {id})",
                    repo.display()
                );
            }
            if declared_id == Some(id) && view.name.is_some() {
                bail!("channel {id} already has a genesis naming it");
            }
        }

        let id = declared_id.unwrap_or_default();
        guard
            .append(LedgerEntry {
                id: EntryId::new(),
                channel: id,
                author: opened_by.clone(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::ChannelOpened {
                    name: name.to_string(),
                },
            })
            .await?;
        // The opener is the founding member; mint their code so the founder
        // can write through the code-checked surfaces (`docs/adr/0017`).
        let founder_code = crate::members::mint(&self.member_home()?, &opened_by)?;
        Ok(OpenedChannel { id, founder_code })
    }

    /// Grant channel membership (`docs/adr/0017`): append a founder-authored
    /// `MemberAdded` entry and mint the new member's machine-local code.
    ///
    /// Only the **founding member** (the genesis author) may grant — that is a
    /// roster rule checked here; *authenticating* `granted_by` (their member
    /// code) is the calling surface's concern: the MCP tool requires it, the
    /// CLI does not (whoever can run commands on this machine can edit the
    /// code store anyway).
    pub async fn add_member(
        &self,
        channel: &str,
        granted_by: &Member,
        member: Member,
    ) -> Result<crate::members::Minted> {
        let resolution = self.resolve(channel).await?;
        let (ledger, id) = match resolution {
            Resolution::Resolved { ledger, id, .. } => (ledger, id),
            Resolution::NotFound => bail!("no channel '{channel}' in any registered substrate"),
            Resolution::Ambiguous(substrates) => bail!(
                "channel name '{channel}' exists in several substrates ({substrates:?}); \
                 address it by id"
            ),
        };
        let mut guard = ledger.lock().await;
        let view = guard.project(&id).await?;
        let Some(founder) = view.party.first() else {
            bail!(
                "channel '{channel}' has no genesis, so it has no founding member to grant \
                 membership (membership is not enforced on pre-genesis channels)"
            );
        };
        if founder.email != granted_by.email {
            bail!(
                "only the founding member ({} <{}>) can grant membership in '{channel}' \
                 (docs/adr/0017)",
                founder.display_name,
                founder.email
            );
        }

        // Re-granting an existing member (the founder included) is a no-op on
        // the roster — skip the entry, just make sure their code exists.
        if view.party.iter().any(|m| m.email == member.email) {
            return crate::members::mint(&self.member_home()?, &member);
        }

        guard
            .append(LedgerEntry {
                id: EntryId::new(),
                channel: id,
                author: granted_by.clone(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::MemberAdded {
                    member: member.clone(),
                },
            })
            .await?;
        crate::members::mint(&self.member_home()?, &member)
    }

    /// **Diverge** a child channel from a parent (`docs/adr/0027`): open the
    /// child *in the parent's home substrate* (the diverger founds it), then
    /// record the divergence edge as a pair — `DivergedFrom` in the child and
    /// `ChildDiverged` in the parent. The diverger must be a member of the
    /// parent (to author the parent-side entry); the parent flows on.
    ///
    /// Because the child opens in the parent's substrate, both writes are local
    /// here, so there is no far side to enqueue (`docs/adr/0028` only bites on
    /// cross-substrate convergence). Returns the opened child.
    pub async fn diverge(
        &self,
        parent: &str,
        child_name: &str,
        at: Option<EntryId>,
        diverger: Member,
        code: Option<&str>,
    ) -> Result<OpenedChannel> {
        let (parent_substrate, parent_ledger, parent_id) = self.resolve_for_write(parent).await?;
        // The diverger must be a member of the parent to author its side.
        {
            let guard = parent_ledger.lock().await;
            let view = guard.project(&parent_id).await?;
            self.authorize_write(&view, &diverger, code)?;
        }

        // Open the child beside its parent; the diverger is its founder.
        let child = self
            .open_channel(Some(&parent_substrate), child_name, diverger.clone(), None)
            .await?;

        // The child and parent share a substrate (one ledger Arc holds both).
        let mut guard = parent_ledger.lock().await;
        guard
            .append(LedgerEntry {
                id: EntryId::new(),
                channel: child.id,
                author: diverger.clone(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::DivergedFrom {
                    parent: parent_id,
                    at,
                },
            })
            .await?;
        guard
            .append(LedgerEntry {
                id: EntryId::new(),
                channel: parent_id,
                author: diverger,
                timestamp: Timestamp::now(),
                payload: EntryPayload::ChildDiverged { child: child.id },
            })
            .await?;
        Ok(child)
    }

    /// **Converge** a source channel into a target (`docs/adr/0027`): record the
    /// convergence edge as a pair — `ConvergedInto` in the source and
    /// `ConvergenceReceived` in the target — and **close the source**.
    ///
    /// Refuses while the source has any **open gate** (a `Pending` proposal):
    /// each must be decided or re-proposed into the target first — honest
    /// disposal at convergence (`docs/attention.md`). The converger must be a
    /// member of *both* channels. `target` must already exist (converge never
    /// creates a channel); the two-into-a-continuation case is two converges
    /// into one opened continuation.
    ///
    /// v1 writes the target side directly (both substrates registered here);
    /// the eventually-consistent far-side queue (`docs/adr/0028`) lands in a
    /// later slice.
    pub async fn converge(
        &self,
        source: &str,
        target: &str,
        rationale: &str,
        converger: Member,
        code: Option<&str>,
    ) -> Result<()> {
        let (_src_substrate, src_ledger, src_id) = self.resolve_for_write(source).await?;
        let (tgt_id, tgt_ledger) = self.resolve_target(target).await?;
        if src_id == tgt_id {
            bail!("a channel cannot converge into itself");
        }

        // The converger must be a member of the source; the source must have no
        // dangling open gate.
        {
            let guard = src_ledger.lock().await;
            let view = guard.project(&src_id).await?;
            self.authorize_write(&view, &converger, code)?;
            let open = view
                .gate_status
                .values()
                .filter(|status| **status == GateStatus::Pending)
                .count();
            if open > 0 {
                bail!(
                    "channel '{source}' has {open} open gate(s) — decide (approve/reject) or \
                     re-propose each into '{target}' before converging (docs/adr/0027)"
                );
            }
        }
        // When the target is hosted here, check the converger's membership up
        // front; when it isn't (a cross-machine id), membership is enforced by
        // projection once the far side reconciles (docs/adr/0028).
        if let Some(tgt_ledger) = &tgt_ledger {
            let guard = tgt_ledger.lock().await;
            let view = guard.project(&tgt_id).await?;
            self.authorize_write(&view, &converger, code)?;
        }

        // Source side: converged-into, then closed (convergence closes it).
        {
            let mut guard = src_ledger.lock().await;
            guard
                .append(LedgerEntry {
                    id: EntryId::new(),
                    channel: src_id,
                    author: converger.clone(),
                    timestamp: Timestamp::now(),
                    payload: EntryPayload::ConvergedInto { target: tgt_id },
                })
                .await?;
            guard
                .append(LedgerEntry {
                    id: EntryId::new(),
                    channel: src_id,
                    author: converger.clone(),
                    timestamp: Timestamp::now(),
                    payload: EntryPayload::ChannelClosed {
                        rationale: rationale.to_string(),
                    },
                })
                .await?;
        }
        // Target side: convergence received. Try to write it now; on any
        // failure — or when the target isn't hosted here — park it for the
        // eventually-consistent reconciliation pass (docs/adr/0028).
        let far = LedgerEntry {
            id: EntryId::new(),
            channel: tgt_id,
            author: converger,
            timestamp: Timestamp::now(),
            payload: EntryPayload::ConvergenceReceived { source: src_id },
        };
        let landed = match &tgt_ledger {
            Some(ledger) => ledger.lock().await.append(far.clone()).await.is_ok(),
            None => false,
        };
        if !landed {
            crate::pending_lineage::enqueue(&self.member_home()?, &far)?;
        }
        Ok(())
    }

    /// Resolve a **target** channel reference for [`Host::converge`]: its id,
    /// plus its local ledger if this host hosts it. A raw id that isn't hosted
    /// here resolves to the id alone — the far side reconciles later
    /// (`docs/adr/0028`); a *name* that doesn't resolve is an error, since we
    /// cannot learn its id (converge never creates a channel, `docs/adr/0027`).
    async fn resolve_target(&self, target: &str) -> Result<(ChannelId, Option<SharedLedger>)> {
        match self.resolve(target).await? {
            Resolution::Resolved { ledger, id, .. } => Ok((id, Some(ledger))),
            Resolution::Ambiguous(substrates) => bail!(
                "channel name '{target}' exists in several substrates ({substrates:?}); \
                 address it by id"
            ),
            Resolution::NotFound => match target.parse::<ChannelId>() {
                Ok(id) => Ok((id, None)),
                Err(_) => bail!(
                    "no channel '{target}' in any registered substrate, and it is not a channel \
                     id — converge needs an existing target (docs/adr/0027)"
                ),
            },
        }
    }

    /// Drain the pending-lineage queue (`docs/adr/0028`): for each parked
    /// far-side entry, resolve its channel and append it (idempotent via
    /// content-addressed dedup, `docs/adr/0010`). Entries that still can't be
    /// written are kept for the next pass; those older than 30 days are dropped
    /// with a warning (the near side stays "unresolved"). Run on host startup
    /// and after sync — exactly when far channels become reachable.
    pub async fn reconcile_lineage(&self) -> Result<()> {
        const TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
        let home = self.member_home()?;
        let queue = crate::pending_lineage::pending(&home)?;
        if queue.is_empty() {
            return Ok(());
        }
        let now = Timestamp::now().as_millis();
        let mut keep = Vec::new();
        for entry in queue {
            if now - entry.timestamp.as_millis() > TTL_MS {
                tracing::warn!(
                    channel = %entry.channel,
                    "dropping a pending lineage edge unreconciled after 30 days (docs/adr/0028)"
                );
                continue;
            }
            let landed = match self.ledger_for_channel(entry.channel).await? {
                Some(ledger) => ledger.lock().await.append(entry.clone()).await.is_ok(),
                None => false,
            };
            if !landed {
                keep.push(entry);
            }
        }
        crate::pending_lineage::rewrite(&home, &keep)
    }

    /// Build a channel's **lineage context** for recall (`docs/adr/0027`): for
    /// each incoming edge, resolve and summarize the ancestor's standing
    /// decisions (as of the divergence point, for a diverge); for each outgoing
    /// edge, resolve the dependent channel's name and closed state. One hop —
    /// transitive recall is deferred. A dangling edge (the other end not yet
    /// reconciled) is surfaced as unresolved rather than erroring.
    pub async fn lineage_context(
        &self,
        view: &ChannelView,
    ) -> Result<crate::render::LineageContext> {
        /// How many of an ancestor's standing decisions to inherit.
        const INHERIT_MAX: usize = 8;
        let mut inherited = Vec::new();
        let mut references = Vec::new();
        for edge in &view.lineage {
            let other = self.ledger_for_channel(edge.other).await?;
            match edge.direction {
                junto_kernel::LineageDirection::Incoming => {
                    let mut entry = crate::render::InheritedLineage {
                        relation: edge.relation,
                        other: edge.other,
                        name: None,
                        decisions: Vec::new(),
                        resolved: false,
                    };
                    if let Some(ledger) = other {
                        let ancestor = ledger.lock().await.project(&edge.other).await?;
                        // The cutoff: the divergence point's timestamp, so the
                        // child inherits the parent *as of* the split.
                        let cutoff = edge.point.and_then(|point| {
                            ancestor
                                .entries
                                .iter()
                                .find(|e| e.id == point)
                                .map(|e| e.timestamp.as_millis())
                        });
                        entry.name = ancestor.name.clone();
                        entry.decisions =
                            crate::render::standing_decision_lines(&ancestor, cutoff, INHERIT_MAX);
                        entry.resolved = true;
                    }
                    inherited.push(entry);
                }
                junto_kernel::LineageDirection::Outgoing => {
                    let mut reference = crate::render::LineageRef {
                        relation: edge.relation,
                        other: edge.other,
                        name: None,
                        closed: false,
                        resolved: false,
                    };
                    if let Some(ledger) = other {
                        let dependent = ledger.lock().await.project(&edge.other).await?;
                        reference.name = dependent.name.clone();
                        reference.closed = dependent.closed;
                        reference.resolved = true;
                    }
                    references.push(reference);
                }
            }
        }
        Ok(crate::render::LineageContext {
            inherited,
            references,
        })
    }

    /// The local ledger hosting `channel`, if any registered substrate holds it.
    async fn ledger_for_channel(&self, channel: ChannelId) -> Result<Option<SharedLedger>> {
        match self.resolve(&channel.to_string()).await? {
            Resolution::Resolved { ledger, .. } => Ok(Some(ledger)),
            _ => Ok(None),
        }
    }

    /// Resolve a channel reference for a write op, turning the not-found /
    /// ambiguous cases into clear errors (the shape diverge/converge share).
    async fn resolve_for_write(&self, channel: &str) -> Result<(PathBuf, SharedLedger, ChannelId)> {
        match self.resolve(channel).await? {
            Resolution::Resolved {
                substrate,
                ledger,
                id,
            } => Ok((substrate, ledger, id)),
            Resolution::NotFound => bail!("no channel '{channel}' in any registered substrate"),
            Resolution::Ambiguous(substrates) => bail!(
                "channel name '{channel}' exists in several substrates ({substrates:?}); \
                 address it by id"
            ),
        }
    }

    /// The write-surface guardrail (`docs/adr/0017`): refuse an author who is
    /// not in the channel's Party, or whose member code is missing or wrong.
    ///
    /// A channel with no genesis has no Party and gets the legacy behaviour —
    /// no membership or code enforcement. The projection remains the real
    /// guardrail for entries that arrive by sync; this check just turns a
    /// misconfigured author into a clear error instead of an orphaned entry.
    pub fn authorize_write(
        &self,
        view: &ChannelView,
        author: &Member,
        code: Option<&str>,
    ) -> Result<()> {
        if view.party.is_empty() {
            return Ok(());
        }
        // The MCP/agent surface keeps the operational message — agents really do
        // grant membership with `junto add-member` / the `add_member` tool. The
        // human surface (authorize_human_write) carries the plain-language variant.
        if !is_member(view, author) {
            bail!(
                "{} <{}> is not a member of this channel — the founding member can grant \
                 membership (junto add-member, or the add_member tool; docs/adr/0017)",
                author.display_name,
                author.email
            );
        }
        let Some(code) = code else {
            bail!(
                "a member code is required to write as {} (it was printed when the member \
                 was minted; docs/adr/0017)",
                author.email
            );
        };
        match crate::members::check(&self.member_home()?, &author.email, code)? {
            crate::members::CodeCheck::Valid => Ok(()),
            crate::members::CodeCheck::WrongCode => {
                bail!("wrong member code for {}", author.email)
            }
            crate::members::CodeCheck::NoCodeOnFile => bail!(
                "no member code minted on this machine for {} — mint one with \
                 junto add-member (docs/adr/0017)",
                author.email
            ),
        }
    }

    /// The **human-surface** write guardrail: membership only, no member
    /// code. The web pages derive the author from git config (never from the
    /// form) and are served by this same process, which *stores* the codes —
    /// demanding one back would prove possession of a file the server itself
    /// can read: friction, not safety. Codes stay required where they earn
    /// their keep — the MCP surface, where an agent *claims* an identity and
    /// the code stops it accidentally authoring as someone else.
    pub fn authorize_human_write(&self, view: &ChannelView, author: &Member) -> Result<()> {
        if view.party.is_empty() {
            return Ok(());
        }
        if !is_member(view, author) {
            // Human surface: keep this plain. The reader is a person, not an
            // agent, so no CLI/MCP/ADR jargon — and naming the git identity makes
            // an identity mismatch (a checkout whose git config differs from your
            // membership) diagnosable at a glance.
            bail!(
                "You're acting as {} <{}> (your git identity), who isn't a member of this \
                 channel — so this can't be recorded. Only members can act on a channel; its \
                 founder can add you as one.",
                author.display_name,
                author.email
            );
        }
        Ok(())
    }
}

/// Whether `author` is in the channel's Party — membership is by stable email
/// (`docs/adr/0017`). Shared by both write surfaces, which format their own
/// (human vs agent) refusal message.
fn is_member(view: &ChannelView, author: &Member) -> bool {
    view.party.iter().any(|member| member.email == author.email)
}

/// The result of opening a channel: its id, and the founding member's
/// machine-local code (freshly minted, or pre-existing if the same identity
/// already had one — codes are per identity per machine, `docs/adr/0017`).
#[derive(Debug)]
pub struct OpenedChannel {
    pub id: ChannelId,
    pub founder_code: crate::members::Minted,
}

/// One channel's attention items from an already-projected view — used by
/// [`Host::attention`] and by the channel page's attention strip (which has
/// the view in hand and must not re-project).
pub fn attention_for_view(id: &ChannelId, view: &ChannelView) -> AttentionGroup {
    let mut gates = Vec::new();
    let mut verifications = Vec::new();
    for entry in &view.entries {
        match &entry.payload {
            EntryPayload::Proposal { .. }
                if view.gate_status(&entry.id) == Some(GateStatus::Pending) =>
            {
                gates.push(AttentionItem {
                    kind: AttentionKind::Gate,
                    entry: entry.clone(),
                });
            }
            EntryPayload::Assertion { .. }
                if view.standing(&entry.id) == Some(junto_kernel::Standing::Provisional) =>
            {
                verifications.push(AttentionItem {
                    kind: AttentionKind::Verification,
                    entry: entry.clone(),
                });
            }
            _ => {}
        }
    }
    // Oldest first within each kind: the longest-waiting item leads.
    gates.sort_by_key(|item| item.entry.timestamp);
    verifications.sort_by_key(|item| item.entry.timestamp);
    gates.extend(verifications);
    AttentionGroup {
        channel: *id,
        name: view.name.clone(),
        items: gates,
    }
}

/// The most milestone nodes a channel's track carries — bounds clutter on a
/// busy channel (the most recent win).
const MILESTONE_CAP: usize = 12;

/// A short, single-line label for a milestone node's tooltip.
fn milestone_label(text: &str) -> String {
    let text = text.trim();
    let cut: String = text.chars().take(70).collect();
    if cut.chars().count() < text.chars().count() {
        format!("{}…", cut.trim_end())
    } else {
        cut
    }
}

/// The curated milestone events on a channel — settled decisions (ratified
/// assertions, plus corrections that carry settled territory's live text),
/// attached artifacts, and open gates — in canonical order, capped to the
/// most recent [`MILESTONE_CAP`]. Verification acts fold into their targets,
/// matching the brief's "state, not history" model.
fn channel_milestones(view: &ChannelView) -> Vec<Milestone> {
    let mut milestones: Vec<Milestone> = view
        .entries
        .iter()
        .filter_map(|entry| {
            let (kind, text) = match &entry.payload {
                EntryPayload::Assertion { statement, .. }
                    if view.standing(&entry.id) == Some(Standing::Ratified) =>
                {
                    (MilestoneKind::Decision, statement.as_str())
                }
                // A correction of an assertion is the live text of settled
                // territory (its target carries a standing).
                EntryPayload::Correction {
                    target, statement, ..
                } if view.standings.contains_key(target) => {
                    (MilestoneKind::Decision, statement.as_str())
                }
                EntryPayload::ArtifactAttached { description, .. } => {
                    (MilestoneKind::Artifact, description.as_str())
                }
                EntryPayload::Proposal { action, .. }
                    if view.gate_status(&entry.id) == Some(GateStatus::Pending) =>
                {
                    (MilestoneKind::Gate, action.as_str())
                }
                _ => return None,
            };
            Some(Milestone {
                at: entry.timestamp,
                kind,
                label: milestone_label(text),
            })
        })
        .collect();
    // Keep the most recent — entries are in canonical (oldest-first) order.
    if milestones.len() > MILESTONE_CAP {
        milestones = milestones.split_off(milestones.len() - MILESTONE_CAP);
    }
    milestones
}

/// Fold one projected channel into its discovery summary.
fn summarize(id: &ChannelId, view: &ChannelView, substrate: &Path) -> ChannelSummary {
    ChannelSummary {
        id: *id,
        name: view.name.clone(),
        substrate: substrate.to_path_buf(),
        entry_count: view.entries.len(),
        last_activity: view.entries.iter().map(|entry| entry.timestamp).max(),
        first_activity: view.entries.iter().map(|entry| entry.timestamp).min(),
        open_gates: view
            .gate_status
            .values()
            .filter(|status| **status == GateStatus::Pending)
            .count(),
        members: view.party.len(),
        latest: view.entries.last().map(preview),
        closed: view.closed,
        milestones: channel_milestones(view),
        // The first parent / convergence-target edge drives the strip's
        // attachment points (docs/adr/0027).
        parent: view.lineage.iter().find_map(|edge| {
            (edge.relation == junto_kernel::LineageRelation::Diverge
                && edge.direction == junto_kernel::LineageDirection::Incoming)
                .then_some(edge.other)
        }),
        converged_into: view.lineage.iter().find_map(|edge| {
            (edge.relation == junto_kernel::LineageRelation::Converge
                && edge.direction == junto_kernel::LineageDirection::Outgoing)
                .then_some(edge.other)
        }),
    }
}

/// One entry as a one-line resumption cue: its kind, then a snippet of its
/// most telling text.
fn preview(entry: &LedgerEntry) -> String {
    let (kind, text) = match &entry.payload {
        EntryPayload::ChannelOpened { name } => ("genesis", format!("channel '{name}' opened")),
        EntryPayload::MemberAdded { member } => ("member added", member.display_name.clone()),
        EntryPayload::ChannelClosed { rationale } => ("closed", rationale.clone()),
        EntryPayload::ChannelReopened { rationale } => ("reopened", rationale.clone()),
        EntryPayload::DivergedFrom { parent, .. } => ("diverged from", parent.to_string()),
        EntryPayload::ChildDiverged { child } => ("child diverged", child.to_string()),
        EntryPayload::ConvergedInto { target } => ("converged into", target.to_string()),
        EntryPayload::ConvergenceReceived { source } => {
            ("convergence received", source.to_string())
        }
        EntryPayload::Assertion { statement, .. } => ("assertion", statement.clone()),
        EntryPayload::Ratification { rationale, .. } => ("ratification", rationale.clone()),
        EntryPayload::Park { rationale, .. } => ("park", rationale.clone()),
        EntryPayload::Correction { statement, .. } => ("correction", statement.clone()),
        EntryPayload::Proposal { action, .. } => ("proposal", action.clone()),
        EntryPayload::Approval { rationale, .. } => ("approval", rationale.clone()),
        EntryPayload::Rejection { rationale, .. } => ("rejection", rationale.clone()),
        EntryPayload::SessionStarted { intent } => ("session started", intent.clone()),
        EntryPayload::SessionUpdated { note, .. } => ("session updated", note.clone()),
        EntryPayload::ArtifactAttached { description, .. } => ("artifact", description.clone()),
    };
    const LIMIT: usize = 160;
    let snippet: String = text.chars().take(LIMIT).collect();
    let ellipsis = if text.chars().count() > LIMIT {
        "…"
    } else {
        ""
    };
    format!("{kind} — {snippet}{ellipsis}")
}

/// Test support for anything that touches `JUNTO_HOME`: the env var is
/// process-global and cargo runs tests in parallel threads, so every test
/// that sets it must hold the **one** lock — a per-module lock would still
/// race against other modules' tests.
#[cfg(test)]
pub(crate) mod test_home {
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Points `JUNTO_HOME` at a fresh temp dir for the guard's lifetime,
    /// serialized across the whole test process.
    pub(crate) struct HomeGuard {
        dir: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl HomeGuard {
        pub(crate) fn new() -> Self {
            let lock = HOME_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let dir = tempfile::tempdir().expect("temp junto home");
            unsafe { std::env::set_var("JUNTO_HOME", dir.path()) };
            Self { dir, _lock: lock }
        }

        pub(crate) fn path(&self) -> &std::path::Path {
            self.dir.path()
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("JUNTO_HOME") };
        }
    }
}

#[cfg(test)]
mod lineage_tests {
    use super::*;
    use junto_kernel::{ApprovalRequirement, LineageDirection, LineageRelation};
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn git_repo() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
        dir
    }

    /// A fixed host over `count` fresh git repos, with an isolated member-code
    /// store (never the real `~/.junto`). The member home is the last dir.
    fn lineage_host(count: usize) -> (Vec<TempDir>, Arc<Host>) {
        let mut dirs = Vec::new();
        let mut paths = Vec::new();
        for _ in 0..count {
            let d = git_repo();
            paths.push(d.path().to_path_buf());
            dirs.push(d);
        }
        let member_home = tempfile::tempdir().unwrap();
        let host = Host::fixed_with_member_home(paths, Some(member_home.path().to_path_buf()));
        dirs.push(member_home);
        (dirs, host)
    }

    fn member_home(dirs: &[TempDir]) -> &Path {
        dirs.last().unwrap().path()
    }
    fn dan() -> Member {
        Member::human("Dan", "dan@example.com")
    }
    fn code_for(dirs: &[TempDir], m: &Member) -> String {
        crate::members::mint(member_home(dirs), m).unwrap().code
    }

    async fn project(host: &Host, channel: &str) -> (ChannelId, ChannelView) {
        let Resolution::Resolved { ledger, id, .. } = host.resolve(channel).await.unwrap() else {
            panic!("channel '{channel}' resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        (id, view)
    }

    #[tokio::test]
    async fn diverge_opens_child_and_records_both_edges() {
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "parent", dan(), None)
            .await
            .unwrap();
        let code = code_for(&dirs, &dan());

        let child = host
            .diverge("parent", "side-quest", None, dan(), Some(&code))
            .await
            .unwrap();

        let (parent_id, parent_view) = project(&host, "parent").await;
        assert!(
            parent_view
                .lineage
                .iter()
                .any(|e| e.relation == LineageRelation::Diverge
                    && e.direction == LineageDirection::Outgoing
                    && e.other == child.id),
            "parent records the child diverged"
        );

        let child_view = host.resolve(&child.id.to_string()).await.unwrap();
        let Resolution::Resolved { ledger, id, .. } = child_view else {
            panic!("child resolves");
        };
        let child_view = ledger.lock().await.project(&id).await.unwrap();
        assert!(
            child_view
                .lineage
                .iter()
                .any(|e| e.relation == LineageRelation::Diverge
                    && e.direction == LineageDirection::Incoming
                    && e.other == parent_id),
            "child records it diverged from the parent"
        );
        // The diverger founds the child (docs/adr/0027).
        assert_eq!(
            child_view.party.first().map(|m| m.email.as_str()),
            Some("dan@example.com")
        );
    }

    #[tokio::test]
    async fn diverge_requires_parent_membership() {
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "parent", dan(), None)
            .await
            .unwrap();
        let stranger = Member::agent("Stranger", "stranger@example.com");
        let code = code_for(&dirs, &stranger);
        let err = host
            .diverge("parent", "sq", None, stranger, Some(&code))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a member"), "{err}");
    }

    #[tokio::test]
    async fn converge_closes_source_and_records_both_edges() {
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "src", dan(), None).await.unwrap();
        host.open_channel(None, "tgt", dan(), None).await.unwrap();
        let code = code_for(&dirs, &dan());

        host.converge("src", "tgt", "merged the side-quest", dan(), Some(&code))
            .await
            .unwrap();

        let (src_id, src_view) = project(&host, "src").await;
        assert!(src_view.closed, "the source closes on convergence");
        assert!(
            src_view
                .lineage
                .iter()
                .any(|e| e.relation == LineageRelation::Converge
                    && e.direction == LineageDirection::Outgoing),
            "source records it converged into the target"
        );

        let (_tgt_id, tgt_view) = project(&host, "tgt").await;
        assert!(
            tgt_view
                .lineage
                .iter()
                .any(|e| e.relation == LineageRelation::Converge
                    && e.direction == LineageDirection::Incoming
                    && e.other == src_id),
            "target records it received the source"
        );
    }

    #[tokio::test]
    async fn converge_refuses_while_source_has_an_open_gate() {
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "src", dan(), None).await.unwrap();
        host.open_channel(None, "tgt", dan(), None).await.unwrap();
        let code = code_for(&dirs, &dan());

        // A pending proposal in the source — an undisposed open gate.
        let (src_id, _) = project(&host, "src").await;
        let ledger = host.ledger_for(dirs[0].path()).await.unwrap();
        ledger
            .lock()
            .await
            .append(LedgerEntry {
                id: EntryId::new(),
                channel: src_id,
                author: dan(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::Proposal {
                    action: "ship it".into(),
                    rationale: "because".into(),
                    provenance: vec![],
                    requirement: ApprovalRequirement::Count(1),
                    frame: None,
                },
            })
            .await
            .unwrap();

        let err = host
            .converge("src", "tgt", "merge", dan(), Some(&code))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("open gate"), "{err}");

        // And the source is NOT closed — the refusal left it untouched.
        let (_, src_view) = project(&host, "src").await;
        assert!(!src_view.closed);
    }

    #[tokio::test]
    async fn child_brief_inherits_parent_standing_decisions() {
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "parent", dan(), None)
            .await
            .unwrap();
        let code = code_for(&dirs, &dan());

        // A ratified decision in the parent.
        let (parent_id, _) = project(&host, "parent").await;
        let ledger = host.ledger_for(dirs[0].path()).await.unwrap();
        let decision = EntryId::new();
        ledger
            .lock()
            .await
            .append(LedgerEntry {
                id: decision,
                channel: parent_id,
                author: dan(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::Assertion {
                    statement: "use NDJSON for the pending queue".into(),
                    rationale: "matches the substrate".into(),
                    provenance: vec![],
                    frame: None,
                },
            })
            .await
            .unwrap();
        ledger
            .lock()
            .await
            .append(LedgerEntry {
                id: EntryId::new(),
                channel: parent_id,
                author: dan(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::Ratification {
                    target: decision,
                    rationale: "agreed".into(),
                },
            })
            .await
            .unwrap();

        let child = host
            .diverge("parent", "sq", None, dan(), Some(&code))
            .await
            .unwrap();
        let child_view = {
            let Resolution::Resolved { ledger, id, .. } =
                host.resolve(&child.id.to_string()).await.unwrap()
            else {
                panic!("child resolves");
            };
            ledger.lock().await.project(&id).await.unwrap()
        };
        let ctx = host.lineage_context(&child_view).await.unwrap();
        let brief = crate::render::brief_markdown("sq", &child.id, &child_view, &ctx);
        assert!(brief.contains("inherited context"), "{brief}");
        assert!(
            brief.contains("use NDJSON for the pending queue"),
            "the child's brief inherits the parent's ratified decision: {brief}"
        );
    }

    #[tokio::test]
    async fn reconcile_lands_a_pending_far_side_edge() {
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "tgt", dan(), None).await.unwrap();
        let (tgt_id, _) = project(&host, "tgt").await;

        // Park a far-side edge as if a source had converged into tgt.
        let far = LedgerEntry {
            id: EntryId::new(),
            channel: tgt_id,
            author: dan(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::ConvergenceReceived {
                source: ChannelId::new(),
            },
        };
        crate::pending_lineage::enqueue(member_home(&dirs), &far).unwrap();

        host.reconcile_lineage().await.unwrap();

        let (_, tgt_view) = project(&host, "tgt").await;
        assert!(
            tgt_view
                .lineage
                .iter()
                .any(|e| e.relation == LineageRelation::Converge
                    && e.direction == LineageDirection::Incoming),
            "the parked edge landed in the target"
        );
        assert!(
            crate::pending_lineage::pending(member_home(&dirs))
                .unwrap()
                .is_empty(),
            "and was removed from the queue"
        );
    }

    #[tokio::test]
    async fn reconcile_drops_edges_older_than_30_days() {
        let (dirs, host) = lineage_host(1);
        let thirty_one_days = 31 * 24 * 60 * 60 * 1000;
        let old = Timestamp::from_millis(Timestamp::now().as_millis() - thirty_one_days);
        // Target not hosted here, so it could never land regardless.
        let far = LedgerEntry {
            id: EntryId::new(),
            channel: ChannelId::new(),
            author: dan(),
            timestamp: old,
            payload: EntryPayload::ConvergenceReceived {
                source: ChannelId::new(),
            },
        };
        crate::pending_lineage::enqueue(member_home(&dirs), &far).unwrap();

        host.reconcile_lineage().await.unwrap();

        assert!(
            crate::pending_lineage::pending(member_home(&dirs))
                .unwrap()
                .is_empty(),
            "the 30-day bound drops the unreconciled edge"
        );
    }
}
