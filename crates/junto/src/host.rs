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
    MemberKind, PublicKey, Standing, SubstrateProvider, Timestamp,
};
use junto_substrate_git::GitRefsSubstrate;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// One open ledger, shared by the MCP tools and the web routes. The mutex
/// serializes appends (read-modify-write on the underlying git ref).
pub type SharedLedger = Arc<Mutex<Ledger<GitRefsSubstrate>>>;

/// How a write is authorized — the two surfaces differ (`docs/adr/0021`): the
/// agent surface (MCP/CLI) requires the author's machine-local member code; the
/// human surface (web pages the desktop shell wraps) checks membership only,
/// deriving the author from git config. Threaded into the lineage ops so
/// `diverge`/`converge` serve both.
pub enum WriteAuth<'a> {
    /// Agent surface: the claimed author's member code (`docs/adr/0017`).
    Agent(Option<&'a str>),
    /// Human surface: membership only, no code (`docs/adr/0021`).
    Human,
}

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
    /// An **approved actionable gate whose action has not run** (`docs/adr/0030`):
    /// approved + carries an executable `kind` + no successful `GateExecuted`.
    /// Surfaced so a silently-stuck or failed execution is never mistaken for a
    /// completed one.
    AwaitingExecution,
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

    /// The live plane registry (`crate::live_plane`) for this host's
    /// running Agent Sessions — one CRDT document + presence + frame
    /// broadcast per live session, tapped alongside [`Host::live`]'s SSE
    /// feed and archived when a session ends. Delegates to
    /// [`crate::launch::LiveSessions`]'s own plane field rather than adding
    /// a second field to `Host`, since `LiveSessions`' `begin`/`publish`/
    /// `finish` are the taps that populate it.
    pub(crate) fn live_plane(&self) -> &Arc<crate::live_plane::LivePlane> {
        &self.live.plane
    }

    /// Where this host's member-code store lives (`docs/adr/0017`): the
    /// machine registry's junto home, unless overridden (tests). Exposed
    /// crate-wide (not just to [`Host::sign_entry`]) so a caller that needs
    /// to ask "does this host hold a key for X" — e.g.
    /// `crate::live_bridge::deliver_batch`'s non-minting lookup — resolves
    /// the identical path `sign_entry` signs against, including the
    /// test-only override; asking a *different* home would silently check
    /// the wrong store.
    pub(crate) fn member_home(&self) -> Result<PathBuf> {
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

    /// Attach `member`'s public key (`docs/adr/0033`) — from `key` when the
    /// caller supplied one (an enrolled device's own public half; this host
    /// must never mint or hold a keypair for someone else), or minted/reused
    /// locally when none is supplied (the legitimate case: the channel
    /// founder in [`Host::open_channel`], or a local agent identity via
    /// [`Host::add_member`]'s keyless path). Used on the
    /// **membership-granting** entries (genesis, `MemberAdded`) — the party
    /// projection reads these as the channel keyring. Best-effort on the
    /// local-mint path: a key-store failure leaves the member keyless
    /// (their entries surface as unverified) rather than blocking.
    fn keyed(&self, member: Member, key: Option<PublicKey>) -> Member {
        if let Some(key) = key {
            return member.with_key(key);
        }
        let key = self
            .member_home()
            .and_then(|home| crate::keys::signing_key(&home, &member.email));
        match key {
            Ok(key) => member.with_key(key.public_key()),
            Err(err) => {
                tracing::warn!("minting a signing key for {}: {err:#}", member.email);
                member
            }
        }
    }

    /// Attach `member`'s transport key (`docs/adr/0033` two-key
    /// separation) — mirrors [`Self::keyed`], but for
    /// [`Member::with_transport_key`]/`keys::transport_key`: from
    /// `transport_key` when the caller supplied one, or minted/reused
    /// locally when none is supplied AND `local_mint_allowed` — the exact
    /// condition [`Host::add_member`] already established was legitimate
    /// for the *signing* key (its own `key` param was absent). Gating on
    /// that, rather than blindly minting whenever `transport_key` alone is
    /// `None`, matters: a caller that supplied a signing key but not a
    /// transport key is describing a remote identity this host has no
    /// authority over, and must never mint a transport key for it either
    /// — silently doing so would reopen the exact "minted on the wrong
    /// machine" bug `docs/adr/0033` exists to close, just for the
    /// transport half. Best-effort like `keyed`: a key-store failure
    /// leaves the member without a transport key rather than blocking.
    /// Only [`Host::add_member`] calls this — [`Host::open_channel`]'s
    /// founder genesis does not carry a transport key yet; no caller needs
    /// a founder's transport key before a later transport slice consumes
    /// it.
    fn keyed_transport(
        &self,
        member: Member,
        transport_key: Option<PublicKey>,
        local_mint_allowed: bool,
    ) -> Member {
        if let Some(transport_key) = transport_key {
            return member.with_transport_key(transport_key);
        }
        if !local_mint_allowed {
            return member;
        }
        let key = self
            .member_home()
            .and_then(|home| crate::keys::transport_key(&home, &member.email));
        match key {
            Ok(key) => member.with_transport_key(key.public_key()),
            Err(err) => {
                tracing::warn!("minting a transport key for {}: {err:#}", member.email);
                member
            }
        }
    }

    /// Sign `entry` with its author's machine-local key (`docs/adr/0033`),
    /// minting the keypair on first use. Best-effort by design: verification
    /// is a surfaced projection fact, never a gate — so a signing failure
    /// logs and leaves the entry unsigned instead of refusing the write.
    pub fn sign_entry(&self, entry: &mut LedgerEntry) {
        let signed = self
            .member_home()
            .and_then(|home| crate::keys::signing_key(&home, &entry.author.email))
            .and_then(|key| entry.sign(&key).map_err(anyhow::Error::from));
        if let Err(err) = signed {
            tracing::warn!("signing an entry as {}: {err:#}", entry.author.email);
        }
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
        // The genesis carries the founder's public key — the first link in the
        // channel keyring (`docs/adr/0033`) — and is signed like every entry
        // written through this host.
        let opened_by = self.keyed(opened_by, None);
        let mut genesis = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: id,
            author: opened_by.clone(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::ChannelOpened {
                name: name.to_string(),
            },
        };
        self.sign_entry(&mut genesis);
        guard.append(genesis).await?;
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
        key: Option<PublicKey>,
        transport_key: Option<PublicKey>,
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

        // Re-granting an existing member (the founder included) is a no-op
        // on the **Party** — `project_party` is first-write-wins by email,
        // so a second `MemberAdded` never changes the row; display name and
        // kind stay whatever the first admission set. But it is not
        // necessarily a no-op on the **keyring**: multi-device enrollment
        // (`docs/adr/0033`) is expressed as a second `MemberAdded` for the
        // same email carrying a device key that email has never been
        // granted (the founder's own second device works the same way —
        // they author a grant for their own email). Skip the append only
        // when there is truly no new key to publish: none was supplied, or
        // that exact key already has an ACTIVE grant on this email's
        // keyring. A *retired* grant for the same key does not count —
        // re-admitting a member after `revoke-member` parked every grant
        // must still append, or `add-member --enroll` reports success
        // while leaving them cut off (and `keys::signing_key` mints a
        // device's key only once, so there is no other path to a fresh
        // one). This is by design, not a bug to "fix" back: a retired
        // grant is re-grantable, and the party/keyring divergence here is
        // the whole mechanism multi-device enrollment relies on.
        if view.party.iter().any(|m| m.email == member.email) {
            let already_granted = key.as_ref().is_some_and(|supplied| {
                view.keyring.get(&member.email).is_some_and(|grants| {
                    grants
                        .iter()
                        .any(|grant| &grant.key == supplied && grant.retired_at.is_none())
                })
            });
            if key.is_none() || already_granted {
                return crate::members::mint(&self.member_home()?, &member);
            }
        }

        // Local minting authority (`docs/adr/0033`): when the caller has not
        // supplied the new member's own key (the keyless/interactive path;
        // `junto add-member --enroll` always supplies one), this host must
        // decide whether it may legitimately mint one itself.
        // `keys::has_signing_key` — never `signing_key`, which mints as a
        // side effect of the lookup — answers "does a key already exist"
        // without creating one: if this machine already holds a key for
        // this email, reuse is legitimate no matter the member kind (e.g.
        // the founder joining a second channel). Otherwise an agent may
        // mint fresh — it runs on this machine by construction — but a
        // human may not: minting a key for a human whose machine is not
        // this one is exactly the bug this plan exists to close (a keypair
        // they never receive, on the founder's machine instead of their
        // own).
        if key.is_none() {
            let home = self.member_home()?;
            let already_local = crate::keys::has_signing_key(&home, &member.email)?;
            if !already_local && member.kind == MemberKind::Human {
                bail!(
                    "{} has no signing key on this machine, and none was supplied — a human \
                     member's key must come from their own device, not be minted here. This \
                     requires terminal access: run `junto invite --member {} --channel \
                     {channel}`, have them run `junto enroll --invite <url>`, then finish with \
                     `junto add-member --enroll <their-enroll-url> --channel {channel}`. A \
                     caller without terminal access (e.g. over MCP) cannot complete this \
                     exchange itself — hand it off to someone who can run those commands",
                    member.email,
                    member.email
                );
            }
        }

        // The grant carries the new member's public key — how the keyring
        // grows (`docs/adr/0033`). An agent's key is its own, minted like its
        // member code — never its operator's. The transport half follows the
        // same rule (`docs/adr/0033` two-key separation) — never the
        // operator's, and never a copy of the signing key.
        let local_mint_allowed = key.is_none();
        let member = self.keyed(member, key);
        let member = self.keyed_transport(member, transport_key, local_mint_allowed);
        let mut grant = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: id,
            author: granted_by.clone(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::MemberAdded {
                member: member.clone(),
            },
        };
        self.sign_entry(&mut grant);
        guard.append(grant).await?;
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
        auth: WriteAuth<'_>,
    ) -> Result<OpenedChannel> {
        let (parent_substrate, parent_ledger, parent_id) = self.resolve_for_write(parent).await?;
        // The diverger must be a member of the parent to author its side.
        {
            let guard = parent_ledger.lock().await;
            let view = guard.project(&parent_id).await?;
            self.check_write_auth(&view, &diverger, &auth)?;
        }

        // Open the child beside its parent; the diverger is its founder.
        let child = self
            .open_channel(Some(&parent_substrate), child_name, diverger.clone(), None)
            .await?;

        // The child and parent share a substrate (one ledger Arc holds both).
        let mut guard = parent_ledger.lock().await;
        let mut child_side = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: child.id,
            author: diverger.clone(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::DivergedFrom {
                parent: parent_id,
                at,
            },
        };
        self.sign_entry(&mut child_side);
        guard.append(child_side).await?;
        let mut parent_side = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: parent_id,
            author: diverger,
            timestamp: Timestamp::now(),
            payload: EntryPayload::ChildDiverged { child: child.id },
        };
        self.sign_entry(&mut parent_side);
        guard.append(parent_side).await?;
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
        auth: WriteAuth<'_>,
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
            self.check_write_auth(&view, &converger, &auth)?;
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
            self.check_write_auth(&view, &converger, &auth)?;
        }

        // Source side: converged-into, then closed (convergence closes it).
        {
            let mut guard = src_ledger.lock().await;
            let mut converged = LedgerEntry {
                signature: None,
                id: EntryId::new(),
                channel: src_id,
                author: converger.clone(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::ConvergedInto { target: tgt_id },
            };
            self.sign_entry(&mut converged);
            guard.append(converged).await?;
            let mut closed = LedgerEntry {
                signature: None,
                id: EntryId::new(),
                channel: src_id,
                author: converger.clone(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::ChannelClosed {
                    rationale: rationale.to_string(),
                },
            };
            self.sign_entry(&mut closed);
            guard.append(closed).await?;
        }
        // Target side: convergence received. Try to write it now; on any
        // failure — or when the target isn't hosted here — park it for the
        // eventually-consistent reconciliation pass (docs/adr/0028). Signed
        // **before** enqueueing: the queue's retry contract is fixed bytes
        // (docs/adr/0028), and the signature is part of the bytes.
        let mut far = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: tgt_id,
            author: converger,
            timestamp: Timestamp::now(),
            payload: EntryPayload::ConvergenceReceived { source: src_id },
        };
        self.sign_entry(&mut far);
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

    /// Authorize a write under whichever surface's rules apply (`docs/adr/0021`)
    /// — the agent surface checks the member code, the human surface checks
    /// membership only. The lineage ops ([`Host::diverge`]/[`Host::converge`])
    /// dispatch through here so they serve both surfaces.
    fn check_write_auth(
        &self,
        view: &ChannelView,
        author: &Member,
        auth: &WriteAuth<'_>,
    ) -> Result<()> {
        match auth {
            WriteAuth::Agent(code) => self.authorize_write(view, author, *code),
            WriteAuth::Human => self.authorize_human_write(view, author),
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
    let mut awaiting = Vec::new();
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
            // An approved actionable gate whose action hasn't succeeded
            // (docs/adr/0030): surfaced so a stuck/failed execution isn't
            // mistaken for done.
            EntryPayload::Proposal { kind: Some(_), .. }
                if view.gate_status(&entry.id) == Some(GateStatus::Approved)
                    && view.gate_executed(&entry.id) != Some(true) =>
            {
                awaiting.push(AttentionItem {
                    kind: AttentionKind::AwaitingExecution,
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
    // Oldest first within each kind: the longest-waiting item leads. Gates
    // (blocked proposer) first, then stuck executions, then verification debt.
    gates.sort_by_key(|item| item.entry.timestamp);
    awaiting.sort_by_key(|item| item.entry.timestamp);
    verifications.sort_by_key(|item| item.entry.timestamp);
    gates.extend(awaiting);
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
        EntryPayload::GateExecuted { success, note, .. } => (
            if *success {
                "gate executed"
            } else {
                "gate execution failed"
            },
            note.clone(),
        ),
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

    /// `docs/adr/0033` end to end through the host: opening a channel keys
    /// and signs the genesis; adding an agent member keys the grant with the
    /// **agent's own** key (never the operator's); every entry written through
    /// the host verifies, so `unverified` is empty.
    #[tokio::test]
    async fn host_writes_are_signed_and_verify() {
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "signed", dan(), None)
            .await
            .unwrap();
        let agent = Member::agent("Worker", "worker@agents.junto");
        host.add_member("signed", &dan(), agent.clone(), None, None)
            .await
            .unwrap();

        let (_, view) = project(&host, "signed").await;
        // The roster carries the keys minted into the machine-local store.
        let home = member_home(&dirs);
        let dan_key = crate::keys::signing_key(home, "dan@example.com").unwrap();
        let agent_key = crate::keys::signing_key(home, "worker@agents.junto").unwrap();
        assert_ne!(
            dan_key.public_key(),
            agent_key.public_key(),
            "an agent's key is its own, never its operator's (docs/adr/0033)"
        );
        assert_eq!(
            view.party.first().and_then(|m| m.public_key.clone()),
            Some(dan_key.public_key())
        );
        assert_eq!(
            view.party.get(1).and_then(|m| m.public_key.clone()),
            Some(agent_key.public_key())
        );
        // Everything written through the host is signed and verifies.
        assert!(
            view.unverified.is_empty(),
            "host-written entries verify: {:?}",
            view.unverified
        );
    }

    /// The founder's machine must never mint or hold a keypair for someone
    /// else (`docs/adr/0033`) — the whole reason this plan exists. Adding a
    /// member whose key comes from an enroll payload must leave no record
    /// for that email in `<member_home>/keys.toml`.
    #[tokio::test]
    async fn add_member_with_a_supplied_key_does_not_mint_locally() {
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "acme", dan(), None).await.unwrap();
        let alice_key = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let alice = Member::human("Alice", "alice@example.com");
        host.add_member("acme", &dan(), alice, Some(alice_key.public_key()), None)
            .await
            .unwrap();

        assert!(
            !crate::keys::has_signing_key(member_home(&dirs), "alice@example.com").unwrap(),
            "alice's own device mints her key, never dan's machine"
        );
    }

    /// The recorded keyring grant is the key that was passed in, not one
    /// minted locally.
    #[tokio::test]
    async fn the_recorded_member_carries_the_supplied_public_key() {
        let (_dirs, host) = lineage_host(1);
        host.open_channel(None, "acme", dan(), None).await.unwrap();
        let alice_key = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let alice = Member::human("Alice", "alice@example.com");
        host.add_member("acme", &dan(), alice, Some(alice_key.public_key()), None)
            .await
            .unwrap();

        let (_, view) = project(&host, "acme").await;
        let grant = view
            .keyring
            .get("alice@example.com")
            .and_then(|grants| grants.first())
            .expect("alice has a keyring grant");
        assert_eq!(
            grant.key,
            alice_key.public_key(),
            "the recorded key is the one supplied, not a locally minted one"
        );
    }

    /// The central integration point this task exists for: an enroll
    /// payload supplying BOTH halves flows through `Host::add_member` into
    /// the projected grant unchanged — the recorded transport key is the
    /// one supplied, not a locally minted one, and it rides the SAME grant
    /// as the signing key (not a separate one).
    #[tokio::test]
    async fn add_member_with_a_supplied_transport_key_records_the_pair() {
        let (_dirs, host) = lineage_host(1);
        host.open_channel(None, "acme", dan(), None).await.unwrap();
        let alice_key = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let alice_transport_key = junto_kernel::SigningKey::from_secret_bytes([8; 32]);
        let alice = Member::human("Alice", "alice@example.com");
        host.add_member(
            "acme",
            &dan(),
            alice,
            Some(alice_key.public_key()),
            Some(alice_transport_key.public_key()),
        )
        .await
        .unwrap();

        let (_, view) = project(&host, "acme").await;
        let grant = view
            .keyring
            .get("alice@example.com")
            .and_then(|grants| grants.first())
            .expect("alice has a keyring grant");
        assert_eq!(
            grant.key,
            alice_key.public_key(),
            "the recorded signing key is the one supplied"
        );
        assert_eq!(
            grant.transport_key,
            Some(alice_transport_key.public_key()),
            "the recorded transport key is the one supplied, not a locally minted one"
        );
    }

    /// The core fix's other half: a keyless grant for a human this host has
    /// no local key for (and was handed no key) must be refused, not
    /// silently mint one on the founder's machine.
    #[tokio::test]
    async fn add_member_refuses_to_mint_for_a_keyless_remote_human() {
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "acme", dan(), None).await.unwrap();
        let bob = Member::human("Bob", "bob@example.com");
        let err = host
            .add_member("acme", &dan(), bob, None, None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("enroll"),
            "the refusal should point at the enrollment path: {err}"
        );
        assert!(
            !crate::keys::has_signing_key(member_home(&dirs), "bob@example.com").unwrap(),
            "no key was minted for the refused human"
        );
    }

    /// The keyless/interactive path must keep working for the local-agent
    /// case: an agent runs on this machine by construction, so first-use
    /// minting is legitimate — and that must mint BOTH halves, not just
    /// the signing key, giving `keys::has_transport_key` its first real
    /// caller.
    #[tokio::test]
    async fn add_member_keyless_still_mints_for_a_local_agent() {
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "acme", dan(), None).await.unwrap();
        let worker = Member::agent("Worker", "worker@agents.junto");
        host.add_member("acme", &dan(), worker, None, None)
            .await
            .unwrap();
        assert!(
            crate::keys::has_signing_key(member_home(&dirs), "worker@agents.junto").unwrap(),
            "an agent's key is minted like its member code (docs/adr/0033)"
        );
        assert!(
            crate::keys::has_transport_key(member_home(&dirs), "worker@agents.junto").unwrap(),
            "the transport half is minted alongside the signing key on the same local path"
        );
    }

    /// A human who already has a local key (e.g. the founder joining a
    /// second channel they didn't found) reuses it rather than being
    /// refused — the refusal is specifically for identities this machine
    /// has never held a key for.
    #[tokio::test]
    async fn add_member_keyless_reuses_an_existing_local_human_key() {
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "acme", dan(), None).await.unwrap();
        let key_before = crate::keys::signing_key(member_home(&dirs), "dan@example.com").unwrap();
        let eve = Member::human("Eve", "eve@example.com");
        host.open_channel(None, "second", eve.clone(), None)
            .await
            .unwrap();
        host.add_member("second", &eve, dan(), None, None)
            .await
            .unwrap();

        // The deciding claim: the grant actually recorded on 'second's
        // keyring is the PRE-EXISTING key, not a value `keyed` obtained by
        // some other path (e.g. re-minting). Comparing two `signing_key`
        // calls to each other would only pin `signing_key`'s own
        // reuse-when-present contract, not what `add_member` did with it.
        let (_, view) = project(&host, "second").await;
        let grant = view
            .keyring
            .get("dan@example.com")
            .and_then(|grants| grants.first())
            .expect("dan has a keyring grant in 'second'");
        assert_eq!(
            grant.key,
            key_before.public_key(),
            "the recorded grant carries the pre-existing local key, not a fresh mint"
        );
    }

    /// Multi-device enrollment's headline case (`docs/adr/0033`,
    /// `docs/superpowers/specs/2026-08-21-device-key-enrollment-design.md`):
    /// a second `add_member` for an email already on the roster, carrying a
    /// key that email has never been granted, must still append — that is
    /// how a second device's key reaches the channel and the keyring.
    #[tokio::test]
    async fn add_member_records_a_second_device_for_an_existing_member() {
        let (_dirs, host) = lineage_host(1);
        host.open_channel(None, "acme", dan(), None).await.unwrap();
        let key_a = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let key_b = junto_kernel::SigningKey::from_secret_bytes([13; 32]);
        assert_ne!(
            key_a.public_key(),
            key_b.public_key(),
            "the fixture must exercise two genuinely distinct device keypairs"
        );
        host.add_member(
            "acme",
            &dan(),
            Member::human("Alice", "alice@example.com"),
            Some(key_a.public_key()),
            None,
        )
        .await
        .unwrap();
        host.add_member(
            "acme",
            &dan(),
            Member::human("Alice", "alice@example.com"),
            Some(key_b.public_key()),
            None,
        )
        .await
        .unwrap();

        let (_, view) = project(&host, "acme").await;
        let alice_entries = view
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    &entry.payload,
                    EntryPayload::MemberAdded { member } if member.email == "alice@example.com"
                )
            })
            .count();
        assert_eq!(
            alice_entries, 2,
            "the ledger gained a second MemberAdded for alice's second device"
        );
        let keys: Vec<_> = view
            .keyring
            .get("alice@example.com")
            .expect("alice has keyring grants")
            .iter()
            .map(|grant| grant.key.clone())
            .collect();
        assert!(
            keys.contains(&key_a.public_key()) && keys.contains(&key_b.public_key()),
            "the projected keyring carries both of alice's device keys: {keys:?}"
        );
    }

    /// The Party is first-write-wins by email — a second device's grant must
    /// not create a second row, nor change the display name the first
    /// admission set (`project_party`'s guarantee, which this fix must not
    /// route around by mutating the email or re-admitting differently).
    #[tokio::test]
    async fn add_member_second_device_leaves_party_row_unchanged() {
        let (_dirs, host) = lineage_host(1);
        host.open_channel(None, "acme", dan(), None).await.unwrap();
        let key_a = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let key_b = junto_kernel::SigningKey::from_secret_bytes([13; 32]);
        host.add_member(
            "acme",
            &dan(),
            Member::human("Alice", "alice@example.com"),
            Some(key_a.public_key()),
            None,
        )
        .await
        .unwrap();
        // A different display name on the second call: if the party row
        // were ever affected by the second grant, this assertion would
        // catch it. Same-name fixtures would make the count check the only
        // load-bearing assertion here.
        host.add_member(
            "acme",
            &dan(),
            Member::human("Alice Renamed", "alice@example.com"),
            Some(key_b.public_key()),
            None,
        )
        .await
        .unwrap();

        let (_, view) = project(&host, "acme").await;
        let alice_rows: Vec<_> = view
            .party
            .iter()
            .filter(|m| m.email == "alice@example.com")
            .collect();
        assert_eq!(
            alice_rows.len(),
            1,
            "one party row per email, no matter how many device grants: {alice_rows:?}"
        );
        assert_eq!(
            alice_rows[0].display_name, "Alice",
            "the party row keeps the FIRST admission's display name, not the second's"
        );
    }

    /// Re-supplying a key already granted to that email is still a
    /// no-op — the case the original unconditional guard existed to
    /// prevent, and it must keep working now that the guard is conditional.
    #[tokio::test]
    async fn add_member_repeat_of_same_key_appends_nothing() {
        let (_dirs, host) = lineage_host(1);
        host.open_channel(None, "acme", dan(), None).await.unwrap();
        let key_a = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        host.add_member(
            "acme",
            &dan(),
            Member::human("Alice", "alice@example.com"),
            Some(key_a.public_key()),
            None,
        )
        .await
        .unwrap();
        host.add_member(
            "acme",
            &dan(),
            Member::human("Alice", "alice@example.com"),
            Some(key_a.public_key()),
            None,
        )
        .await
        .unwrap();

        let (_, view) = project(&host, "acme").await;
        let alice_entries = view
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    &entry.payload,
                    EntryPayload::MemberAdded { member } if member.email == "alice@example.com"
                )
            })
            .count();
        assert_eq!(
            alice_entries, 1,
            "the same key twice must not double-append (the original guard's own case)"
        );
        assert_eq!(
            view.keyring
                .get("alice@example.com")
                .map(|grants| grants.len()),
            Some(1),
            "the keyring holds one grant, not a duplicate of the same key"
        );
    }

    /// A keyless re-grant (no new key to publish) is still the pre-0033
    /// no-op: no entry appended. The second, keyless call must still
    /// succeed — Task 8's keyless-refusal path (`key.is_none()` and no
    /// local key for a human) is for a *new* human with no local key, not
    /// this already-admitted case; it must not fall through to that
    /// `bail!`.
    #[tokio::test]
    async fn add_member_keyless_repeat_still_no_ops() {
        let (_dirs, host) = lineage_host(1);
        host.open_channel(None, "acme", dan(), None).await.unwrap();
        let key_a = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        host.add_member(
            "acme",
            &dan(),
            Member::human("Bob", "bob@example.com"),
            Some(key_a.public_key()),
            None,
        )
        .await
        .unwrap();
        host.add_member(
            "acme",
            &dan(),
            Member::human("Bob", "bob@example.com"),
            None,
            None,
        )
        .await
        .unwrap();
        let (_, view) = project(&host, "acme").await;
        let bob_entries = view
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    &entry.payload,
                    EntryPayload::MemberAdded { member } if member.email == "bob@example.com"
                )
            })
            .count();
        assert_eq!(
            bob_entries, 1,
            "no key to publish means no entry, exactly like before 0033"
        );
    }

    /// Re-admitting a member's device after `revoke-member` parked its
    /// grant (Task 9, main.rs) must still append — the dedup that skips a
    /// repeat of the same key only holds while that grant is ACTIVE.
    /// Without this, `add-member --enroll` for a returning member would
    /// report success while leaving them cut off, with no other way back:
    /// `keys::signing_key` mints a device's key once and returns it
    /// forever, so there is no CLI path to a fresh keypair for the same
    /// device.
    #[tokio::test]
    async fn add_member_re_grants_a_previously_revoked_key() {
        let (_dirs, host) = lineage_host(1);
        host.open_channel(None, "acme", dan(), None).await.unwrap();
        let key_a = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        host.add_member(
            "acme",
            &dan(),
            Member::human("Alice", "alice@example.com"),
            Some(key_a.public_key()),
            None,
        )
        .await
        .unwrap();

        // Revoke it — a founder-authored Park targeting the grant's
        // `granted_by` entry id, exactly what `junto revoke-member`
        // constructs (main.rs). Appended directly on the ledger: this is
        // the act, not something `Host::add_member` performs.
        let (id, view) = project(&host, "acme").await;
        let grant_id = view
            .keyring
            .get("alice@example.com")
            .and_then(|grants| grants.first())
            .expect("alice has a grant to revoke")
            .granted_by;
        let mut park = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: id,
            author: dan(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::Park {
                target: grant_id,
                rationale: "device lost".into(),
            },
        };
        host.sign_entry(&mut park);
        let Resolution::Resolved { ledger, .. } = host.resolve("acme").await.unwrap() else {
            panic!("channel 'acme' resolves");
        };
        ledger.lock().await.append(park).await.unwrap();

        // The park must actually have retired the grant, or the rest of
        // this test would exercise nothing.
        let (_, view) = project(&host, "acme").await;
        assert!(
            view.keyring["alice@example.com"][0].retired_at.is_some(),
            "the park must retire alice's only grant before re-enrollment is exercised"
        );

        // Re-enroll the SAME device (the same key) after revocation.
        host.add_member(
            "acme",
            &dan(),
            Member::human("Alice", "alice@example.com"),
            Some(key_a.public_key()),
            None,
        )
        .await
        .unwrap();

        let (_, view) = project(&host, "acme").await;
        let alice_grants = view
            .keyring
            .get("alice@example.com")
            .expect("alice has grants");
        assert_eq!(
            alice_grants.len(),
            2,
            "re-granting after revocation appends a new grant alongside the retired one: \
             {alice_grants:?}"
        );
        assert!(
            alice_grants
                .iter()
                .any(|g| g.key == key_a.public_key() && g.retired_at.is_none()),
            "alice's re-enrollment leaves an ACTIVE grant for her key: {alice_grants:?}"
        );
    }

    /// `docs/superpowers/specs/2026-08-21-device-key-enrollment-design.md`
    /// line 61: "the founder's own second device works naturally — they
    /// author a `MemberAdded` for their own email carrying the new key."
    /// Pinned as a contract, not just verified by inspection: the founder
    /// is always on the Party (so the guard is entered), and
    /// `project_keyring` grants on `entry.author.email == founder_email`
    /// with no self-exclusion, so nothing here needs a special case.
    #[tokio::test]
    async fn add_member_records_the_founders_own_second_device() {
        let (_dirs, host) = lineage_host(1);
        host.open_channel(None, "acme", dan(), None).await.unwrap();
        let (_, view_before) = project(&host, "acme").await;
        let founder_key_before = view_before.keyring["dan@example.com"][0].key.clone();

        let key_b = junto_kernel::SigningKey::from_secret_bytes([13; 32]);
        assert_ne!(
            founder_key_before,
            key_b.public_key(),
            "the fixture's second device key must genuinely differ from the founder's first"
        );

        // The founder grants membership to themself, carrying a second
        // device's key.
        host.add_member("acme", &dan(), dan(), Some(key_b.public_key()), None)
            .await
            .unwrap();

        let (_, view) = project(&host, "acme").await;
        let dan_grants = view
            .keyring
            .get("dan@example.com")
            .expect("dan has keyring grants");
        assert_eq!(
            dan_grants.len(),
            2,
            "the founder's genesis key plus their second device's key: {dan_grants:?}"
        );
        let dan_rows: Vec<_> = view
            .party
            .iter()
            .filter(|m| m.email == "dan@example.com")
            .collect();
        assert_eq!(
            dan_rows.len(),
            1,
            "the founder still has exactly one party row, not a duplicate for their second \
             device: {dan_rows:?}"
        );
    }

    #[tokio::test]
    async fn diverge_opens_child_and_records_both_edges() {
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "parent", dan(), None)
            .await
            .unwrap();
        let code = code_for(&dirs, &dan());

        let child = host
            .diverge(
                "parent",
                "side-quest",
                None,
                dan(),
                WriteAuth::Agent(Some(&code)),
            )
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
            .diverge(
                "parent",
                "sq",
                None,
                stranger,
                WriteAuth::Agent(Some(&code)),
            )
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

        host.converge(
            "src",
            "tgt",
            "merged the side-quest",
            dan(),
            WriteAuth::Agent(Some(&code)),
        )
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
                signature: None,
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
                    kind: None,
                },
            })
            .await
            .unwrap();

        let err = host
            .converge("src", "tgt", "merge", dan(), WriteAuth::Agent(Some(&code)))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("open gate"), "{err}");

        // And the source is NOT closed — the refusal left it untouched.
        let (_, src_view) = project(&host, "src").await;
        assert!(!src_view.closed);
    }

    #[tokio::test]
    async fn approved_actionable_gate_surfaces_until_executed() {
        use junto_kernel::{ApprovalRequirement, EntryPayload};
        let (dirs, host) = lineage_host(1);
        host.open_channel(None, "c", dan(), None).await.unwrap();
        let _ = &dirs;
        let (id, _) = project(&host, "c").await;
        let ledger = host
            .ledger_for(host.substrate_paths().unwrap()[0].as_path())
            .await
            .unwrap();

        // An approved, actionable (kind-tagged) gate.
        let proposal = EntryId::new();
        for payload in [
            EntryPayload::Proposal {
                action: "Open the PR".into(),
                rationale: "verified".into(),
                provenance: vec![],
                requirement: ApprovalRequirement::Count(1),
                frame: None,
                kind: Some("code-pr.open-pr".into()),
            },
            EntryPayload::Approval {
                target: proposal,
                rationale: "go".into(),
            },
        ] {
            let eid = if matches!(payload, EntryPayload::Proposal { .. }) {
                proposal
            } else {
                EntryId::new()
            };
            ledger
                .lock()
                .await
                .append(LedgerEntry {
                    signature: None,
                    id: eid,
                    channel: id,
                    author: dan(),
                    timestamp: Timestamp::now(),
                    payload,
                })
                .await
                .unwrap();
        }

        let view = ledger.lock().await.project(&id).await.unwrap();
        let group = attention_for_view(&id, &view);
        assert!(
            group
                .items
                .iter()
                .any(|i| i.kind == AttentionKind::AwaitingExecution),
            "an approved actionable gate with no execution surfaces"
        );

        // A successful GateExecuted clears it.
        ledger
            .lock()
            .await
            .append(LedgerEntry {
                signature: None,
                id: EntryId::new(),
                channel: id,
                author: dan(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::GateExecuted {
                    target: proposal,
                    success: true,
                    note: "pr #1".into(),
                },
            })
            .await
            .unwrap();
        let view = ledger.lock().await.project(&id).await.unwrap();
        let group = attention_for_view(&id, &view);
        assert!(
            !group
                .items
                .iter()
                .any(|i| i.kind == AttentionKind::AwaitingExecution),
            "a successful execution clears the signal"
        );
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
                signature: None,
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
                signature: None,
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
            .diverge("parent", "sq", None, dan(), WriteAuth::Agent(Some(&code)))
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
            signature: None,
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
            signature: None,
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
