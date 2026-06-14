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
    SubstrateProvider, Timestamp,
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
        self.authorize_human_write(view, author)?;
        if view.party.is_empty() {
            return Ok(());
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
        if !view.party.iter().any(|member| member.email == author.email) {
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

/// Fold one projected channel into its discovery summary.
fn summarize(id: &ChannelId, view: &ChannelView, substrate: &Path) -> ChannelSummary {
    ChannelSummary {
        id: *id,
        name: view.name.clone(),
        substrate: substrate.to_path_buf(),
        entry_count: view.entries.len(),
        last_activity: view.entries.iter().map(|entry| entry.timestamp).max(),
        open_gates: view
            .gate_status
            .values()
            .filter(|status| **status == GateStatus::Pending)
            .count(),
        members: view.party.len(),
        latest: view.entries.last().map(preview),
        closed: view.closed,
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
