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
        let out = std::process::Command::new("git")
            .args(["-C", &repo.display().to_string(), "config", key])
            .output()
            .context("running git config")?;
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
    /// Ledgers opened so far, keyed by substrate repo path — cached so each
    /// repo has one append-serializing mutex for the host's lifetime.
    ledgers: Mutex<HashMap<PathBuf, SharedLedger>>,
}

impl Host {
    /// A host over the machine registry under `junto_home` (`docs/adr/0015`).
    pub fn from_registry(junto_home: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            substrates: Substrates::Registry(junto_home),
            ledgers: Mutex::new(HashMap::new()),
        })
    }

    /// A host over a fixed substrate set (single-repo dev mode, tests).
    pub fn fixed(repos: Vec<PathBuf>) -> Arc<Self> {
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
            ledgers: Mutex::new(HashMap::new()),
        })
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

    /// Every channel across every served substrate, projected into summaries.
    pub async fn inventory(&self) -> Result<Vec<ChannelSummary>> {
        let mut summaries = Vec::new();
        for repo in self.substrate_paths()? {
            let ledger = self.ledger_for(&repo).await?;
            let guard = ledger.lock().await;
            for id in guard.substrate().channels().await? {
                let view = guard.project(&id).await?;
                summaries.push(summarize(&id, &view, &repo));
            }
        }
        Ok(summaries)
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
    /// `ChannelOpened` genesis entry.
    ///
    /// `repo`: the home substrate; may be omitted when the host serves exactly
    /// one. Returns the opened channel's id.
    pub async fn open_channel(
        &self,
        repo: Option<&Path>,
        name: &str,
        opened_by: Member,
        declared_id: Option<ChannelId>,
    ) -> Result<ChannelId> {
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
                author: opened_by,
                timestamp: Timestamp::now(),
                payload: EntryPayload::ChannelOpened {
                    name: name.to_string(),
                },
            })
            .await?;
        Ok(id)
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
    }
}
