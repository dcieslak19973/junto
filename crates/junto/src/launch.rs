//! Launching Agent Sessions from the surface (`docs/adr/0023`).
//!
//! The **Workspace** is the machine-local mapping channel → repo(s) — where a
//! channel's Agent Sessions execute (`~/.junto/workspaces.toml`). Paths never
//! enter the ledger: they are machine facts and don't sync. The harness
//! session-id mapping (`~/.junto/harness-sessions.toml`) is machine-local for
//! the same reason.
//!
//! A turn runs the harness over **ACP** (`docs/adr/0024`, see [`crate::acp`])
//! when available, falling back to the **`claude -p` oneshot-exec** CLI here.
//! Either way the host parses the result, attaches the result memo + workspace
//! `git diff` as artifacts (content written under `~/.junto/artifacts/`,
//! referenced by `file://` URI + sha256 digest — never blobs in the ledger),
//! and marks the session done/error. Steering is a later resume turn
//! (ACP `session/load` or `claude -p --resume <harness-session-id>`); state
//! lives in the harness's own session storage, so host restarts are harmless.
//! ACP's capability flags stand in for an `AgentHarnessAdapter` trait — one
//! client, many harnesses (`docs/adr/0024`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use tokio::sync::{broadcast, mpsc};

use junto_kernel::{
    ChannelId, ContentDigest, EntryId, EntryPayload, LedgerEntry, Member, ProvenanceRef,
    SessionState, Timestamp, Uri,
};

use crate::host::Host;

/// A registered agent harness — its member identity and how junto drives it
/// (`docs/adr/0024`). Adding one is *data here*, not a new code path: junto's
/// ACP client is identical for all; the differences are the adapter command
/// and the agent's identity. This is what ACP's capability model gives us in
/// place of a per-vendor trait.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Harness {
    /// Stable id used in forms and the session mapping (e.g. `claude`).
    pub(crate) id: &'static str,
    /// Display label and the agent member's name (e.g. `Claude Code`).
    pub(crate) label: &'static str,
    /// The agent member's stable email identity.
    pub(crate) email: &'static str,
}

/// Every harness junto can drive. Claude is the default (first).
const HARNESSES: &[Harness] = &[
    Harness {
        id: "claude",
        label: "Claude Code",
        email: "claude-code@anthropic.com",
    },
    Harness {
        id: "opencode",
        label: "OpenCode",
        email: "opencode@opencode.ai",
    },
];

impl Harness {
    /// The agent's member identity. Agents now own authorship
    /// ([`crate::agent::Agent::member`]); this remains for the stock-Claude
    /// identity tests assert against.
    #[cfg(test)]
    pub(crate) fn member(&self) -> Member {
        Member::agent(self.label, self.email)
    }

    /// The ACP adapter command for this harness (OS-aware), or `None` if it has
    /// no ACP entry point. Each is overridable via its env var.
    fn acp_command(&self) -> Option<Vec<String>> {
        let (default, var) = match self.id {
            // Claude's adapter runs Claude Code's SDK (same subscription auth);
            // on Windows the launcher is `npx.cmd` (Rust won't append `.cmd`).
            "claude" => (
                if cfg!(windows) {
                    "npx.cmd -y @agentclientprotocol/claude-agent-acp"
                } else {
                    "npx -y @agentclientprotocol/claude-agent-acp"
                },
                "JUNTO_ACP_CLAUDE_CMD",
            ),
            // OpenCode speaks ACP natively — no adapter package.
            "opencode" => (
                if cfg!(windows) {
                    "opencode.cmd acp"
                } else {
                    "opencode acp"
                },
                "JUNTO_ACP_OPENCODE_CMD",
            ),
            _ => return None,
        };
        let cmd = std::env::var(var).unwrap_or_else(|_| default.to_string());
        let parts: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
        if parts.is_empty() { None } else { Some(parts) }
    }

    /// Whether this harness has a non-ACP CLI fallback (`claude -p`). Only
    /// Claude does; the rest are ACP-only.
    fn has_cli_fallback(&self) -> bool {
        self.id == "claude"
    }

    /// A one-line description of how junto reaches this harness, for settings.
    pub(crate) fn adapter_summary(&self) -> String {
        match self.acp_command() {
            Some(command) => format!("ACP — {}", command.join(" ")),
            None if self.has_cli_fallback() => "claude -p (CLI)".to_string(),
            None => "(no adapter)".to_string(),
        }
    }
}

/// The harness for an id, or the default (Claude) when unknown/empty.
pub(crate) fn harness_by_id(id: &str) -> Harness {
    HARNESSES
        .iter()
        .copied()
        .find(|harness| harness.id == id)
        .unwrap_or(HARNESSES[0])
}

/// Every registered harness (for settings and the agent form's harness
/// picker). The established agent per channel is now resolved at the agent
/// layer (`crate::agent::channel_agent`).
pub(crate) fn all_harnesses() -> &'static [Harness] {
    HARNESSES
}

/// The default harness's member identity — the stock Claude agent authors as
/// this, so tests assert against it.
#[cfg(test)]
pub fn harness_member() -> Member {
    HARNESSES[0].member()
}

/// The harness command line, overridable for tests (`JUNTO_HARNESS_CMD`
/// names a program that accepts the same trailing arguments and prints a
/// `claude -p --output-format stream-json`-shaped result).
fn harness_program() -> String {
    std::env::var("JUNTO_HARNESS_CMD").unwrap_or_else(|_| "claude".to_string())
}

// ---- the ExecutionBackend: where the harness runs (docs/adr/0023) ----
//
// On Windows a native `claude.exe` flashes a console window for every Bash
// tool call — an upstream Claude Code bug (anthropics/claude-code#15572 and
// friends), and one a pseudo-terminal does *not* fix (the bug reproduces in
// interactive/PTY mode). Running the harness inside WSL makes those Linux
// processes, so no Windows console windows exist to flash. We prefer WSL when
// a distro actually has `claude`, and fall back to native otherwise — with a
// suggestion to set WSL up. This is the first concrete ExecutionBackend; the
// trait waits for a second one (rule of three).

/// Where the harness runs on this machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HarnessBackend {
    /// The harness binary directly (`claude` on PATH, or `JUNTO_HARNESS_CMD`).
    Native,
    /// `claude` inside the default WSL distro — Linux processes, no flashing.
    Wsl,
}

/// The resolved backend plus a one-line suggestion shown on the start-work
/// surface when we fell back to native on Windows (else `None`).
struct HarnessChoice {
    backend: HarnessBackend,
    hint: Option<&'static str>,
}

/// The machine's resolved harness backend, detected once and cached (machine
/// facts don't change mid-run).
static HARNESS_CHOICE: OnceLock<HarnessChoice> = OnceLock::new();

/// The machine's harness backend, detecting it if needed. Detection probes are
/// quiet (no flashed window) and bounded (a wedged WSL can't hang a launch),
/// but can still take a second or two — callers off the render path (a launch)
/// may block on it.
fn harness_choice() -> &'static HarnessChoice {
    HARNESS_CHOICE.get_or_init(detect_harness_choice)
}

/// The harness suggestion for the human surface, if any. **Non-blocking**:
/// detection shells out to WSL, so the render path must never wait on it — if
/// it hasn't run yet, warm it off-thread and show nothing this time (the next
/// page load has it).
pub(crate) fn harness_hint() -> Option<&'static str> {
    use std::sync::atomic::{AtomicBool, Ordering};
    if let Some(choice) = HARNESS_CHOICE.get() {
        return choice.hint;
    }
    static WARMING: AtomicBool = AtomicBool::new(false);
    if !WARMING.swap(true, Ordering::SeqCst) {
        std::thread::spawn(|| {
            let _ = harness_choice();
        });
    }
    None
}

/// A read-only snapshot of how this machine runs the harness, for the settings
/// page (`docs/adr/0023`/`0024`). Non-blocking — the WSL probe is only read if
/// already detected.
pub(crate) struct HarnessStatus {
    /// `ACP` or `claude -p (CLI)`.
    pub(crate) protocol: &'static str,
    /// A detail line: the ACP adapter command, or why ACP is off.
    pub(crate) detail: String,
    /// `native`, `WSL`, or `detecting…`.
    pub(crate) backend: &'static str,
    /// How the harness authenticates — read-only status, never a stored key
    /// (auth stays with the harness, `docs/adr/0024`).
    pub(crate) auth: &'static str,
    /// The flashing/setup suggestion, if any.
    pub(crate) hint: Option<&'static str>,
}

/// Build the harness status shown on the settings page.
pub(crate) fn harness_status() -> HarnessStatus {
    let (protocol, detail) = match acp_adapter_command(HARNESSES[0]) {
        Some(command) => ("ACP", format!("adapter: {}", command.join(" "))),
        None if std::env::var("JUNTO_HARNESS_PROTOCOL").ok().as_deref() == Some("cli") => (
            "claude -p (CLI)",
            "ACP disabled (JUNTO_HARNESS_PROTOCOL=cli)".to_string(),
        ),
        None => (
            "claude -p (CLI)",
            "ACP unavailable (Node not found) — using the claude -p fallback".to_string(),
        ),
    };
    // Read the backend only if already detected; otherwise warm it off-thread.
    let backend = match HARNESS_CHOICE.get() {
        Some(choice) => match choice.backend {
            HarnessBackend::Native => "native",
            HarnessBackend::Wsl => "WSL",
        },
        None => "detecting…",
    };
    HarnessStatus {
        protocol,
        detail,
        backend,
        auth: claude_auth_mode(),
        hint: harness_hint(),
    }
}

/// Detect how Claude Code will authenticate, **read-only** — junto never
/// stores a credential; the harness owns its auth (`docs/adr/0024`). Mirrors
/// Claude Code's own precedence: cloud routing flags, then a gateway base-url,
/// then a direct key/token, else the subscription login.
fn claude_auth_mode() -> &'static str {
    let flag = |key: &str| {
        std::env::var(key)
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    };
    let present = |key: &str| std::env::var_os(key).is_some_and(|v| !v.is_empty());
    if flag("CLAUDE_CODE_USE_BEDROCK") {
        "Claude via AWS Bedrock"
    } else if flag("CLAUDE_CODE_USE_VERTEX") {
        "Claude via Google Vertex"
    } else if flag("CLAUDE_CODE_USE_FOUNDRY") {
        "Claude via Microsoft Foundry"
    } else if present("ANTHROPIC_BASE_URL") {
        "Claude via a gateway (ANTHROPIC_BASE_URL)"
    } else if present("ANTHROPIC_API_KEY") || present("ANTHROPIC_AUTH_TOKEN") {
        "Claude: API key"
    } else {
        "Claude: subscription login (no API key)"
    }
}

fn detect_harness_choice() -> HarnessChoice {
    // A test/override stub always runs natively (and never probes WSL).
    if std::env::var_os("JUNTO_HARNESS_CMD").is_some() {
        return HarnessChoice {
            backend: HarnessBackend::Native,
            hint: None,
        };
    }
    match std::env::var("JUNTO_HARNESS_BACKEND").ok().as_deref() {
        Some("native") => {
            return HarnessChoice {
                backend: HarnessBackend::Native,
                hint: None,
            };
        }
        Some("wsl") => {
            return HarnessChoice {
                backend: HarnessBackend::Wsl,
                hint: None,
            };
        }
        _ => {}
    }
    // Auto-detect only in real builds — tests must never shell out to WSL
    // (slow, machine-dependent). A forced backend via env still works above.
    #[cfg(all(windows, not(test)))]
    {
        if !wsl_has_distro() {
            HarnessChoice {
                backend: HarnessBackend::Native,
                hint: Some(
                    "Console windows flash during agent turns — an upstream Claude Code bug \
                     on Windows. Install WSL (run `wsl --install`) and Claude Code inside it; \
                     junto will run the harness there and the flashing stops.",
                ),
            }
        } else if wsl_has_claude() {
            HarnessChoice {
                backend: HarnessBackend::Wsl,
                hint: None,
            }
        } else {
            HarnessChoice {
                backend: HarnessBackend::Native,
                hint: Some(
                    "Console windows flash during agent turns — an upstream Claude Code bug on \
                     Windows. WSL is installed but Claude Code isn't inside it; install \
                     `claude` in your WSL distro (and sign in there) and junto will run the \
                     harness there.",
                ),
            }
        }
    }
    #[cfg(not(all(windows, not(test))))]
    {
        HarnessChoice {
            backend: HarnessBackend::Native,
            hint: None,
        }
    }
}

/// Does WSL have at least one installed distro? `wsl -l -q` exits non-zero
/// when WSL is absent or empty, and is fast (no distro boot).
#[cfg(all(windows, not(test)))]
fn wsl_has_distro() -> bool {
    let mut command = std::process::Command::new("wsl");
    command.args(["-l", "-q"]);
    no_console_window(&mut command);
    command
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Is `claude` runnable inside the default WSL distro? Booting the distro can
/// take a moment, so the probe is bounded — a wedged WSL reads as "absent".
#[cfg(all(windows, not(test)))]
fn wsl_has_claude() -> bool {
    run_bounded(|| {
        let mut command = std::process::Command::new("wsl");
        command.args(["--", "claude", "--version"]);
        no_console_window(&mut command);
        command
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

/// Run a blocking probe on a worker thread, giving up after a few seconds so
/// a broken WSL can never wedge backend detection.
#[cfg(all(windows, not(test)))]
fn run_bounded(probe: impl FnOnce() -> bool + Send + 'static) -> Option<bool> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(probe());
    });
    rx.recv_timeout(std::time::Duration::from_secs(15)).ok()
}

/// The base harness command for `workspace`, per the detected backend. The
/// caller appends the shared `claude` arguments (`-p`, `--output-format …`).
fn harness_command(workspace: &Path) -> tokio::process::Command {
    match harness_choice().backend {
        HarnessBackend::Native => {
            let mut command = tokio::process::Command::new(harness_program());
            command.current_dir(workspace);
            command
        }
        HarnessBackend::Wsl => {
            // `--cd` takes the Windows workspace path and translates it; the
            // harness then runs as a Linux process (no flashing console).
            let mut command = tokio::process::Command::new("wsl");
            command.arg("--cd").arg(workspace).arg("--").arg("claude");
            command
        }
    }
}

/// How long a turn may run before the host kills it (docs/adr/0023).
pub(crate) const TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

// ---- the Workspace store (channel → repos; machine config) ----

#[derive(Debug, Default, Serialize, Deserialize)]
struct WorkspacesFile {
    #[serde(default)]
    workspaces: Vec<WorkspaceRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkspaceRecord {
    channel: ChannelId,
    /// List-shaped so one channel can span several repos later; v1 uses
    /// exactly one (docs/adr/0023).
    repos: Vec<PathBuf>,
}

fn workspaces_path(junto_home: &Path) -> PathBuf {
    junto_home.join("workspaces.toml")
}

/// The stored workspace repo for a channel, if one was remembered.
pub fn workspace_for(junto_home: &Path, channel: &ChannelId) -> Result<Option<PathBuf>> {
    let path = workspaces_path(junto_home);
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let file: WorkspacesFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(file
        .workspaces
        .into_iter()
        .find(|record| record.channel == *channel)
        .and_then(|record| record.repos.into_iter().next()))
}

/// Every remembered (channel → repo) mapping. Used to infer a default
/// workspace for a fresh channel from what's been used recently elsewhere.
pub fn all_workspaces(junto_home: &Path) -> Result<Vec<(ChannelId, PathBuf)>> {
    let path = workspaces_path(junto_home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let file: WorkspacesFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(file
        .workspaces
        .into_iter()
        .filter_map(|record| {
            record
                .repos
                .into_iter()
                .next()
                .map(|repo| (record.channel, repo))
        })
        .collect())
}

/// Remember (or update) a channel's workspace repo.
pub fn remember_workspace(junto_home: &Path, channel: &ChannelId, repo: &Path) -> Result<()> {
    let repo = dunce::canonicalize(repo)
        .with_context(|| format!("workspace repo {} not found", repo.display()))?;
    if !repo.join(".git").exists() {
        bail!(
            "{} is not a git repository (v1 workspaces must be git repos — diff capture \
             depends on it; docs/adr/0023)",
            repo.display()
        );
    }
    let path = workspaces_path(junto_home);
    let mut file: WorkspacesFile = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
    } else {
        WorkspacesFile::default()
    };
    match file
        .workspaces
        .iter_mut()
        .find(|record| record.channel == *channel)
    {
        Some(record) => record.repos = vec![repo],
        None => file.workspaces.push(WorkspaceRecord {
            channel: *channel,
            repos: vec![repo],
        }),
    }
    std::fs::create_dir_all(junto_home)
        .with_context(|| format!("creating {}", junto_home.display()))?;
    std::fs::write(
        &path,
        toml::to_string_pretty(&file).context("serializing workspaces")?,
    )
    .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// ---- the harness session-id mapping (junto session → harness session) ----

#[derive(Debug, Default, Serialize, Deserialize)]
struct HarnessSessionsFile {
    #[serde(default)]
    sessions: Vec<HarnessSessionRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct HarnessSessionRecord {
    /// The junto session — the `SessionStarted` entry's id.
    junto: EntryId,
    /// The harness's own session id (what resume takes).
    harness: String,
    /// Which harness produced it (`claude`, `opencode`) — so steering resumes
    /// the same one. Defaults to `claude` for records written before
    /// multi-harness support.
    #[serde(default = "default_harness_id")]
    harness_id: String,
    /// Which agent ran it — so steering rebuilds the same agent (its
    /// identity + config). Empty for records written before agents existed;
    /// steering then falls back to the stock agent for `harness_id`.
    #[serde(default)]
    agent_slug: String,
    /// Turns run so far (names the artifact files).
    turns: u32,
}

fn default_harness_id() -> String {
    "claude".to_string()
}

fn harness_sessions_path(junto_home: &Path) -> PathBuf {
    junto_home.join("harness-sessions.toml")
}

fn load_harness_sessions(junto_home: &Path) -> Result<HarnessSessionsFile> {
    let path = harness_sessions_path(junto_home);
    if !path.exists() {
        return Ok(HarnessSessionsFile::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn save_harness_sessions(junto_home: &Path, file: &HarnessSessionsFile) -> Result<()> {
    std::fs::create_dir_all(junto_home)
        .with_context(|| format!("creating {}", junto_home.display()))?;
    let path = harness_sessions_path(junto_home);
    std::fs::write(
        &path,
        toml::to_string_pretty(file).context("serializing harness sessions")?,
    )
    .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// The recorded harness session id for a junto session, if any.
pub fn harness_session_for(junto_home: &Path, junto: &EntryId) -> Result<Option<String>> {
    Ok(load_harness_sessions(junto_home)?
        .sessions
        .into_iter()
        .find(|record| record.junto == *junto)
        .map(|record| record.harness))
}

/// Which harness ran a junto session, if recorded — so steering resumes the
/// same one (`docs/adr/0024`).
pub(crate) fn harness_id_for(junto_home: &Path, junto: &EntryId) -> Result<Option<String>> {
    Ok(load_harness_sessions(junto_home)?
        .sessions
        .into_iter()
        .find(|record| record.junto == *junto)
        .map(|record| record.harness_id))
}

/// Which agent ran a junto session, if recorded (empty when pre-agents).
pub(crate) fn agent_slug_for(junto_home: &Path, junto: &EntryId) -> Result<Option<String>> {
    Ok(load_harness_sessions(junto_home)?
        .sessions
        .into_iter()
        .find(|record| record.junto == *junto)
        .map(|record| record.agent_slug)
        .filter(|slug| !slug.is_empty()))
}

fn record_turn(
    junto_home: &Path,
    junto: &EntryId,
    harness: Option<String>,
    harness_id: &str,
    agent_slug: &str,
) -> Result<u32> {
    let mut file = load_harness_sessions(junto_home)?;
    let turn = match file.sessions.iter_mut().find(|r| r.junto == *junto) {
        Some(record) => {
            record.turns += 1;
            if let Some(harness) = harness {
                record.harness = harness;
            }
            record.harness_id = harness_id.to_string();
            record.agent_slug = agent_slug.to_string();
            record.turns
        }
        None => {
            file.sessions.push(HarnessSessionRecord {
                junto: *junto,
                harness: harness.unwrap_or_default(),
                harness_id: harness_id.to_string(),
                agent_slug: agent_slug.to_string(),
                turns: 1,
            });
            1
        }
    };
    save_harness_sessions(junto_home, &file)?;
    Ok(turn)
}

// ---- live progress: an ephemeral feed of the running turn (docs/adr/0023) ----
//
// A running turn streams structured progress (assistant text, named tool
// actions) so the human can watch it work instead of staring at "working".
// This is **not the record**: it lives in memory, never the ledger — the
// durable capture stays the memo + diff artifacts (CLAUDE.md terminal-less:
// the verifiable Artifact is the record; this feed is a transient window that
// vanishes when the turn lands). It is also the normalized event shape a
// future `AgentHarnessAdapter` will converge on.

/// One line in a session's live progress feed.
#[derive(Clone, Debug, Serialize)]
pub struct LiveEvent {
    /// `status` (lifecycle), `assistant` (model text), `thinking` (model
    /// reasoning), `tool` (a named action), `result` (final summary), or
    /// `error`.
    pub kind: String,
    /// The line's content. For `assistant`/`thinking` segments this is
    /// **sanitized HTML** (`html == true`, rendered server-side from Markdown);
    /// otherwise a plain string.
    pub text: String,
    /// Segment id. `0` = a discrete line (always appended). A non-zero `seq`
    /// marks a growing block: successive events with the same `seq` **replace**
    /// the prior one (a Markdown segment re-rendered as it streams).
    #[serde(default)]
    pub seq: u64,
    /// When true, `text` is sanitized HTML the client sets via `innerHTML`;
    /// otherwise plain text set via `textContent`.
    #[serde(default)]
    pub html: bool,
    /// The raw Markdown behind a rendered segment, for clients that render
    /// Markdown themselves (the native app) instead of consuming `text`'s HTML.
    /// `None` for plain lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
}

impl LiveEvent {
    /// A discrete plain-text line (status / result / error / tool).
    pub(crate) fn new(kind: &str, text: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            text: text.into(),
            seq: 0,
            html: false,
            markdown: None,
        }
    }

    /// A growing Markdown segment: `text` is sanitized HTML and `markdown` the
    /// raw source, keyed by `seq` so the client replaces the block in place as
    /// it streams.
    pub(crate) fn segment(
        kind: &str,
        markdown: impl Into<String>,
        html: impl Into<String>,
        seq: u64,
    ) -> Self {
        Self {
            kind: kind.into(),
            text: html.into(),
            seq,
            html: true,
            markdown: Some(markdown.into()),
        }
    }

    /// A discrete line carrying an explicit `seq` (a tool block that a later
    /// `tool_call_update` can replace in place).
    pub(crate) fn line_seq(kind: &str, text: impl Into<String>, seq: u64) -> Self {
        Self {
            kind: kind.into(),
            text: text.into(),
            seq,
            html: false,
            markdown: None,
        }
    }
}

/// A human's mid-turn signal to a running turn — the reverse direction of the
/// live feed (human → turn). Delivered over a per-session control channel and
/// acted on by the turn driver's `select!` loop (`docs/adr/0032`).
#[derive(Debug)]
pub(crate) enum TurnControl {
    /// Stop the current prompt and end the turn.
    Interrupt,
    /// Stop the current prompt and re-prompt in place with this text.
    Steer(String),
}

/// No turn is currently live for the session, so control could not be
/// delivered. The caller falls back to the between-turns resume path.
#[derive(Debug)]
pub(crate) struct NotLive;

/// Per-session live feed: a bounded replay buffer (for a page that loads
/// mid-turn), a broadcast sender for the live tail (host → human), and a
/// control sender for mid-turn signals (human → turn).
struct LiveFeed {
    buffer: Vec<LiveEvent>,
    sender: broadcast::Sender<LiveEvent>,
    control: mpsc::Sender<TurnControl>,
}

/// The host's in-memory registry of running sessions' live feeds. Ephemeral —
/// nothing here is part of the durable record.
#[derive(Default)]
pub struct LiveSessions {
    inner: Mutex<HashMap<EntryId, LiveFeed>>,
}

impl LiveSessions {
    /// Open a fresh feed for a session about to run (replaces any stale one),
    /// returning the control receiver the running turn selects on (human →
    /// turn).
    pub(crate) fn begin(&self, session: EntryId) -> mpsc::Receiver<TurnControl> {
        let (sender, _rx) = broadcast::channel(256);
        // Capacity 1: one human, one in-flight signal at a time.
        let (control, control_rx) = mpsc::channel(1);
        let mut map = self.inner.lock().expect("live sessions registry lock");
        map.insert(
            session,
            LiveFeed {
                buffer: Vec::new(),
                sender,
                control,
            },
        );
        control_rx
    }

    /// Deliver a human's control signal to the running turn, or `Err(NotLive)`
    /// if no turn is currently streaming for the session.
    pub(crate) fn control(&self, session: EntryId, signal: TurnControl) -> Result<(), NotLive> {
        let map = self.inner.lock().expect("live sessions registry lock");
        let feed = map.get(&session).ok_or(NotLive)?;
        feed.control.try_send(signal).map_err(|_| NotLive)
    }

    /// Append an event: into the replay buffer (bounded) and to live tails.
    /// A non-zero `seq` marks a growing segment — successive same-`seq` events
    /// **coalesce** in the replay buffer (the last one wins, so a late joiner
    /// sees one rendered block, not every intermediate frame), which also keeps
    /// a long Markdown stream from blowing the bound. Live subscribers still
    /// receive every frame.
    pub(crate) fn publish(&self, session: EntryId, event: LiveEvent) {
        let mut map = self.inner.lock().expect("live sessions registry lock");
        if let Some(feed) = map.get_mut(&session) {
            let coalesce =
                event.seq != 0 && feed.buffer.last().is_some_and(|last| last.seq == event.seq);
            if coalesce {
                if let Some(last) = feed.buffer.last_mut() {
                    *last = event.clone();
                }
            } else if feed.buffer.len() < 1000 {
                feed.buffer.push(event.clone());
            }
            // Err just means no one is watching right now — fine.
            let _ = feed.sender.send(event);
        }
    }

    /// Subscribe to a running session: its replay buffer plus a live receiver,
    /// or `None` if no turn is currently streaming for it.
    pub fn subscribe(
        &self,
        session: EntryId,
    ) -> Option<(Vec<LiveEvent>, broadcast::Receiver<LiveEvent>)> {
        let map = self.inner.lock().expect("live sessions registry lock");
        let feed = map.get(&session)?;
        Some((feed.buffer.clone(), feed.sender.subscribe()))
    }

    /// Close a session's feed — dropping the sender, so any live subscriber
    /// sees the stream end and reloads to the now-persisted outcome.
    fn finish(&self, session: EntryId) {
        let mut map = self.inner.lock().expect("live sessions registry lock");
        map.remove(&session);
    }
}

/// What interpreting one `stream-json` line yielded: progress events to show,
/// plus any harness-session id and final result it carried.
#[derive(Default)]
struct LineEffects {
    events: Vec<LiveEvent>,
    session: Option<String>,
    result: Option<String>,
    is_error: bool,
    saw_result: bool,
}

/// A short label for a tool action, e.g. `Bash: cargo test` — the first
/// salient input field, never the whole payload (terminal-less: a glanceable
/// action, not scrollback).
fn tool_summary(name: &str, input: Option<&serde_json::Value>) -> String {
    let detail = input.and_then(|i| {
        [
            "command",
            "file_path",
            "path",
            "pattern",
            "url",
            "description",
        ]
        .iter()
        .find_map(|key| i.get(*key).and_then(|v| v.as_str()))
    });
    match detail {
        Some(d) => {
            let first = d.lines().next().unwrap_or(d);
            format!("{name}: {}", snippet(first, 80))
        }
        None => name.to_string(),
    }
}

/// Interpret one line of `claude -p --output-format stream-json` (JSONL).
/// Lenient: an unrecognized line yields nothing rather than failing the turn.
fn interpret_stream_line(line: &str) -> LineEffects {
    let mut effects = LineEffects::default();
    let line = line.trim();
    if line.is_empty() {
        return effects;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return effects;
    };
    if let Some(session) = value.get("session_id").and_then(|v| v.as_str()) {
        effects.session = Some(session.to_string());
    }
    match value.get("type").and_then(|t| t.as_str()) {
        Some("system") => {
            effects
                .events
                .push(LiveEvent::new("status", "session started"));
        }
        Some("assistant") => {
            if let Some(blocks) = value.pointer("/message/content").and_then(|c| c.as_array()) {
                for block in blocks {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str())
                                && !text.trim().is_empty()
                            {
                                effects
                                    .events
                                    .push(LiveEvent::new("assistant", text.trim()));
                            }
                        }
                        Some("tool_use") => {
                            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                            effects.events.push(LiveEvent::new(
                                "tool",
                                tool_summary(name, block.get("input")),
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("result") => {
            effects.saw_result = true;
            effects.is_error = value
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let text = value
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            effects.events.push(LiveEvent::new(
                if effects.is_error { "error" } else { "result" },
                snippet(&text, 240),
            ));
            effects.result = Some(text);
        }
        // "user" carries tool results — skipped to keep the feed glanceable.
        _ => {}
    }
    effects
}

// ---- the turn itself: spawn → capture → record ----

/// How a turn ended — folds the old `failed: bool` into the distinct cases that
/// matter for the recorded session state (`docs/adr/0032`). An interrupt is a
/// human choice, not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnEnd {
    /// The agent finished its turn normally (`stopReason == "end_turn"`).
    Completed,
    /// A human interrupted the turn mid-flight.
    Interrupted,
    /// The agent errored, exited non-zero, or produced unparseable output.
    Failed,
    /// The turn exceeded its timeout and was killed.
    TimedOut,
}

/// What one finished harness turn yielded.
pub(crate) struct TurnOutcome {
    /// The result text (the harness's final message, or the failure tail).
    pub(crate) result: String,
    /// The harness's session id, when the output carried one.
    pub(crate) harness_session: Option<String>,
    /// How the turn ended.
    pub(crate) end: TurnEnd,
}

/// Map a finished turn's end-state to the session state + note recorded for it.
/// An interrupt lands the session `Done` (a human choice, not an error).
fn outcome_state(end: TurnEnd, turn: u32, result: &str) -> (SessionState, String) {
    let tail = snippet(result, 160);
    match end {
        TurnEnd::Completed => (SessionState::Done, format!("turn {turn} complete: {tail}")),
        TurnEnd::Interrupted => (
            SessionState::Done,
            format!("turn {turn} interrupted: {tail}"),
        ),
        TurnEnd::Failed => (SessionState::Error, format!("turn {turn} failed: {tail}")),
        TurnEnd::TimedOut => (
            SessionState::Error,
            format!("turn {turn} timed out: {tail}"),
        ),
    }
}

/// Run one harness turn in `workspace`: the launch turn when `resume` is
/// `None`, a steer turn otherwise; publishing progress to the live feed.
/// Callers run this inside a spawned task.
///
/// Prefers **ACP** (`docs/adr/0024`) when an adapter is available, falling
/// back to the `claude -p` CLI when ACP is disabled/unavailable or its setup
/// fails.
async fn run_turn(
    workspace: &Path,
    prompt: &str,
    resume: Option<&str>,
    live: &LiveSessions,
    session: EntryId,
    agent: &crate::agent::Agent,
    control: &mut mpsc::Receiver<TurnControl>,
) -> TurnOutcome {
    let harness = harness_by_id(&agent.harness);
    if let Some(adapter) = acp_adapter_command(harness) {
        let acp_agent = acp_config(agent, harness);
        match crate::acp::run_turn_acp(
            &adapter, workspace, prompt, resume, live, session, &acp_agent, control,
        )
        .await
        {
            Ok(outcome) => return outcome,
            Err(err) => {
                tracing::warn!("ACP turn setup failed for {} ({err:#})", harness.label);
                if harness.has_cli_fallback() {
                    live.publish(
                        session,
                        LiveEvent::new("status", "ACP unavailable — falling back to claude -p"),
                    );
                } else {
                    return TurnOutcome {
                        result: format!("{} could not start over ACP: {err:#}", harness.label),
                        harness_session: None,
                        end: TurnEnd::Failed,
                    };
                }
            }
        }
    }
    if harness.has_cli_fallback() {
        run_turn_cli(workspace, prompt, resume, live, session, control).await
    } else {
        TurnOutcome {
            result: format!(
                "{} needs ACP, but no adapter is available (is Node installed?)",
                harness.label
            ),
            harness_session: None,
            end: TurnEnd::Failed,
        }
    }
}

/// Build the per-turn ACP config from an Agent. MCP servers cross to any
/// harness (standard ACP); the role, model, skills, and plugins ride the Claude
/// adapter's `_meta` extensions (the SDK options the adapter spreads), so they
/// are only carried for Claude agents — other harnesses would ignore them,
/// and `docs/.../agent-personas-design.md` defers OpenCode's own surface.
fn acp_config(agent: &crate::agent::Agent, harness: Harness) -> crate::acp::AcpAgent {
    let claude = harness.id == "claude";
    let claude_only = |items: &[String]| if claude { items.to_vec() } else { Vec::new() };
    crate::acp::AcpAgent {
        mcp_servers: agent.mcp_servers.clone(),
        system_prompt: if claude { agent.role.clone() } else { None },
        model: if claude { agent.model.clone() } else { None },
        skills: claude_only(&agent.skills),
        plugins: claude_only(&agent.plugins),
    }
}

/// The ACP adapter command for `harness`, or `None` to use the `claude -p`
/// CLI. ACP is preferred; fall back when a test stub is set, when forced to
/// `cli`, or when Node (which the adapters need) is absent.
fn acp_adapter_command(harness: Harness) -> Option<Vec<String>> {
    if std::env::var_os("JUNTO_HARNESS_CMD").is_some() {
        return None; // tests drive the stub over the CLI path
    }
    if std::env::var("JUNTO_HARNESS_PROTOCOL").ok().as_deref() == Some("cli") {
        return None;
    }
    if !node_available() {
        return None;
    }
    harness.acp_command()
}

/// Is Node on PATH? The ACP adapter is a Node package; probed once and cached.
fn node_available() -> bool {
    static NODE: OnceLock<bool> = OnceLock::new();
    *NODE.get_or_init(|| {
        let mut command = std::process::Command::new("node");
        command
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        no_console_window(&mut command);
        command.status().map(|s| s.success()).unwrap_or(false)
    })
}

/// Run one harness turn over the `claude -p` stream-json CLI — the fallback
/// path. Streams `stream-json` line by line, publishing progress to the live
/// feed as it arrives; returns the final outcome.
///
/// The prompt travels over **stdin**, never argv: prompts are multi-line,
/// and Windows refuses newline-bearing arguments to `.cmd` shims (which is
/// what an npm-installed `claude` is).
async fn run_turn_cli(
    workspace: &Path,
    prompt: &str,
    resume: Option<&str>,
    live: &LiveSessions,
    session: EntryId,
    control: &mut mpsc::Receiver<TurnControl>,
) -> TurnOutcome {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    // The backend decides native-vs-WSL and the working directory; we add the
    // shared claude arguments on top.
    let mut command = harness_command(workspace);
    if let Some(harness_session) = resume {
        command.arg("--resume").arg(harness_session);
    }
    command
        .arg("-p")
        .arg("--output-format")
        .arg("stream-json")
        // stream-json under --print requires --verbose; it only affects
        // stderr logging, so stdout stays clean JSONL.
        .arg("--verbose")
        // docs/adr/0023: a headless turn stalled on an invisible permission
        // prompt is worthless; junto's gates are the outcome layer.
        .arg("--dangerously-skip-permissions")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // CLAUDE.md (terminal-less): never flash a console window for the harness;
    // its output is captured as a memo/diff Artifact, not shown as scrollback.
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let mut spawned = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return TurnOutcome {
                result: format!("failed to spawn the harness: {err}"),
                harness_session: None,
                end: TurnEnd::Failed,
            };
        }
    };
    if let Some(mut stdin) = spawned.stdin.take() {
        // A stub that never reads stdin is fine — the pipe buffer holds a
        // prompt-sized write; errors here just mean the child exited early.
        let _ = stdin.write_all(prompt.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }
    let Some(stdout) = spawned.stdout.take() else {
        return TurnOutcome {
            result: "harness produced no stdout pipe".into(),
            harness_session: None,
            end: TurnEnd::Failed,
        };
    };
    // Drain stderr concurrently so a chatty harness can't fill the pipe and
    // block; it's the fallback message when no result line arrives.
    let stderr_task = spawned.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt as _;
            let mut buf = String::new();
            let _ = stderr.read_to_string(&mut buf).await;
            buf
        })
    });

    let mut harness_session: Option<String> = None;
    let mut result_text: Option<String> = None;
    let mut is_error = false;

    // Read stdout to EOF, publishing each interpreted line, then reap the
    // child for its exit status. The whole drive is under the turn timeout;
    // on timeout the future drops and kill_on_drop reaps the child.
    let drive = async {
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let effects = interpret_stream_line(&line);
            for event in effects.events {
                live.publish(session, event);
            }
            if let Some(found) = effects.session {
                harness_session = Some(found);
            }
            if effects.saw_result {
                result_text = effects.result;
                is_error = effects.is_error;
            }
        }
        spawned.wait().await
    };

    let status = tokio::select! {
        // A human interrupt ends the CLI turn. Steer-in-place is ACP-only; a CLI
        // re-prompt happens via the resume path (`launch::steer`). kill_on_drop
        // reaps the child as the drive future drops.
        _ = control.recv() => {
            return TurnOutcome {
                result: "turn interrupted by the human".into(),
                harness_session: None,
                end: TurnEnd::Interrupted,
            };
        }
        timed = tokio::time::timeout(TURN_TIMEOUT, drive) => match timed {
            Ok(status) => status,
            Err(_) => {
                return TurnOutcome {
                    result: format!(
                        "turn exceeded the {}-minute timeout and was killed (docs/adr/0023)",
                        TURN_TIMEOUT.as_secs() / 60
                    ),
                    harness_session,
                    end: TurnEnd::TimedOut,
                };
            }
        },
    };

    let exit_ok = matches!(status, Ok(s) if s.success());
    let stderr = match stderr_task {
        Some(handle) => handle.await.unwrap_or_default(),
        None => String::new(),
    };
    let result = match result_text {
        Some(text) if !text.trim().is_empty() => text,
        _ if !stderr.trim().is_empty() => stderr.trim().to_string(),
        _ => "(the harness produced no result)".to_string(),
    };
    TurnOutcome {
        result,
        harness_session,
        end: if is_error || !exit_ok {
            TurnEnd::Failed
        } else {
            TurnEnd::Completed
        },
    }
}

/// Write `content` into the machine-local artifact store and return its
/// provenance ref (`file://` URI + sha256) — the content itself never enters
/// the ledger (`docs/adr/0020`).
fn store_artifact(
    junto_home: &Path,
    session: &EntryId,
    name: &str,
    content: &str,
) -> Result<ProvenanceRef> {
    let dir = junto_home.join("artifacts").join(session.to_string());
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(name);
    std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    let digest = format!("sha256:{:x}", sha2::Sha256::digest(content.as_bytes()));
    let uri = Uri::new(format!(
        "file:///{}",
        path.display().to_string().replace('\\', "/")
    ))
    .context("artifact uri")?;
    let digest = ContentDigest::new(digest).context("artifact digest")?;
    Ok(ProvenanceRef::with_digest(uri, digest))
}

/// Suppress the console window Windows flashes when a GUI-hosted process
/// spawns a console child. CLAUDE.md (terminal-less): agent and tool output
/// is captured as Artifacts, never rendered as scrollback — and never as a
/// flashed window. A no-op off Windows.
pub(crate) fn no_console_window(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// The workspace's uncommitted changes (`git diff HEAD` + untracked names),
/// or `None` when clean.
fn workspace_diff(workspace: &Path) -> Option<String> {
    let run = |args: &[&str]| -> Option<String> {
        let mut command = std::process::Command::new("git");
        command.arg("-C").arg(workspace).args(args);
        no_console_window(&mut command);
        let out = command.output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    };
    // On a junto PR branch the worker commits its work (ADR-pending push-gate),
    // so `git diff HEAD` would be empty — diff against the recorded base commit
    // instead, which shows everything since base whether committed or not.
    let base = pr_branch_base(workspace);
    let status = run(&["status", "--porcelain"])?;
    if status.trim().is_empty() && base.is_none() {
        return None;
    }
    let diff = match &base {
        Some(base_sha) => run(&["diff", base_sha]).unwrap_or_default(),
        None => run(&["diff", "HEAD"]).unwrap_or_default(),
    };
    if diff.trim().is_empty() && status.trim().is_empty() {
        return None;
    }
    let untracked: Vec<&str> = status
        .lines()
        .filter(|line| line.starts_with("??"))
        .collect();
    let mut out = diff;
    if !untracked.is_empty() {
        out.push_str("\n# untracked files:\n");
        for line in untracked {
            out.push_str(line);
            out.push('\n');
        }
    }
    Some(out)
}

/// The PR branch junto prepared for an Outcome session: a fresh `junto/<session>`
/// off the current HEAD, with `base` (the branch it forked from) recorded so a
/// later slice opens the PR `base ← branch`. The base **commit** is stored in
/// git config (`branch.<branch>.juntoBaseSha`) so [`workspace_diff`] can show
/// the committed work base-relative without any caller threading it through.
#[derive(Debug, Clone)]
pub(crate) struct BranchPlan {
    pub branch: String,
    pub base: String,
}

/// Create `junto/<session>` off the workspace's current HEAD and switch to it,
/// recording the base for later. The worker commits onto this branch; a later
/// slice pushes it and opens the PR. Best-effort: callers log and continue if
/// the workspace isn't a usable git repo.
pub(crate) fn prepare_pr_branch(workspace: &Path, session: EntryId) -> Result<BranchPlan> {
    let git = |args: &[&str]| -> Result<std::process::Output> {
        let mut command = std::process::Command::new("git");
        command.arg("-C").arg(workspace).args(args);
        no_console_window(&mut command);
        command.output().context("running git")
    };
    let trimmed =
        |out: &std::process::Output| String::from_utf8_lossy(&out.stdout).trim().to_string();

    let head = git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    let base = trimmed(&head);
    let base = if base.is_empty() || base == "HEAD" {
        "main".to_string()
    } else {
        base
    };
    let base_sha = trimmed(&git(&["rev-parse", "HEAD"])?);
    let branch = format!("junto/{session}");

    let created = git(&["checkout", "-b", &branch])?;
    if !created.status.success() {
        bail!(
            "creating PR branch {branch}: {}",
            String::from_utf8_lossy(&created.stderr).trim()
        );
    }
    if !base_sha.is_empty() {
        // The base commit drives workspace_diff; the base ref is the PR base
        // (docs/adr/0029). Both recorded in config; non-fatal if they don't take.
        let _ = git(&[
            "config",
            &format!("branch.{branch}.juntoBaseSha"),
            &base_sha,
        ]);
        let _ = git(&["config", &format!("branch.{branch}.juntoBaseRef"), &base]);
    }
    Ok(BranchPlan { branch, base })
}

/// The base **branch** a junto PR branch targets (the PR base), from git config
/// — `None` when not on a junto branch. Distinct from [`pr_branch_base`], which
/// returns the base *commit* for diffing.
fn pr_branch_base_ref(workspace: &Path) -> Option<String> {
    let git = |args: &[&str]| -> Option<String> {
        let mut command = std::process::Command::new("git");
        command.arg("-C").arg(workspace).args(args);
        no_console_window(&mut command);
        let out = command.output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    let base = git(&["config", &format!("branch.{branch}.juntoBaseRef")])?;
    (!base.is_empty()).then_some(base)
}

/// The workspace's current branch name (`None` if git can't say).
fn current_branch(workspace: &Path) -> Option<String> {
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "--abbrev-ref", "HEAD"]);
    no_console_window(&mut command);
    let out = command.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD")
}

/// Push `branch` to `origin` (setting upstream) — inherits the user's git auth,
/// like the substrate's sync (constraint #1). Errors carry git's stderr.
fn push_branch(workspace: &Path, branch: &str) -> Result<()> {
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(workspace)
        .args(["push", "-u", "origin", branch]);
    no_console_window(&mut command);
    let out = command.output().context("running git push")?;
    if !out.status.success() {
        bail!(
            "git push of {branch} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// The base commit a junto PR branch was forked from, from git config — `None`
/// when the workspace isn't on a junto branch (the ordinary working-tree diff).
fn pr_branch_base(workspace: &Path) -> Option<String> {
    let git = |args: &[&str]| -> Option<String> {
        let mut command = std::process::Command::new("git");
        command.arg("-C").arg(workspace).args(args);
        no_console_window(&mut command);
        let out = command.output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    let sha = git(&["config", &format!("branch.{branch}.juntoBaseSha")])?;
    (!sha.is_empty()).then_some(sha)
}

/// First ~N chars of a result for artifact/note descriptions.
fn snippet(text: &str, limit: usize) -> String {
    let mut s: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        s.push('…');
    }
    s
}

/// Launch a new Agent Session: append `SessionStarted` (authored as the
/// harness member), then run the first turn in the background. Returns the
/// new session's entry id immediately — the page shows the live state.
pub async fn launch(
    host: std::sync::Arc<Host>,
    channel: ChannelId,
    channel_ref: String,
    workspace: PathBuf,
    intent: String,
    agent: crate::agent::Agent,
) -> Result<EntryId> {
    let session = EntryId::new();
    append(
        &host,
        &channel_ref,
        LedgerEntry {
            id: session,
            channel,
            author: agent.member(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::SessionStarted {
                intent: intent.clone(),
            },
        },
    )
    .await?;

    let prompt = format!(
        "{intent}\n\n(Launched from junto channel '{channel_ref}'; junto session {session}. \
         Do the work in this repository.)"
    );
    spawn_turn(
        host,
        channel,
        channel_ref,
        workspace,
        session,
        prompt,
        None,
        agent,
    );
    Ok(session)
}

/// Steer an existing session: record the human's instruction as a
/// `SessionUpdated` note (the record keeps the steering — docs/adr/0023),
/// flip the session back to working, and run a `--resume` turn.
pub async fn steer(
    host: std::sync::Arc<Host>,
    channel: ChannelId,
    channel_ref: String,
    workspace: PathBuf,
    session: EntryId,
    steered_by: Member,
    message: String,
) -> Result<()> {
    let junto_home = crate::host::junto_home()?;
    let Some(harness_session) = harness_session_for(&junto_home, &session)? else {
        bail!(
            "no harness session is recorded for {session} on this machine — it was launched \
             elsewhere or before the mapping existed; start a new session instead"
        );
    };
    // Steer the same agent (identity + config) that ran the session.
    let agent = resume_agent(&junto_home, &session)?;
    record_steer_note(&host, &channel_ref, channel, session, steered_by, &message).await?;
    spawn_turn(
        host,
        channel,
        channel_ref,
        workspace,
        session,
        message,
        Some(harness_session),
        agent,
    );
    Ok(())
}

/// Record a human's steer instruction as a `SessionUpdated` note (the record
/// keeps who steered and what — docs/adr/0023), flipping the session to Working.
/// Shared by the between-turns resume path ([`steer`]) and the in-session path
/// ([`steer_live`]).
async fn record_steer_note(
    host: &Host,
    channel_ref: &str,
    channel: ChannelId,
    session: EntryId,
    steered_by: Member,
    message: &str,
) -> Result<()> {
    append(
        host,
        channel_ref,
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author: steered_by,
            timestamp: Timestamp::now(),
            payload: EntryPayload::SessionUpdated {
                target: session,
                state: SessionState::Working,
                note: format!("steer: {message}"),
            },
        },
    )
    .await
}

/// Steer a *running* turn in place (`docs/adr/0032`): deliver the message to the
/// live turn over the control channel, then record the steer note. Returns
/// `Err(NotLive)` when no turn is currently running for the session — the caller
/// falls back to the between-turns resume path ([`steer`]).
pub(crate) async fn steer_live(
    host: std::sync::Arc<Host>,
    channel: ChannelId,
    channel_ref: String,
    session: EntryId,
    steered_by: Member,
    message: String,
) -> Result<(), NotLive> {
    // Deliver first: if no turn is live we record nothing and the caller resumes.
    host.live()
        .control(session, TurnControl::Steer(message.clone()))?;
    // The steer has landed in the running turn; recording the note is
    // best-effort — a note failure must not undo the delivered steer.
    if let Err(err) =
        record_steer_note(&host, &channel_ref, channel, session, steered_by, &message).await
    {
        tracing::warn!("recording steer note for session {session} failed: {err:#}");
    }
    Ok(())
}

/// Rebuild the agent that should run a resumed turn: the recorded agent by
/// slug, falling back to the stock agent for the recorded harness (its slug
/// equals the harness id), and finally the default harness. Seed-on-read means
/// a stock slug always resolves; only a deleted custom agent falls through.
fn resume_agent(junto_home: &Path, session: &EntryId) -> Result<crate::agent::Agent> {
    let slug = agent_slug_for(junto_home, session)?
        .or(harness_id_for(junto_home, session)?)
        .unwrap_or_else(|| HARNESSES[0].id.to_string());
    if let Some(agent) = crate::agent::agent_by_slug(junto_home, &slug)? {
        return Ok(agent);
    }
    crate::agent::agent_by_slug(junto_home, HARNESSES[0].id)?
        .context("no default agent available to resume the session")
}

/// Run one turn in the background and record its outcome: artifacts
/// (result memo + workspace diff) and the final state, authored as the
/// harness member.
#[allow(clippy::too_many_arguments)]
fn spawn_turn(
    host: std::sync::Arc<Host>,
    channel: ChannelId,
    channel_ref: String,
    workspace: PathBuf,
    session: EntryId,
    prompt: String,
    resume: Option<String>,
    agent: crate::agent::Agent,
) {
    // Open the live feed *before* spawning, so it exists the moment this
    // function returns — a client that subscribes right after the launch/steer
    // HTTP call lands on the running turn instead of an immediate "end".
    let mut control_rx = host.live().begin(session);
    tokio::spawn(async move {
        let outcome = run_turn(
            &workspace,
            &prompt,
            resume.as_deref(),
            host.live(),
            session,
            &agent,
            &mut control_rx,
        )
        .await;
        if let Err(err) = record_outcome(
            &host,
            &channel_ref,
            channel,
            session,
            &workspace,
            &outcome,
            &agent,
        )
        .await
        {
            tracing::warn!("recording session {session} outcome failed: {err:#}");
        }
        // Close the live feed only after the outcome is recorded, so a watcher
        // reloading on stream-end sees the landed memo + diff, not "working".
        host.live().finish(session);
        // Best-effort sync so the session's record leaves this machine.
        if let Ok(resolution) = host.resolve(&channel_ref).await
            && let crate::host::Resolution::Resolved { ledger, id, .. } = resolution
        {
            let _ = ledger
                .lock()
                .await
                .substrate_mut()
                .sync("origin", &id)
                .await;
        }
    });
}

async fn record_outcome(
    host: &Host,
    channel_ref: &str,
    channel: ChannelId,
    session: EntryId,
    workspace: &Path,
    outcome: &TurnOutcome,
    agent: &crate::agent::Agent,
) -> Result<()> {
    let junto_home = crate::host::junto_home()?;
    let harness = harness_by_id(&agent.harness);
    let turn = record_turn(
        &junto_home,
        &session,
        outcome.harness_session.clone(),
        harness.id,
        &agent.slug,
    )?;

    // The result memo artifact.
    let memo = store_artifact(
        &junto_home,
        &session,
        &format!("turn-{turn}-result.md"),
        &outcome.result,
    )?;
    append(
        host,
        channel_ref,
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author: agent.member(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::ArtifactAttached {
                target: session,
                kind: "memo".into(),
                description: snippet(&outcome.result, 240),
                provenance: vec![memo],
            },
        },
    )
    .await?;

    // The workspace diff artifact, when the turn changed anything.
    if let Some(diff) = workspace_diff(workspace) {
        let diff_ref = store_artifact(
            &junto_home,
            &session,
            &format!("turn-{turn}-diff.patch"),
            &diff,
        )?;
        append(
            host,
            channel_ref,
            LedgerEntry {
                id: EntryId::new(),
                channel,
                author: agent.member(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::ArtifactAttached {
                    target: session,
                    kind: "diff".into(),
                    description: format!(
                        "uncommitted changes in {} after turn {turn}",
                        workspace.display()
                    ),
                    provenance: vec![diff_ref],
                },
            },
        )
        .await?;
    }

    let (state, note) = outcome_state(outcome.end, turn, &outcome.result);
    append(
        host,
        channel_ref,
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author: agent.member(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::SessionUpdated {
                target: session,
                state,
                note,
            },
        },
    )
    .await
}

// ---- the Outcome loop: the code-PR push-gate (docs/adr/0025) ----

/// The iteration budget for an Outcome loop before it escalates to a Gate.
const MAX_OUTCOME_ITERATIONS: u32 = 3;

/// The `Proposal.kind` tag marking the code-PR push-gate's "open the PR" gate
/// (`docs/adr/0029`). The app-level executor matches this stable key on
/// approval — never the human-readable action text.
pub(crate) const PR_OPEN_GATE_KIND: &str = "code-pr.open-pr";

/// Launch an **Outcome-driven** Agent Session — the code-PR push-gate. The
/// worker does the work; junto verifies it (mechanical checks + the Grader);
/// findings feed back until the Outcome is satisfied or the iteration budget
/// runs out, at which point it escalates to a human Gate. Returns the new
/// session id immediately; the loop runs in the background.
pub async fn launch_outcome(
    host: std::sync::Arc<Host>,
    channel: ChannelId,
    channel_ref: String,
    workspace: PathBuf,
    intent: String,
    agent: crate::agent::Agent,
) -> Result<EntryId> {
    let session = EntryId::new();
    append(
        &host,
        &channel_ref,
        LedgerEntry {
            id: session,
            channel,
            author: agent.member(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::SessionStarted {
                intent: intent.clone(),
            },
        },
    )
    .await?;
    spawn_outcome_loop(
        host,
        channel,
        channel_ref,
        workspace,
        session,
        intent,
        agent,
    );
    Ok(session)
}

/// Drive the Outcome loop in the background: worker turn → verify → feed back →
/// revise, until satisfied or `MAX_OUTCOME_ITERATIONS`, then record the terminal.
fn spawn_outcome_loop(
    host: std::sync::Arc<Host>,
    channel: ChannelId,
    channel_ref: String,
    workspace: PathBuf,
    session: EntryId,
    intent: String,
    agent: crate::agent::Agent,
) {
    tokio::spawn(async move {
        let _control_rx = host.live().begin(session);

        // Prepare a PR branch the worker commits onto (the push-gate's
        // deliverable). Best-effort: without it, grading falls back to the
        // working-tree diff. workspace_diff self-discovers the recorded base.
        let branch_plan = match prepare_pr_branch(&workspace, session) {
            Ok(plan) => {
                tracing::info!(
                    "outcome session {session}: committing onto {} (off {})",
                    plan.branch,
                    plan.base
                );
                Some(plan)
            }
            Err(err) => {
                tracing::warn!(
                    "outcome session {session}: no PR branch ({err:#}); grading the \
                     working-tree diff"
                );
                None
            }
        };

        // Each step owns its own clones so the spawned futures stay `Send`.
        let w_host = host.clone();
        let w_ref = channel_ref.clone();
        let w_ws = workspace.clone();
        let w_agent = agent.clone();
        let mut harness_session: Option<String> = None;
        let worker = async move |feedback: Option<String>| {
            run_worker_turn(
                &w_host,
                &w_ref,
                channel,
                session,
                &w_ws,
                &w_agent,
                &intent,
                feedback.as_deref(),
                &mut harness_session,
            )
            .await;
        };
        let v_host = host.clone();
        let v_ref = channel_ref.clone();
        let v_ws = workspace.clone();
        let v_agent = agent.clone();
        let verify =
            async move || verify_one(&v_host, &v_ref, channel, session, &v_ws, &v_agent).await;

        let terminal = crate::outcome::drive_loop(MAX_OUTCOME_ITERATIONS, worker, verify).await;

        if let Err(err) = finish_outcome(
            &host,
            &channel_ref,
            channel,
            session,
            &workspace,
            &agent,
            &terminal,
            branch_plan.as_ref(),
        )
        .await
        {
            tracing::warn!("recording outcome terminal for session {session} failed: {err:#}");
        }
        host.live().finish(session);
        if let Ok(crate::host::Resolution::Resolved { ledger, id, .. }) =
            host.resolve(&channel_ref).await
        {
            let _ = ledger
                .lock()
                .await
                .substrate_mut()
                .sync("origin", &id)
                .await;
        }
    });
}

/// Run one worker turn (resuming the worker's session after the first), then
/// capture its memo + diff artifacts with the session left `Working`.
#[allow(clippy::too_many_arguments)]
async fn run_worker_turn(
    host: &Host,
    channel_ref: &str,
    channel: ChannelId,
    session: EntryId,
    workspace: &Path,
    agent: &crate::agent::Agent,
    intent: &str,
    feedback: Option<&str>,
    harness_session: &mut Option<String>,
) {
    let prompt = match feedback {
        None => format!(
            "{intent}\n\n(Launched from junto channel '{channel_ref}'; junto session \
             {session}. Do the work in this repository. When the change is complete, \
             commit it to the current git branch with a clear message — junto pushes that \
             branch and opens the pull request.)"
        ),
        Some(findings) => format!(
            "Verification found problems with your last change. Fix them, commit the fix to \
             the current git branch, then stop.\n\n{findings}"
        ),
    };
    // The autonomous worker turn is not interruptible; an inert control channel
    // (sender kept alive, so it never fires) satisfies run_turn's signature.
    let (_inert_control, mut control) = mpsc::channel(1);
    let outcome = run_turn(
        workspace,
        &prompt,
        harness_session.as_deref(),
        host.live(),
        session,
        agent,
        &mut control,
    )
    .await;
    if outcome.harness_session.is_some() {
        *harness_session = outcome.harness_session.clone();
    }
    if let Err(err) = capture_turn(
        host,
        channel_ref,
        channel,
        session,
        workspace,
        &outcome,
        agent,
        SessionState::Working,
    )
    .await
    {
        tracing::warn!("capturing worker turn for session {session} failed: {err:#}");
    }
}

/// Verify one iteration: run the mechanical checks first; if all pass, run the
/// Grader in a fresh (clean-context) session over the workspace diff. The
/// Grader's reply is captured as a `grader-report` Artifact (docs/adr/0025).
async fn verify_one(
    host: &Host,
    channel_ref: &str,
    channel: ChannelId,
    session: EntryId,
    workspace: &Path,
    agent: &crate::agent::Agent,
) -> crate::outcome::VerifyOutcome {
    let checks_ws = workspace.to_path_buf();
    let results = tokio::task::spawn_blocking(move || {
        crate::verify::run_checks(&checks_ws, &crate::verify::default_cargo_checks())
    })
    .await
    .unwrap_or_default();

    if let Some(feedback) = crate::verify::mechanical_feedback(&results) {
        host.live().publish(
            session,
            LiveEvent::new("status", "mechanical checks failed — sending back to fix"),
        );
        return crate::outcome::VerifyOutcome {
            satisfied: false,
            feedback,
        };
    }

    // Mechanical green → the Grader judges the diff in a fresh session.
    let diff = workspace_diff(workspace).unwrap_or_default();
    let prompt = crate::grader::grader_prompt(crate::grader::default_code_pr_rubric(), &diff);
    host.live().publish(
        session,
        LiveEvent::new("status", "checks green — grading the diff"),
    );
    // The grader turn is autonomous, not interruptible — an inert control channel.
    let (_inert_control, mut control) = mpsc::channel(1);
    let graded = run_turn(
        workspace,
        &prompt,
        None,
        host.live(),
        session,
        agent,
        &mut control,
    )
    .await;

    if let Err(err) =
        store_grader_report(host, channel_ref, channel, session, &graded.result, agent).await
    {
        tracing::warn!("storing grader report for session {session} failed: {err:#}");
    }
    let verdict = crate::grader::parse_verdict(&graded.result);
    crate::outcome::VerifyOutcome {
        satisfied: verdict.satisfied,
        feedback: verdict.feedback,
    }
}

/// Capture a turn's result memo + workspace diff as Artifacts and fold the
/// session to `state`. The shared turn-recording path for the Outcome loop.
#[allow(clippy::too_many_arguments)]
async fn capture_turn(
    host: &Host,
    channel_ref: &str,
    channel: ChannelId,
    session: EntryId,
    workspace: &Path,
    outcome: &TurnOutcome,
    agent: &crate::agent::Agent,
    state: SessionState,
) -> Result<()> {
    let junto_home = crate::host::junto_home()?;
    let harness = harness_by_id(&agent.harness);
    let turn = record_turn(
        &junto_home,
        &session,
        outcome.harness_session.clone(),
        harness.id,
        &agent.slug,
    )?;
    let memo = store_artifact(
        &junto_home,
        &session,
        &format!("turn-{turn}-result.md"),
        &outcome.result,
    )?;
    append(
        host,
        channel_ref,
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author: agent.member(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::ArtifactAttached {
                target: session,
                kind: "memo".into(),
                description: snippet(&outcome.result, 240),
                provenance: vec![memo],
            },
        },
    )
    .await?;
    if let Some(diff) = workspace_diff(workspace) {
        let diff_ref = store_artifact(
            &junto_home,
            &session,
            &format!("turn-{turn}-diff.patch"),
            &diff,
        )?;
        append(
            host,
            channel_ref,
            LedgerEntry {
                id: EntryId::new(),
                channel,
                author: agent.member(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::ArtifactAttached {
                    target: session,
                    kind: "diff".into(),
                    description: format!("uncommitted changes after turn {turn}"),
                    provenance: vec![diff_ref],
                },
            },
        )
        .await?;
    }
    append(
        host,
        channel_ref,
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author: agent.member(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::SessionUpdated {
                target: session,
                state,
                note: format!("turn {turn}: {}", snippet(&outcome.result, 160)),
            },
        },
    )
    .await
}

/// Store the Grader's reply as a `grader-report` Artifact on the worker session.
async fn store_grader_report(
    host: &Host,
    channel_ref: &str,
    channel: ChannelId,
    session: EntryId,
    report: &str,
    agent: &crate::agent::Agent,
) -> Result<()> {
    let junto_home = crate::host::junto_home()?;
    let stored = store_artifact(
        &junto_home,
        &session,
        &format!("grade-{}.md", EntryId::new()),
        report,
    )?;
    append(
        host,
        channel_ref,
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author: agent.member(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::ArtifactAttached {
                target: session,
                kind: "grader-report".into(),
                description: snippet(report, 240),
                provenance: vec![stored],
            },
        },
    )
    .await
}

/// Record the loop's terminal. **Satisfied** → an "Open the PR" Gate (a
/// `Proposal`, session left `AwaitingApproval`) when a PR branch is in hand —
/// opening a PR is a consequential outward action junto gates (a later slice's
/// executor opens it on approval); without a branch it falls back to `Done`.
/// **MaxIterationsReached** → the escalation Gate (accept despite unmet
/// verification). Symmetric — both terminals produce a gate (docs/adr/0025).
#[allow(clippy::too_many_arguments)]
async fn finish_outcome(
    host: &Host,
    channel_ref: &str,
    channel: ChannelId,
    session: EntryId,
    workspace: &Path,
    agent: &crate::agent::Agent,
    terminal: &crate::outcome::LoopTerminal,
    branch_plan: Option<&BranchPlan>,
) -> Result<()> {
    // The ACE-style outcome signal (gated) for the future self-improvement
    // Playbook to learn from — recorded for every terminal.
    if let Err(err) =
        store_outcome_signal(host, channel_ref, channel, session, terminal, agent).await
    {
        tracing::warn!("storing outcome signal for session {session} failed: {err:#}");
    }
    match terminal {
        crate::outcome::LoopTerminal::Satisfied { iterations } => {
            // Only gate a PR-open when a forge can honor it (docs/adr/0029):
            // with a branch and an available forge, gate; otherwise the work is
            // verified Done with nothing to open a PR from.
            let plan = match branch_plan {
                Some(plan) if crate::forge::GithubForge::is_available() => plan,
                other => {
                    let why = if other.is_some() {
                        "no forge available"
                    } else {
                        "no PR branch"
                    };
                    return append(
                        host,
                        channel_ref,
                        LedgerEntry {
                            id: EntryId::new(),
                            channel,
                            author: agent.member(),
                            timestamp: Timestamp::now(),
                            payload: EntryPayload::SessionUpdated {
                                target: session,
                                state: SessionState::Done,
                                note: format!(
                                    "verified green after {iterations} iteration(s) ({why})"
                                ),
                            },
                        },
                    )
                    .await;
                }
            };
            // Gate the PR-open: opening a PR is a consequential outward action.
            let junto_home = crate::host::junto_home()?;
            let mut provenance = Vec::new();
            if let Some(diff) = workspace_diff(workspace) {
                provenance.push(store_artifact(
                    &junto_home,
                    &session,
                    "deliverable-diff.patch",
                    &diff,
                )?);
            }
            append(
                host,
                channel_ref,
                LedgerEntry {
                    id: EntryId::new(),
                    channel,
                    author: agent.member(),
                    timestamp: Timestamp::now(),
                    payload: EntryPayload::Proposal {
                        action: format!(
                            "Open a pull request for this verified deliverable ({} → {})",
                            plan.branch, plan.base
                        ),
                        rationale: format!(
                            "The Outcome was verified green after {iterations} iteration(s). \
                             Approve to push '{}' and open the pull request against '{}'.",
                            plan.branch, plan.base
                        ),
                        provenance,
                        requirement: junto_kernel::ApprovalRequirement::Count(1),
                        frame: None,
                        // The tag the executor matches on approval (docs/adr/0029).
                        kind: Some(PR_OPEN_GATE_KIND.to_string()),
                    },
                },
            )
            .await?;
            append(
                host,
                channel_ref,
                LedgerEntry {
                    id: EntryId::new(),
                    channel,
                    author: agent.member(),
                    timestamp: Timestamp::now(),
                    payload: EntryPayload::SessionUpdated {
                        target: session,
                        state: SessionState::AwaitingApproval,
                        note: format!(
                            "verified green after {iterations} iteration(s); awaiting approval \
                             to open the pull request"
                        ),
                    },
                },
            )
            .await
        }
        crate::outcome::LoopTerminal::MaxIterationsReached {
            iterations,
            last_feedback,
        } => {
            let junto_home = crate::host::junto_home()?;
            let mut provenance = Vec::new();
            if let Some(diff) = workspace_diff(workspace) {
                provenance.push(store_artifact(
                    &junto_home,
                    &session,
                    "escalation-diff.patch",
                    &diff,
                )?);
            }
            append(
                host,
                channel_ref,
                LedgerEntry {
                    id: EntryId::new(),
                    channel,
                    author: agent.member(),
                    timestamp: Timestamp::now(),
                    payload: EntryPayload::Proposal {
                        action: "Accept this deliverable despite unmet verification".into(),
                        rationale: format!(
                            "The Outcome loop hit its {iterations}-iteration budget without \
                             passing verification. Latest findings:\n\n{last_feedback}"
                        ),
                        provenance,
                        requirement: junto_kernel::ApprovalRequirement::Count(1),
                        frame: None,
                        kind: None,
                    },
                },
            )
            .await?;
            append(
                host,
                channel_ref,
                LedgerEntry {
                    id: EntryId::new(),
                    channel,
                    author: agent.member(),
                    timestamp: Timestamp::now(),
                    payload: EntryPayload::SessionUpdated {
                        target: session,
                        state: SessionState::AwaitingApproval,
                        note: format!(
                            "escalated to a Gate after {iterations} iterations without passing \
                             verification"
                        ),
                    },
                },
            )
            .await
        }
    }
}

/// React to a just-recorded approval (`docs/adr/0029`): if it resolved a
/// code-PR **"open the PR"** gate, push the `junto/<session>` branch and open
/// the pull request, recording the PR URL as the deliverable. Best-effort and
/// idempotent — a no-op for any other approval. Called from the web/MCP approve
/// paths; fires only on the host that recorded the approval (no multi-host
/// double-open).
pub(crate) async fn execute_pr_gate_if_approved(
    host: &Host,
    channel: ChannelId,
    proposal: EntryId,
) {
    if let Err(err) = try_execute_pr_gate(host, channel, proposal).await {
        tracing::warn!("opening the PR for gate {proposal} failed: {err:#}");
    }
}

async fn try_execute_pr_gate(host: &Host, channel: ChannelId, proposal: EntryId) -> Result<()> {
    let crate::host::Resolution::Resolved { ledger, id, .. } =
        host.resolve(&channel.to_string()).await?
    else {
        return Ok(());
    };
    let view = ledger.lock().await.project(&id).await?;

    // Recognize a code-PR open-PR gate by its kind tag (docs/adr/0029), and only
    // act once it is actually approved.
    let is_open_pr_gate = view.entries.iter().any(|entry| {
        entry.id == proposal
            && matches!(
                &entry.payload,
                EntryPayload::Proposal { kind: Some(kind), .. } if kind == PR_OPEN_GATE_KIND
            )
    });
    if !is_open_pr_gate || view.gate_status(&proposal) != Some(junto_kernel::GateStatus::Approved) {
        return Ok(());
    }

    // Recover the workspace, branch, and session (the branch name carries it).
    let home = crate::host::junto_home()?;
    let Some(workspace) = workspace_for(&home, &channel)? else {
        bail!("no workspace is mapped for channel {channel}");
    };
    let Some(branch) = current_branch(&workspace) else {
        return Ok(());
    };
    let Some(session_str) = branch.strip_prefix("junto/") else {
        return Ok(());
    };
    let session: EntryId = session_str
        .parse()
        .with_context(|| format!("branch {branch} has no session id"))?;

    // Idempotent: a finished session has already opened (or settled) its PR.
    if view
        .session(&session)
        .is_some_and(|s| s.state == SessionState::Done)
    {
        return Ok(());
    }

    let base = pr_branch_base_ref(&workspace).unwrap_or_else(|| "main".to_string());
    let author = view
        .entries
        .iter()
        .find(|entry| entry.id == session)
        .map(|entry| entry.author.clone())
        .unwrap_or_else(|| Member::agent("junto", "junto@local"));
    let intent = view
        .entries
        .iter()
        .find_map(|entry| match &entry.payload {
            EntryPayload::SessionStarted { intent } if entry.id == session => Some(intent.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "junto deliverable".to_string());

    // Push the branch, then open the PR.
    let spec = crate::forge::PullRequestSpec {
        repo: workspace.clone(),
        head: branch.clone(),
        base,
        title: snippet(&intent, 72),
        body: format!(
            "Opened by junto's code-PR push-gate (session {session}). Verified green.\n\n{intent}"
        ),
    };
    let channel_ref = channel.to_string();
    // Push then open — as one fallible step, so *any* failure (push or PR
    // create) records GateExecuted(false), not just the PR-create step
    // (docs/adr/0030; the push-only gap the first signal dogfood found).
    let opened = push_branch(&workspace, &branch)
        .and_then(|()| crate::forge::GithubForge.open_pull_request(&spec));
    let url = match opened {
        Ok(url) => url,
        Err(err) => {
            // Surface the failure; leave the gate approved so a re-approve retries.
            append(
                host,
                &channel_ref,
                LedgerEntry {
                    id: EntryId::new(),
                    channel,
                    author: author.clone(),
                    timestamp: Timestamp::now(),
                    payload: EntryPayload::SessionUpdated {
                        target: session,
                        state: SessionState::AwaitingApproval,
                        note: format!("opening the PR failed: {err:#}"),
                    },
                },
            )
            .await?;
            // Record the failure against the gate (docs/adr/0030) so the gate
            // surfaces as failed-execution rather than silently approved.
            append(
                host,
                &channel_ref,
                LedgerEntry {
                    id: EntryId::new(),
                    channel,
                    author,
                    timestamp: Timestamp::now(),
                    payload: EntryPayload::GateExecuted {
                        target: proposal,
                        success: false,
                        note: format!("{err:#}"),
                    },
                },
            )
            .await?;
            return Err(err);
        }
    };

    // Record the PR as the deliverable, and finish the session.
    let provenance = Uri::new(&url)
        .map(|uri| vec![ProvenanceRef::new(uri)])
        .unwrap_or_default();
    append(
        host,
        &channel_ref,
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author: author.clone(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::ArtifactAttached {
                target: session,
                kind: "pull-request".into(),
                description: format!("pull request {url}"),
                provenance,
            },
        },
    )
    .await?;
    append(
        host,
        &channel_ref,
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author: author.clone(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::SessionUpdated {
                target: session,
                state: SessionState::Done,
                note: format!("opened pull request {url}"),
            },
        },
    )
    .await?;
    // Record success against the gate (docs/adr/0030): the gate's action ran.
    append(
        host,
        &channel_ref,
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author,
            timestamp: Timestamp::now(),
            payload: EntryPayload::GateExecuted {
                target: proposal,
                success: true,
                note: url,
            },
        },
    )
    .await?;
    Ok(())
}

/// Store the loop's structured outcome signal (success/partial/failure-shaped,
/// docs/adr/0025 / the ACE comparison) as an `outcome-signal` Artifact — the
/// gated `record_outcome` the self-improvement Playbook will learn from.
async fn store_outcome_signal(
    host: &Host,
    channel_ref: &str,
    channel: ChannelId,
    session: EntryId,
    terminal: &crate::outcome::LoopTerminal,
    agent: &crate::agent::Agent,
) -> Result<()> {
    let junto_home = crate::host::junto_home()?;
    let (iterations, feedback) = match terminal {
        crate::outcome::LoopTerminal::Satisfied { iterations } => (*iterations, String::new()),
        crate::outcome::LoopTerminal::MaxIterationsReached {
            iterations,
            last_feedback,
        } => (*iterations, last_feedback.clone()),
    };
    let result = terminal.result();
    let body = serde_json::json!({
        "playbook": "code-pr",
        "result": result.as_str(),
        "iterations": iterations,
        "max_iterations": MAX_OUTCOME_ITERATIONS,
        "feedback": feedback,
    })
    .to_string();
    let stored = store_artifact(&junto_home, &session, "outcome-signal.json", &body)?;
    append(
        host,
        channel_ref,
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author: agent.member(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::ArtifactAttached {
                target: session,
                kind: "outcome-signal".into(),
                description: format!(
                    "outcome: {} after {iterations} iteration(s)",
                    result.as_str()
                ),
                provenance: vec![stored],
            },
        },
    )
    .await
}

/// Append one entry to the channel's ledger via the host.
async fn append(host: &Host, channel_ref: &str, entry: LedgerEntry) -> Result<()> {
    match host.resolve(channel_ref).await? {
        crate::host::Resolution::Resolved { ledger, .. } => {
            ledger.lock().await.append(entry).await?;
            Ok(())
        }
        _ => bail!("channel '{channel_ref}' did not resolve"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::test_home::HomeGuard;

    fn git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
        dir
    }

    /// A repo with one commit on a `main` branch and a configured user.
    fn git_repo_with_commit() -> tempfile::TempDir {
        let dir = git_repo();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(dir.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success(),
                "git {args:?}"
            );
        };
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        std::fs::write(dir.path().join("README.md"), "x").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        git(&["branch", "-M", "main"]);
        dir
    }

    #[tokio::test]
    async fn pr_gate_executor_ignores_an_ordinary_approval() {
        let repo = git_repo();
        let member_home = tempfile::tempdir().unwrap();
        let host = crate::host::Host::fixed_with_member_home(
            vec![repo.path().to_path_buf()],
            Some(member_home.path().to_path_buf()),
        );
        let dan = Member::human("Dan", "dan@example.com");
        let channel = host
            .open_channel(None, "c", dan.clone(), None)
            .await
            .unwrap()
            .id;
        let ledger = host.ledger_for(repo.path()).await.unwrap();
        let proposal = EntryId::new();
        // An ordinary gate (no kind tag), then its approval.
        ledger
            .lock()
            .await
            .append(LedgerEntry {
                id: proposal,
                channel,
                author: dan.clone(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::Proposal {
                    action: "do a thing".into(),
                    rationale: "because".into(),
                    provenance: vec![],
                    requirement: junto_kernel::ApprovalRequirement::Count(1),
                    frame: None,
                    kind: None,
                },
            })
            .await
            .unwrap();
        ledger
            .lock()
            .await
            .append(LedgerEntry {
                id: EntryId::new(),
                channel,
                author: dan.clone(),
                timestamp: Timestamp::now(),
                payload: EntryPayload::Approval {
                    target: proposal,
                    rationale: "ok".into(),
                },
            })
            .await
            .unwrap();

        let before = ledger
            .lock()
            .await
            .project(&channel)
            .await
            .unwrap()
            .entries
            .len();
        // No kind tag → a no-op: it must not push, open a PR, or append anything.
        execute_pr_gate_if_approved(&host, channel, proposal).await;
        let after = ledger
            .lock()
            .await
            .project(&channel)
            .await
            .unwrap()
            .entries
            .len();
        assert_eq!(before, after, "an ordinary approval triggers no PR-open");
    }

    #[tokio::test]
    async fn record_steer_note_appends_a_working_session_update() {
        let repo = git_repo();
        let member_home = tempfile::tempdir().unwrap();
        let host = crate::host::Host::fixed_with_member_home(
            vec![repo.path().to_path_buf()],
            Some(member_home.path().to_path_buf()),
        );
        let dan = Member::human("Dan", "dan@example.com");
        let channel = host
            .open_channel(None, "c", dan.clone(), None)
            .await
            .unwrap()
            .id;
        let session = EntryId::new();

        record_steer_note(
            &host,
            "c",
            channel,
            session,
            dan.clone(),
            "focus on the parser",
        )
        .await
        .unwrap();

        let ledger = host.ledger_for(repo.path()).await.unwrap();
        let view = ledger.lock().await.project(&channel).await.unwrap();
        let recorded = view
            .entries
            .iter()
            .find_map(|e| match &e.payload {
                EntryPayload::SessionUpdated {
                    target,
                    state,
                    note,
                } if *target == session => Some((*state, note.clone())),
                _ => None,
            })
            .expect("a steer SessionUpdated was recorded");
        assert_eq!(recorded.0, SessionState::Working);
        assert!(recorded.1.contains("focus on the parser"));
    }

    #[test]
    fn pr_branch_makes_committed_work_show_in_the_base_relative_diff() {
        let repo = git_repo_with_commit();
        let session = EntryId::new();

        let plan = prepare_pr_branch(repo.path(), session).unwrap();
        assert_eq!(plan.branch, format!("junto/{session}"));
        assert_eq!(plan.base, "main");

        // The worker edits AND commits to the junto branch.
        std::fs::write(repo.path().join("feature.rs"), "fn added() {}\n").unwrap();
        for args in [
            &["add", "."][..],
            &["commit", "-q", "-m", "add feature"][..],
        ] {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(repo.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }

        // `git diff HEAD` is now empty (the work is committed), but the
        // base-relative diff still shows it — so the Grader sees the change.
        let diff = workspace_diff(repo.path()).expect("committed work shows base-relative");
        assert!(diff.contains("feature.rs"), "{diff}");
        assert!(diff.contains("fn added"), "{diff}");
    }

    #[test]
    fn workspace_store_remembers_and_updates() {
        let home = HomeGuard::new();
        let channel = ChannelId::new();
        assert!(workspace_for(home.path(), &channel).unwrap().is_none());

        let repo_a = git_repo();
        remember_workspace(home.path(), &channel, repo_a.path()).unwrap();
        let stored = workspace_for(home.path(), &channel).unwrap().unwrap();
        assert_eq!(stored, dunce::canonicalize(repo_a.path()).unwrap());

        // Updating replaces, not duplicates; other channels are untouched.
        let repo_b = git_repo();
        remember_workspace(home.path(), &channel, repo_b.path()).unwrap();
        let stored = workspace_for(home.path(), &channel).unwrap().unwrap();
        assert_eq!(stored, dunce::canonicalize(repo_b.path()).unwrap());

        let other = ChannelId::new();
        assert!(workspace_for(home.path(), &other).unwrap().is_none());
    }

    #[test]
    fn non_git_workspaces_are_refused() {
        let home = HomeGuard::new();
        let channel = ChannelId::new();
        let plain = tempfile::tempdir().unwrap();
        let err = remember_workspace(home.path(), &channel, plain.path()).unwrap_err();
        assert!(err.to_string().contains("not a git repository"), "{err}");
    }

    #[test]
    fn acp_config_carries_claude_extras_but_mcp_crosses_to_any_harness() {
        let agent = crate::agent::Agent {
            slug: "reviewer".into(),
            name: "Reviewer".into(),
            harness: "claude".into(),
            email: "reviewer@junto.local".into(),
            role: Some("be careful".into()),
            model: Some("claude-opus-4-8".into()),
            mcp_servers: vec![crate::agent::McpServer {
                name: "junto".into(),
                url: "http://127.0.0.1:1727/mcp".into(),
            }],
            skills: vec!["diagnose".into()],
            plugins: vec!["/abs/plugin".into()],
        };
        // Claude agents carry role + model + skills + plugins over _meta.
        let claude = acp_config(&agent, harness_by_id("claude"));
        assert_eq!(claude.system_prompt.as_deref(), Some("be careful"));
        assert_eq!(claude.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(claude.mcp_servers.len(), 1);
        assert_eq!(claude.skills, vec!["diagnose".to_string()]);
        assert_eq!(claude.plugins, vec!["/abs/plugin".to_string()]);
        // Other harnesses get MCP (standard ACP) but not the Claude _meta extras.
        let opencode = acp_config(&agent, harness_by_id("opencode"));
        assert!(opencode.system_prompt.is_none());
        assert!(opencode.model.is_none());
        assert_eq!(opencode.mcp_servers.len(), 1);
        assert!(opencode.skills.is_empty());
        assert!(opencode.plugins.is_empty());
    }

    #[test]
    fn harness_backend_honors_the_test_stub_override() {
        // The HomeGuard's process-wide lock serializes env mutation here.
        let _home = HomeGuard::new();
        // SAFETY: env mutation is serialized by the HomeGuard lock.
        unsafe { std::env::set_var("JUNTO_HARNESS_CMD", "stub") };
        let choice = detect_harness_choice();
        assert_eq!(choice.backend, HarnessBackend::Native);
        assert!(
            choice.hint.is_none(),
            "the stub override never suggests WSL"
        );
        unsafe { std::env::remove_var("JUNTO_HARNESS_CMD") };
    }

    #[test]
    fn forced_backend_env_selects_wsl_without_probing() {
        let _home = HomeGuard::new();
        unsafe { std::env::set_var("JUNTO_HARNESS_BACKEND", "wsl") };
        // JUNTO_HARNESS_CMD must be unset for the backend env to win.
        unsafe { std::env::remove_var("JUNTO_HARNESS_CMD") };
        let choice = detect_harness_choice();
        assert_eq!(choice.backend, HarnessBackend::Wsl);
        unsafe { std::env::remove_var("JUNTO_HARNESS_BACKEND") };
    }

    #[test]
    fn stream_line_system_carries_session_and_status() {
        let effects = interpret_stream_line(
            r#"{"type":"system","subtype":"init","session_id":"h-abc","tools":[]}"#,
        );
        assert_eq!(effects.session.as_deref(), Some("h-abc"));
        assert_eq!(effects.events.len(), 1);
        assert_eq!(effects.events[0].kind, "status");
        assert!(!effects.saw_result);
    }

    #[test]
    fn stream_line_assistant_yields_text_and_tool_events() {
        let line = r#"{"type":"assistant","session_id":"h-abc","message":{"content":[
            {"type":"text","text":"Running the tests."},
            {"type":"tool_use","name":"Bash","input":{"command":"cargo test\n--workspace"}}
        ]}}"#;
        let effects = interpret_stream_line(line);
        assert_eq!(effects.events.len(), 2);
        assert_eq!(effects.events[0].kind, "assistant");
        assert_eq!(effects.events[0].text, "Running the tests.");
        assert_eq!(effects.events[1].kind, "tool");
        // The summary takes the first line of the salient input field.
        assert_eq!(effects.events[1].text, "Bash: cargo test");
    }

    #[test]
    fn stream_line_result_captures_outcome() {
        let ok = interpret_stream_line(
            r#"{"type":"result","subtype":"success","result":"all green","session_id":"h-abc","is_error":false}"#,
        );
        assert!(ok.saw_result);
        assert!(!ok.is_error);
        assert_eq!(ok.result.as_deref(), Some("all green"));
        assert_eq!(ok.events[0].kind, "result");

        let bad = interpret_stream_line(
            r#"{"type":"result","subtype":"error","result":"boom","is_error":true}"#,
        );
        assert!(bad.is_error);
        assert_eq!(bad.events[0].kind, "error");
    }

    #[test]
    fn stream_line_garbage_is_ignored() {
        assert!(interpret_stream_line("not json at all").events.is_empty());
        assert!(interpret_stream_line("").events.is_empty());
        // An unknown event type carries its session id but shows nothing.
        let unknown = interpret_stream_line(r#"{"type":"user","session_id":"h-z"}"#);
        assert!(unknown.events.is_empty());
        assert_eq!(unknown.session.as_deref(), Some("h-z"));
    }

    #[test]
    fn live_registry_replays_buffer_and_tails() {
        let live = LiveSessions::default();
        let session = EntryId::new();
        // No feed yet → no subscription.
        assert!(live.subscribe(session).is_none());

        let _ = live.begin(session);
        live.publish(session, LiveEvent::new("assistant", "first"));
        let (buffer, mut receiver) = live.subscribe(session).expect("feed is live");
        assert_eq!(buffer.len(), 1, "late joiner replays what already happened");
        assert_eq!(buffer[0].text, "first");

        // A subsequent publish reaches the live tail.
        live.publish(session, LiveEvent::new("tool", "Bash: ls"));
        let tailed = receiver.try_recv().expect("live event delivered");
        assert_eq!(tailed.text, "Bash: ls");

        // Finishing drops the feed: the receiver closes, new subscribes miss.
        live.finish(session);
        assert!(live.subscribe(session).is_none());
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Closed)
        ));
    }

    #[test]
    fn segment_events_coalesce_in_the_replay_buffer() {
        let live = LiveSessions::default();
        let session = EntryId::new();
        let _rx = live.begin(session);
        // Two frames of the same growing segment (seq 1) keep only the latest.
        live.publish(session, LiveEvent::segment("assistant", "hel", "<p>hel</p>", 1));
        live.publish(
            session,
            LiveEvent::segment("assistant", "hello", "<p>hello</p>", 1),
        );
        // A discrete line (seq 0) always appends.
        live.publish(session, LiveEvent::new("tool", "Bash: ls"));
        let (buffer, _rx2) = live.subscribe(session).expect("feed live");
        assert_eq!(
            buffer.len(),
            2,
            "same-seq frames coalesce; discrete line appends"
        );
        assert_eq!(buffer[0].text, "<p>hello</p>");
        assert!(buffer[0].html);
        assert_eq!(buffer[0].seq, 1);
        assert_eq!(buffer[1].kind, "tool");
    }

    #[test]
    fn outcome_state_maps_each_turn_end() {
        use SessionState::*;
        assert!(matches!(
            outcome_state(TurnEnd::Completed, 1, "ok"),
            (Done, _)
        ));
        assert!(matches!(
            outcome_state(TurnEnd::Failed, 1, "boom"),
            (Error, _)
        ));
        assert!(matches!(
            outcome_state(TurnEnd::TimedOut, 1, "slow"),
            (Error, _)
        ));
        // An interrupt is a human choice, not an error: the session lands Done.
        let (state, note) = outcome_state(TurnEnd::Interrupted, 2, "stopped mid-edit");
        assert_eq!(state, Done);
        assert!(note.contains("interrupted"));
    }

    #[tokio::test]
    async fn control_channel_delivers_to_live_turn_and_errors_when_idle() {
        let live = LiveSessions::default();
        let session = EntryId::new();

        // No feed yet → control reports NotLive.
        assert!(live.control(session, TurnControl::Interrupt).is_err());

        let mut control_rx = live.begin(session);
        live.control(session, TurnControl::Steer("focus on the parser".into()))
            .expect("delivered to the live turn");
        match control_rx.recv().await {
            Some(TurnControl::Steer(msg)) => assert_eq!(msg, "focus on the parser"),
            other => panic!("expected steer, got {other:?}"),
        }

        // After finish, control is NotLive again.
        live.finish(session);
        assert!(live.control(session, TurnControl::Interrupt).is_err());
    }

    #[test]
    fn harness_session_mapping_round_trips() {
        let home = HomeGuard::new();
        let session = EntryId::new();
        assert!(
            harness_session_for(home.path(), &session)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            record_turn(
                home.path(),
                &session,
                Some("h-123".into()),
                "claude",
                "claude"
            )
            .unwrap(),
            1
        );
        assert_eq!(
            harness_session_for(home.path(), &session)
                .unwrap()
                .as_deref(),
            Some("h-123")
        );
        // A later turn increments and may refresh the harness id.
        assert_eq!(
            record_turn(home.path(), &session, None, "claude", "claude").unwrap(),
            2
        );
        assert_eq!(
            harness_session_for(home.path(), &session)
                .unwrap()
                .as_deref(),
            Some("h-123")
        );
    }

    #[test]
    fn artifacts_store_with_digest() {
        let home = HomeGuard::new();
        let session = EntryId::new();
        let provenance =
            store_artifact(home.path(), &session, "turn-1-result.md", "hello").unwrap();
        assert!(provenance.uri.as_str().starts_with("file:///"));
        // sha256("hello")
        assert!(
            provenance
                .digest
                .as_ref()
                .unwrap()
                .as_str()
                .ends_with("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
    }
}
