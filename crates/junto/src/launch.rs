//! Launching Agent Sessions from the surface (`docs/adr/0023`).
//!
//! Where a session executes is resolved through `crate::mounts` (the
//! machine-local **Mount** store, `~/.junto/mounts.toml`) against the
//! channel's projected Subjects; paths never enter the ledger — they are
//! machine facts and don't sync. The harness session-id mapping
//! (`~/.junto/harness-sessions.toml`) is machine-local for the same reason.
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
    ChannelId, ChannelView, ContentDigest, EntryId, EntryPayload, LedgerEntry, Member,
    ProvenanceRef, SessionState, Timestamp, Uri,
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
/// control sender for mid-turn signals (human → turn) — `None` for a
/// session `begin` opened as non-steerable (`LiveSessions::begin`'s
/// `steerable` flag), so [`LiveSessions::control`] reports [`NotLive`] for
/// it even while the feed itself is registered. This is deliberately
/// *not* the same thing as no feed existing at all: an autonomous Outcome
/// loop turn (`crate::launch::spawn_outcome_loop`) drives itself with its
/// own inert control channel and never reads the one `begin` would hand
/// back, so a real, always-succeeding `try_send` into that channel would
/// make `steer_live` report success for a steer nobody will ever act on —
/// and `record_steer_note` would then write a `SessionUpdated` entry
/// asserting a steer that never happened. `control: None` makes that
/// failure mode structurally unreachable instead of relying on every
/// caller to remember not to steer an unsteerable session.
struct LiveFeed {
    buffer: Vec<LiveEvent>,
    sender: broadcast::Sender<LiveEvent>,
    control: Option<mpsc::Sender<TurnControl>>,
}

/// The host's in-memory registry of running sessions' live feeds. Ephemeral —
/// nothing here is part of the durable record.
#[derive(Default)]
pub struct LiveSessions {
    inner: Mutex<HashMap<EntryId, LiveFeed>>,
    /// The live plane registry (`crate::live_plane`) — one CRDT document +
    /// presence + frame broadcast per session, tapped from `begin`/
    /// `publish`/`finish` below alongside the SSE feed above. `Host`
    /// exposes it via `Host::live_plane`, delegating to this field rather
    /// than duplicating a registry on `Host` itself (`LiveSessions`'
    /// methods, not `Host`'s, are what see every session lifecycle event).
    pub(crate) plane: std::sync::Arc<crate::live_plane::LivePlane>,
}

impl LiveSessions {
    /// Open a fresh feed for a session about to run (replaces any stale one),
    /// returning the control receiver the running turn selects on (human →
    /// turn). `steerable` is `false` for an autonomous Outcome-loop turn
    /// (`crate::launch::spawn_outcome_loop`) that never reads its own
    /// control receiver — see [`LiveFeed`]'s doc for why that must not
    /// register a real control sender.
    ///
    /// Also flushes `session`'s pending annotation queue (any watcher
    /// comments queued while no turn was running) at the end, once the
    /// fresh feed and live-plane document both exist: wired in here
    /// directly, not left to the caller, so a future `begin` call site
    /// cannot silently skip it (`crate::live_bridge::flush_pending`'s docs).
    pub(crate) fn begin(
        &self,
        host: std::sync::Arc<Host>,
        channel: String,
        session: EntryId,
        steerable: bool,
    ) -> mpsc::Receiver<TurnControl> {
        let (sender, _rx) = broadcast::channel(256);
        // Capacity 1: one human, one in-flight signal at a time.
        let (control, control_rx) = mpsc::channel(1);
        let mut map = self.inner.lock().expect("live sessions registry lock");
        map.insert(
            session,
            LiveFeed {
                buffer: Vec::new(),
                sender,
                control: steerable.then_some(control),
            },
        );
        drop(map);
        // Live-plane tap: start this session's CRDT document alongside the
        // SSE feed above (docs/superpowers/specs/2026-08-20-live-session-plane-design.md).
        self.plane.begin(session);
        crate::live_bridge::flush_pending(host, channel, session);
        control_rx
    }

    /// Deliver a human's control signal to the running turn, or `Err(NotLive)`
    /// if no turn is currently streaming for the session, or the session's
    /// feed was opened non-steerable (`begin`'s `steerable` flag).
    pub(crate) fn control(&self, session: EntryId, signal: TurnControl) -> Result<(), NotLive> {
        let map = self.inner.lock().expect("live sessions registry lock");
        let feed = map.get(&session).ok_or(NotLive)?;
        feed.control
            .as_ref()
            .ok_or(NotLive)?
            .try_send(signal)
            .map_err(|_| NotLive)
    }

    /// Whether `session` currently has a registered live feed — regardless
    /// of whether it is steerable (`begin`'s `steerable` flag). Distinct
    /// from [`Self::control`] succeeding: an outcome-loop session is live
    /// but not steerable, so `control` reports [`NotLive`] for it even
    /// while a turn is genuinely still running. [`crate::launch::steer`]
    /// uses this to refuse resuming a session the host still considers
    /// live, rather than treating `control`'s `NotLive` as "safe to start
    /// a second, concurrent turn".
    #[must_use]
    pub(crate) fn is_live(&self, session: EntryId) -> bool {
        self.inner
            .lock()
            .expect("live sessions registry lock")
            .contains_key(&session)
    }

    /// Also taps the live plane first (fire-and-forget, never propagated —
    /// see `crate::live_plane`'s module docs): every event feeds the
    /// session's CRDT `conversation` container, and a tool event whose
    /// label (`acp::tool_label`) indicates a file edit or write also lands
    /// in `worktree` (reusing the same serialized value — see
    /// [`SessionLive::publish_conversation`]), so watchers see file
    /// activity without waiting for the turn-end diff snapshot.
    ///
    /// Then: into the replay buffer (bounded) and to live tails. A
    /// non-zero `seq` marks a growing segment — successive same-`seq`
    /// events **coalesce** in the replay buffer (the last one wins, so a
    /// late joiner sees one rendered block, not every intermediate frame),
    /// which also keeps a long Markdown stream from blowing the bound.
    /// Live subscribers still receive every frame.
    pub(crate) fn publish(&self, session: EntryId, event: LiveEvent) {
        if let Some(live) = self.plane.get(session) {
            let is_edit_or_write =
                event.text.starts_with("Edit") || event.text.starts_with("Write");
            if let Some(value) = live.publish_conversation(&event)
                && event.kind == "tool"
                && is_edit_or_write
            {
                live.doc.push_worktree(&value);
            }
        }
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
    /// sees the stream end and reloads to the now-persisted outcome. Also
    /// taps the live plane: removes its `SessionLive` and returns the
    /// session's final CRDT snapshot bytes for the caller to archive as an
    /// artifact (`None` if the live plane wasn't tracking the session).
    pub(crate) fn finish(&self, session: EntryId) -> Option<Vec<u8>> {
        let mut map = self.inner.lock().expect("live sessions registry lock");
        map.remove(&session);
        drop(map);
        self.plane.finish(session)
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

/// Archive a finished session's live-plane snapshot (`LivePlane::finish`'s
/// return value) as an artifact named `name`, using the exact
/// `store_artifact` + `ArtifactAttached` shape [`record_outcome`] uses for
/// `diff.patch` — same signing, same append path (see the taps in
/// `LiveSessions::finish`). `name` must be collision-free per session — a
/// constant name would let a later turn's archive silently overwrite an
/// earlier turn's already-appended `ArtifactAttached` entry's `file://`
/// target, corrupting that entry's recorded digest; callers use the same
/// `turn-{turn}-*`-numbered convention `record_outcome`'s own artifacts use,
/// falling back to an id-suffixed name where no single turn number applies.
/// `store_artifact` only writes text, so the binary snapshot is hex-encoded
/// first — the durable ledger never carries CRDT bytes verbatim, only their
/// provenance-tracked artifact.
/// `pub(crate)` (not private) so `crate::live_ws`'s end-to-end smoke test
/// (Task 11 of the live-session-plane plan) can archive a real finished
/// session the same way `spawn_turn`/`capture_turn` do — this is the only
/// path that proves the archived artifact is genuinely re-importable and
/// that the durable `ArtifactAttached` ledger entry actually lands, not a
/// hand-rolled substitute. Crate-internal only: do not re-tighten this back
/// to private without moving or duplicating that test.
pub(crate) async fn archive_live_snapshot(
    host: &Host,
    channel_ref: &str,
    channel: ChannelId,
    session: EntryId,
    agent: &crate::agent::Agent,
    name: &str,
    snapshot: &[u8],
) -> Result<()> {
    let junto_home = crate::host::junto_home()?;
    // One allocation for the whole string, not one `format!` heap `String`
    // per byte: a real turn's snapshot runs hundreds of KB to a few MB, so
    // the old `.map(|byte| format!(...)).collect()` was millions of
    // allocations on a tokio worker with no yield point in between.
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(snapshot.len() * 2);
    for byte in snapshot {
        write!(hex, "{byte:02x}").expect("writing hex digits into a String never fails");
    }
    let stored = store_artifact(&junto_home, &session, name, &hex)?;
    append(
        host,
        channel_ref,
        LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel,
            author: agent.member(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::ArtifactAttached {
                target: session,
                kind: "live-snapshot".into(),
                description: format!("live session plane snapshot ({} bytes)", snapshot.len()),
                provenance: vec![stored],
            },
        },
    )
    .await
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

/// A workspace's uncommitted-changes diff (`git diff` against the right
/// base, plus untracked file names), paired with the commit oid that diff
/// is relative to.
#[derive(Default)]
struct WorkspaceDiff {
    text: String,
    /// The commit oid `text` is relative to: the recorded PR-branch base
    /// commit (`branch.<branch>.juntoBaseSha`) when on a `junto/<session>`
    /// branch, otherwise `HEAD`'s own oid — resolved together with `text`
    /// itself so the two can never drift to different commits between two
    /// separate git invocations. `None` only when the workspace has no
    /// commits at all yet (nothing for `HEAD` to name); `text` may still be
    /// non-empty in that case (untracked files in a brand-new repo), so
    /// callers that only want the diff text must not treat `None` here as
    /// "no diff" — only a caller that needs a real oid to anchor against
    /// (the live plane's worktree-diff tap) may skip on `None`, and must
    /// never fabricate one.
    commit: Option<String>,
}

/// The workspace's uncommitted changes (`git diff HEAD` + untracked names),
/// or `None` when clean — see [`WorkspaceDiff`] for the paired commit oid.
fn workspace_diff(workspace: &Path) -> Option<WorkspaceDiff> {
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
    let commit = workspace_commit(workspace);
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
    let mut text = diff;
    if !untracked.is_empty() {
        text.push_str("\n# untracked files:\n");
        for line in untracked {
            text.push_str(line);
            text.push('\n');
        }
    }
    Some(WorkspaceDiff { text, commit })
}

/// Resolve the workspace's real commit oid, independent of whether there
/// happens to be a diff right now: the recorded PR-branch base commit
/// (`branch.<branch>.juntoBaseSha`) when on a `junto/<session>` branch,
/// otherwise `HEAD`'s own oid. The one oid-resolution codepath —
/// [`workspace_diff`] uses it for [`WorkspaceDiff::commit`], and the
/// live-plane begin-time worktree tap (`spawn_turn`, `spawn_outcome_loop`)
/// calls it directly, because "no diff yet" (`workspace_diff`'s own
/// gating condition — it returns `None` on a clean workspace even when a
/// commit is perfectly resolvable) is not the same fact as "no commit to
/// anchor against": a freshly connected watcher on an otherwise-clean
/// workspace still has a real `HEAD` to anchor a `CodeAnchor` comment on.
/// `None` only when the workspace has no commits at all yet; never
/// fabricated.
fn workspace_commit(workspace: &Path) -> Option<String> {
    match pr_branch_base(workspace) {
        Some(base_sha) => Some(base_sha),
        None => workspace_head_commit(workspace),
    }
}

/// Push a `{"kind":"diff", ...}` worktree entry carrying the workspace's
/// current commit oid into `session`'s live-plane document, if one exists
/// and a commit resolves — the begin-time half of the live plane's
/// worktree tap (BLOCKER 2 of the final branch review). Before this
/// existed, the only worktree push happened after the turn finished, in
/// the same statement that torn the live connection down
/// (`host.live().finish`), so the commit oid and the connection's end
/// always arrived together and the composer's `CodeAnchor` path could
/// never be exercised on a running session; `spawn_outcome_loop` pushed no
/// worktree entry at all. Shared by both `spawn_turn`'s and
/// `spawn_outcome_loop`'s `begin` sequence so a watcher connecting at any
/// point during the run has a commit to anchor against, not only once the
/// turn (or the whole Outcome loop) ends. No-op when `session` has no live
/// plane entry or the workspace has no commits yet — never fabricates an
/// oid (see [`workspace_commit`]).
fn push_begin_worktree_diff(host: &Host, session: EntryId, workspace: &Path) {
    if let Some(live) = host.live_plane().get(session)
        && let Some(commit) = workspace_commit(workspace)
    {
        let text = workspace_diff(workspace)
            .map(|diff| diff.text)
            .unwrap_or_default();
        live.doc.push_worktree(&serde_json::json!({
            "kind": "diff",
            "text": text,
            "commit": commit,
        }));
    }
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

/// The workspace's current `HEAD` commit oid, or `None` if git can't say
/// (not a repo, or an initial repo with no commits yet). [`workspace_diff`]
/// uses this as the non-PR-branch case of [`WorkspaceDiff::commit`] — never
/// a fabricated oid, since the live plane's worktree-diff tap anchors
/// watcher code comments on it.
fn workspace_head_commit(workspace: &Path) -> Option<String> {
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(workspace).args(["rev-parse", "HEAD"]);
    no_console_window(&mut command);
    let out = command.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|sha| !sha.is_empty())
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

/// Where a session's agent should run.
///
/// The first `Execute`-capable mounted subject wins (spec §1) — the same
/// subject [`crate::mounts::capabilities`] reports as executable, given
/// whatever this machine has mounted for it. A channel with none — a
/// Document subject, or no subject at all — is not a refusal: it gets a
/// fresh, per-session scratch directory under `<junto_home>/scratch/<session>`,
/// deliberately never a git repository. This is the mechanism that makes a
/// repo-free channel work: nothing past this call requires git.
///
/// # Errors
///
/// Returns an error when the mount store cannot be read, or the scratch
/// directory cannot be created.
pub fn session_workdir(junto_home: &Path, view: &ChannelView, session: EntryId) -> Result<PathBuf> {
    let subjects: Vec<_> = view.subjects.iter().map(|(_, s)| s.clone()).collect();
    let mounts = crate::mounts::mounts_for(junto_home, &subjects)?;
    for subject in &subjects {
        let mount = mounts.iter().find(|m| m.uri == subject.uri);
        if crate::mounts::capabilities(subject, mount).contains(&crate::mounts::Capability::Execute)
            && let Some(mount) = mount
        {
            return Ok(mount.path.clone());
        }
    }
    let scratch = junto_home.join("scratch").join(session.to_string());
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("creating scratch dir {}", scratch.display()))?;
    Ok(scratch)
}

/// Whether `workspace` is one of `session_workdir`'s own scratch
/// directories rather than a mounted subject. `Capability::Diff` — the
/// condition [`prepare_pr_branch`] needs — can only hold for a mounted
/// `Repo` subject (`crate::mounts::capabilities`), and `session_workdir`'s
/// only two possible outputs are such a mount or a scratch directory under
/// `<junto_home>/scratch`, so this prefix check is exact, not a heuristic.
fn is_scratch_workdir(junto_home: &Path, workspace: &Path) -> bool {
    workspace.starts_with(junto_home.join("scratch"))
}

/// Launch a new Agent Session: append `SessionStarted` (authored as the
/// harness member), then run the first turn in the background. `session` is
/// minted by the caller (`crate::web::launch_session`) rather than here, so
/// it can resolve that same id's workdir (`session_workdir`) before the
/// session exists in the ledger. Returns the session's entry id — the page
/// shows the live state.
pub async fn launch(
    host: std::sync::Arc<Host>,
    channel: ChannelId,
    channel_ref: String,
    session: EntryId,
    workspace: PathBuf,
    intent: String,
    agent: crate::agent::Agent,
) -> Result<EntryId> {
    append(
        &host,
        &channel_ref,
        LedgerEntry {
            signature: None,
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
         Do the work in this workspace.)"
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
///
/// Refuses (`Err`) when the session currently has a registered live feed
/// ([`LiveSessions::is_live`]) — this is the between-turns resume path,
/// only correct when nothing is already running; a live but non-steerable
/// session (an Outcome-loop turn — `crate::launch::spawn_outcome_loop`'s
/// `steerable: false`) makes `steer_live` report `NotLive` the same way a
/// truly idle session does, and without this guard `steer_session`'s
/// `NotLive` fallback would resume it here anyway: `harness_session_for`
/// already has a mapping recorded from the loop's own first turn, so
/// `spawn_turn` would succeed in starting a *second*, concurrent agent
/// process over the same workspace, and its `begin`/`finish` would steal
/// and archive the loop's own live snapshot out from under it.
pub async fn steer(
    host: std::sync::Arc<Host>,
    channel: ChannelId,
    channel_ref: String,
    workspace: PathBuf,
    session: EntryId,
    steered_by: Member,
    message: String,
) -> Result<()> {
    if host.live().is_live(session) {
        bail!(
            "session {session} still has a live turn running — steer it in place instead of \
             starting a second one"
        );
    }
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
            signature: None,
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
///
/// Also reused by `crate::live_bridge` (IMPORTANT 4 of the final branch
/// review) to author a steer note as the session's own driving agent when
/// the sender's identity has no local signing key on file — the same
/// "which agent runs this session" question, asked from a different
/// caller.
pub(crate) fn resume_agent(junto_home: &Path, session: &EntryId) -> Result<crate::agent::Agent> {
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
    // Steerable: an interactive session's turn selects on this receiver
    // (`run_turn`, below) and acts on a delivered `TurnControl::Steer`.
    // `begin` also flushes any annotations queued while no turn was running
    // for this session (e.g. between turns) now that the fresh control
    // channel is up.
    let mut control_rx = host.live().begin(
        std::sync::Arc::clone(&host),
        channel_ref.clone(),
        session,
        true,
    );
    // Populate the live plane's workspace so `crate::live_bridge` can
    // re-anchor annotations against it; this is the one place a running
    // turn's workspace path is in hand at `begin` time.
    if let Some(live) = host.live_plane().get(session) {
        *live.workspace.lock().expect("live plane workspace lock") = Some(workspace.clone());
    }
    tokio::spawn(async move {
        // Live-plane worktree tap, begin-time half (BLOCKER 2 of the final
        // branch review): the only other worktree push used to happen
        // after the turn finished, whose very next statement tears the
        // live connection down (`host.live().finish`, below) — so the
        // commit oid and the connection's end always arrived in the same
        // instant, and the composer's `CodeAnchor` path could never be
        // exercised on a running session. Push once, here, before the turn
        // even starts, so a watcher connecting mid-turn has a commit to
        // anchor against immediately. See `push_begin_worktree_diff`'s
        // docs for the oid-resolution guarantee.
        push_begin_worktree_diff(&host, session, &workspace);
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
        let turn = match record_outcome(
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
            Ok(turn) => Some(turn),
            Err(err) => {
                tracing::warn!("recording session {session} outcome failed: {err:#}");
                None
            }
        };
        // Live-plane worktree tap, turn-end half (see the begin-time push
        // above, in this same task, for the other half): one more diff
        // snapshot pushed while the session's `SessionLive` still exists
        // (turn-end is the periodic floor for v1 — no in-turn ticker, no
        // filesystem watcher). Fire-and-forget: never lets a live-plane
        // hiccup affect the outcome already recorded above. Pushed only
        // when `workspace_diff` resolved a real commit oid to pair with
        // the diff — never a fabricated one, since watcher UIs anchor
        // code comments on it (see `WorkspaceDiff::commit`).
        if let Some(live) = host.live_plane().get(session)
            && let Some(diff) = workspace_diff(&workspace)
            && let Some(commit) = diff.commit
        {
            live.doc.push_worktree(&serde_json::json!({
                "kind": "diff",
                "text": diff.text,
                "commit": commit,
            }));
        }
        // Close the live feed only after the outcome is recorded, so a
        // watcher reloading on stream-end sees the landed memo + diff, not
        // "working". Also archives the live plane's final snapshot as an
        // artifact, named per turn (like `record_outcome`'s own
        // `turn-{turn}-*` artifacts) so a later steered turn on the same
        // session never overwrites an earlier turn's archived snapshot;
        // falls back to a collision-safe id (the `grade-{}.md` precedent)
        // when the turn number itself couldn't be recorded above.
        if let Some(snapshot) = host.live().finish(session) {
            let name = match turn {
                Some(turn) => format!("turn-{turn}-live.loro"),
                None => format!("live-{}.loro", EntryId::new()),
            };
            if let Err(err) = archive_live_snapshot(
                &host,
                &channel_ref,
                channel,
                session,
                &agent,
                &name,
                &snapshot,
            )
            .await
            {
                tracing::warn!("archiving live snapshot for session {session} failed: {err:#}");
            }
        }
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

/// Record a finished turn's outcome: the result memo + workspace diff
/// Artifacts and the final session state, authored as the harness member.
/// Returns the recorded turn number ([`record_turn`]'s own counter) so the
/// caller can name a same-turn artifact (the live-plane snapshot) without
/// calling `record_turn` a second time, which would double-increment it.
async fn record_outcome(
    host: &Host,
    channel_ref: &str,
    channel: ChannelId,
    session: EntryId,
    workspace: &Path,
    outcome: &TurnOutcome,
    agent: &crate::agent::Agent,
) -> Result<u32> {
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
            signature: None,
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
            &diff.text,
        )?;
        append(
            host,
            channel_ref,
            LedgerEntry {
                signature: None,
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
            signature: None,
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
    .await?;
    Ok(turn)
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
/// runs out, at which point it escalates to a human Gate. `session` is
/// minted by the caller for the same reason `launch` takes it: so it can
/// resolve the same id's workdir (`session_workdir`) before the session
/// exists in the ledger. Returns the session id; the loop runs in the
/// background.
pub async fn launch_outcome(
    host: std::sync::Arc<Host>,
    channel: ChannelId,
    channel_ref: String,
    session: EntryId,
    workspace: PathBuf,
    intent: String,
    agent: crate::agent::Agent,
) -> Result<EntryId> {
    append(
        &host,
        &channel_ref,
        LedgerEntry {
            signature: None,
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
        // Not steerable (finding 2 of the Task 8 fix round): this loop
        // drives its own inert control channel via `run_worker_turn`'s
        // local `mpsc::channel`, never the receiver `begin` hands back
        // here, so a real registered control sender would let `steer_live`
        // report success for a steer nobody will ever act on. `begin`
        // still does the live-plane/SSE bookkeeping — but the pending
        // annotations it flushes never actually reach this loop while it
        // stays non-steerable: `flush_pending`'s delivery still calls
        // `steer_live`, which still reports `NotLive` for a non-steerable
        // session, so every flush attempt just requeues. See
        // `crate::live_bridge::flush_pending`'s doc for the accepted
        // known-gap trade-off this implies.
        let _control_rx = host.live().begin(
            std::sync::Arc::clone(&host),
            channel_ref.clone(),
            session,
            false,
        );
        // Populate the live plane's workspace so `crate::live_bridge` can
        // re-anchor annotations against it.
        if let Some(live) = host.live_plane().get(session) {
            *live.workspace.lock().expect("live plane workspace lock") = Some(workspace.clone());
        }
        // Live-plane worktree tap, begin-time (BLOCKER 2 of the final
        // branch review): before this fix `spawn_outcome_loop` never
        // pushed a `kind: "diff"` worktree entry at all, so
        // `worktree_commit` stayed `None` for the whole loop and the
        // composer's `CodeAnchor` path always refused with "no commit
        // seen yet". Push once, here, before the PR branch is even
        // prepared, so a watcher connecting during the loop has a commit
        // to anchor against immediately. See `push_begin_worktree_diff`'s
        // docs for the oid-resolution guarantee.
        push_begin_worktree_diff(&host, session, &workspace);

        // Prepare a PR branch the worker commits onto (the push-gate's
        // deliverable). Only meaningful where `Capability::Diff` holds — a
        // `session_workdir` scratch directory is deliberately never a git
        // repository (Task 6: a session runs without a repo), so skip
        // straight past `prepare_pr_branch` instead of letting it fail
        // loudly for a workspace that was never going to have a branch.
        // Best-effort otherwise: without a branch, grading falls back to
        // the working-tree diff either way. workspace_diff self-discovers
        // the recorded base.
        let scratch =
            crate::host::junto_home().is_ok_and(|home| is_scratch_workdir(&home, &workspace));
        let branch_plan = if scratch {
            tracing::info!(
                "outcome session {session}: workspace {} has no Diff capability (a \
                 scratch dir, not a mounted repo) — skipping PR branch preparation, \
                 grading the working-tree diff",
                workspace.display()
            );
            None
        } else {
            match prepare_pr_branch(&workspace, session) {
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
                scratch,
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
        // No single turn number describes this snapshot — the outcome loop
        // spans every iteration's `capture_turn` call under one `begin`/
        // `finish` pair, unlike `spawn_turn`'s one-turn-per-archive case
        // (see `archive_live_snapshot`'s doc comment) — so fall back to the
        // `grade-{}.md` precedent's id-suffixed, collision-free naming.
        if let Some(snapshot) = host.live().finish(session)
            && let Err(err) = archive_live_snapshot(
                &host,
                &channel_ref,
                channel,
                session,
                &agent,
                &format!("live-{}.loro", EntryId::new()),
                &snapshot,
            )
            .await
        {
            tracing::warn!("archiving live snapshot for session {session} failed: {err:#}");
        }
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
    scratch: bool,
) {
    let prompt = match (feedback, scratch) {
        (None, true) => format!(
            "{intent}\n\n(Launched from junto channel '{channel_ref}'; junto session \
             {session}. Do the work in this workspace. It has no git repository mounted, \
             so there is no branch to commit onto or PR to open — junto grades your \
             working-tree changes directly.)"
        ),
        (None, false) => format!(
            "{intent}\n\n(Launched from junto channel '{channel_ref}'; junto session \
             {session}. Do the work in this repository. When the change is complete, \
             commit it to the current git branch with a clear message — junto pushes that \
             branch and opens the pull request.)"
        ),
        (Some(findings), true) => format!(
            "Verification found problems with your last change. Fix them, then \
             stop.\n\n{findings}"
        ),
        (Some(findings), false) => format!(
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
    // Live-plane segment-boundary reset: this session's `LiveDoc` (if any)
    // is shared across every iteration of the Outcome loop under one
    // `begin`/`finish` pair (unlike `spawn_turn`'s one-document-per-turn
    // case), and `run_turn`'s `acp::FeedState` mints a fresh segment
    // counter starting at 1 on every call — without this, the next
    // iteration's first `seq: 1` push would silently overwrite this turn's
    // own `seq: 1` entry (see `LiveDoc::reset_segment_state`'s doc
    // comment). Fire-and-forget, same as every other tap.
    if let Some(live) = host.live_plane().get(session) {
        live.doc.reset_segment_state();
    }
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
    let prompt = crate::grader::grader_prompt(crate::grader::default_code_pr_rubric(), &diff.text);
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
    // Same segment-boundary reset as `run_worker_turn` — the grader turn is
    // another `run_turn` call sharing this session's one `LiveDoc` across
    // the whole Outcome loop, and its own `acp::FeedState` also restarts
    // its segment counter at 1.
    if let Some(live) = host.live_plane().get(session) {
        live.doc.reset_segment_state();
    }

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
            signature: None,
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
            &diff.text,
        )?;
        append(
            host,
            channel_ref,
            LedgerEntry {
                signature: None,
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
            signature: None,
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
            signature: None,
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
                            signature: None,
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
                    &diff.text,
                )?);
            }
            append(
                host,
                channel_ref,
                LedgerEntry {
                    signature: None,
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
                    signature: None,
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
                    &diff.text,
                )?);
            }
            append(
                host,
                channel_ref,
                LedgerEntry {
                    signature: None,
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
                    signature: None,
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
    let subjects: Vec<_> = view.subjects.iter().map(|(_, s)| s.clone()).collect();
    let mounts = crate::mounts::mounts_for(&home, &subjects)?;
    let Some(workspace) = mounts.into_iter().next().map(|mount| mount.path) else {
        bail!("channel {channel} has no subject to run in — attach one first");
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
                    signature: None,
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
                    signature: None,
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
            signature: None,
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
            signature: None,
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
            signature: None,
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
            signature: None,
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

/// Append one entry to the channel's ledger via the host — signed with its
/// author's machine-local key first (`docs/adr/0033`), so session records and
/// artifacts carry the agent's own signature, never the operator's.
async fn append(host: &Host, channel_ref: &str, mut entry: LedgerEntry) -> Result<()> {
    host.sign_entry(&mut entry);
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
                signature: None,
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
                signature: None,
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

    #[tokio::test]
    async fn steer_refuses_to_resume_a_session_with_a_live_feed() {
        // Round 2, finding 1: making the outcome loop non-steerable turned
        // `steer_live`'s `NotLive` into the *first*-steer outcome for a
        // live-but-non-steerable session, not just a second-steer race —
        // so `steer_session`'s `NotLive` fallback would resume it here
        // every time without this guard, starting a second, concurrent
        // turn over the same workspace. `is_live` must refuse regardless
        // of whether the feed is steerable — a live but non-steerable
        // feed (`begin(steerable: false)`, the outcome-loop shape) must
        // be refused exactly like a steerable one.
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
        let _rx = host.live().begin(host.clone(), "c".into(), session, false);

        let err = steer(
            host.clone(),
            channel,
            "c".into(),
            repo.path().to_path_buf(),
            session,
            dan,
            "keep going".into(),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("live turn running"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn begin_time_worktree_tap_publishes_a_commit_before_any_turn_runs() {
        // BLOCKER 2 of the final branch review: pre-fix, a worktree
        // `kind: "diff"` entry — the ONLY permitted source of a
        // `CodeAnchor`'s commit — was pushed exactly once, after the turn
        // finished, in the statement right before the live connection
        // tore down. So the commit oid and the connection's end always
        // arrived together, and the composer's code-anchor path could
        // never be exercised on a running session. This proves the fix:
        // a commit is visible in the worktree container immediately after
        // `begin`, well before any turn (let alone turn-end work) runs.
        let repo = git_repo_with_commit();
        let member_home = tempfile::tempdir().unwrap();
        let host = crate::host::Host::fixed_with_member_home(
            vec![repo.path().to_path_buf()],
            Some(member_home.path().to_path_buf()),
        );
        let session = EntryId::new();
        let _rx = host.live().begin(host.clone(), "c".into(), session, false);
        if let Some(live) = host.live_plane().get(session) {
            *live.workspace.lock().expect("live plane workspace lock") =
                Some(repo.path().to_path_buf());
        }

        // The exact call both `spawn_turn` and `spawn_outcome_loop` make
        // at `begin`, before starting (or, for the loop, even preparing
        // the PR branch for) any turn.
        push_begin_worktree_diff(&host, session, repo.path());

        let live = host
            .live_plane()
            .get(session)
            .expect("session registered by begin");
        assert_eq!(
            live.doc.worktree_len(),
            1,
            "begin-time push must land exactly one worktree entry before any turn-end work runs"
        );
        let event = live
            .doc
            .worktree_event(0)
            .expect("worktree entry must parse back as JSON");
        assert_eq!(event["kind"], "diff");
        let commit = event["commit"]
            .as_str()
            .expect("commit must be present — the composer refuses without one");
        assert_eq!(
            commit,
            workspace_head_commit(repo.path()).expect("HEAD resolves in a committed repo"),
            "must carry the real resolved oid, never a fabricated one"
        );
    }

    #[tokio::test]
    async fn archive_live_snapshot_records_a_named_artifact() {
        let home = HomeGuard::new();
        let repo = git_repo();
        let host = crate::host::Host::fixed_with_member_home(
            vec![repo.path().to_path_buf()],
            Some(home.path().to_path_buf()),
        );
        let dan = Member::human("Dan", "dan@example.com");
        let channel = host
            .open_channel(None, "c", dan.clone(), None)
            .await
            .unwrap()
            .id;
        let session = EntryId::new();
        let agent = crate::agent::Agent {
            slug: "claude".into(),
            name: "Claude".into(),
            harness: "claude".into(),
            email: "claude@junto.local".into(),
            role: None,
            model: None,
            mcp_servers: Vec::new(),
            skills: Vec::new(),
            plugins: Vec::new(),
        };

        archive_live_snapshot(
            &host,
            "c",
            channel,
            session,
            &agent,
            "turn-1-live.loro",
            b"snapshot-bytes",
        )
        .await
        .unwrap();

        let ledger = host.ledger_for(repo.path()).await.unwrap();
        let view = ledger.lock().await.project(&channel).await.unwrap();
        let recorded = view
            .entries
            .iter()
            .find_map(|e| match &e.payload {
                EntryPayload::ArtifactAttached {
                    target,
                    kind,
                    provenance,
                    ..
                } if *target == session && kind == "live-snapshot" => Some(provenance.clone()),
                _ => None,
            })
            .expect("a live-snapshot artifact was recorded");
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].uri.as_str().ends_with("turn-1-live.loro"));

        // `store_artifact` only writes text, so the archived content is the
        // hex encoding of the snapshot bytes, not the raw bytes.
        let stored = std::fs::read_to_string(
            home.path()
                .join("artifacts")
                .join(session.to_string())
                .join("turn-1-live.loro"),
        )
        .unwrap();
        assert_eq!(stored, "736e617073686f742d6279746573"); // hex("snapshot-bytes")
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
        assert!(diff.text.contains("feature.rs"), "{}", diff.text);
        assert!(diff.text.contains("fn added"), "{}", diff.text);
        // The paired commit must be the PR-branch base, not the new "add
        // feature" commit — otherwise a watcher would anchor this diff's
        // line numbers against the wrong blob, which is exactly the bug
        // pairing the two together in one call is meant to prevent.
        let base_sha = pr_branch_base(repo.path()).expect("base sha recorded");
        assert_eq!(diff.commit.as_deref(), Some(base_sha.as_str()));
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
        let host = crate::host::Host::fixed(vec![]);
        let session = EntryId::new();
        // No feed yet → no subscription.
        assert!(live.subscribe(session).is_none());

        let _ = live.begin(std::sync::Arc::clone(&host), "c".into(), session, true);
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
        let host = crate::host::Host::fixed(vec![]);
        let session = EntryId::new();
        let _rx = live.begin(std::sync::Arc::clone(&host), "c".into(), session, true);
        // Two frames of the same growing segment (seq 1) keep only the latest.
        let frame1 = LiveEvent::segment("assistant", "hel", "<p>hel</p>", 1);
        let frame2 = LiveEvent::segment("assistant", "hello", "<p>hello</p>", 1);
        live.publish(session, frame1);
        live.publish(session, frame2);
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
    fn worktree_tap_gates_on_edit_and_write_tool_labels() {
        let live = LiveSessions::default();
        let host = crate::host::Host::fixed(vec![]);
        let session = EntryId::new();
        let _rx = live.begin(std::sync::Arc::clone(&host), "c".into(), session, true);
        let plane = live.plane.get(session).expect("plane session began");

        // A non-tool event, and a tool event whose label is neither `Edit`
        // nor `Write` (`acp::tool_label`'s shapes), must never land in
        // `worktree` — only feed `conversation`.
        live.publish(session, LiveEvent::new("assistant", "hello"));
        live.publish(session, LiveEvent::new("tool", "Bash: cargo test"));
        assert_eq!(plane.doc.worktree_len(), 0);
        assert_eq!(
            plane.doc.conversation_len(),
            2,
            "both still feed conversation"
        );

        // Edit/Write-labeled tool events land in both containers.
        live.publish(session, LiveEvent::new("tool", "Edit: src/x.rs"));
        live.publish(session, LiveEvent::new("tool", "Write: src/y.rs"));
        assert_eq!(plane.doc.worktree_len(), 2);
        assert_eq!(plane.doc.conversation_len(), 4);
    }

    #[test]
    fn finish_broadcasts_frame_end_to_connected_watchers() {
        let live = LiveSessions::default();
        let host = crate::host::Host::fixed(vec![]);
        let session = EntryId::new();
        let _rx = live.begin(std::sync::Arc::clone(&host), "c".into(), session, true);
        let plane = live.plane.get(session).expect("plane session began");
        let mut watcher = plane.outbound.subscribe();

        live.finish(session);
        assert!(matches!(watcher.try_recv(), Ok(junto_live::Frame::End)));
    }

    #[test]
    fn begin_ends_a_stale_session_before_replacing_it() {
        let live = LiveSessions::default();
        let host = crate::host::Host::fixed(vec![]);
        let session = EntryId::new();
        let _rx1 = live.begin(std::sync::Arc::clone(&host), "c".into(), session, true);
        let stale = live.plane.get(session).expect("plane session began");
        let mut stale_watcher = stale.outbound.subscribe();

        // Re-`begin` without an intervening `finish` (the prior turn's task
        // never got there — a panic, or a process restart mid-turn) must
        // still end the stale watcher's stream, exactly as `finish` would.
        let _rx2 = live.begin(std::sync::Arc::clone(&host), "c".into(), session, true);
        assert!(matches!(
            stale_watcher.try_recv(),
            Ok(junto_live::Frame::End)
        ));
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
        let host = crate::host::Host::fixed(vec![]);
        let session = EntryId::new();

        // No feed yet → control reports NotLive.
        assert!(live.control(session, TurnControl::Interrupt).is_err());

        let mut control_rx = live.begin(std::sync::Arc::clone(&host), "c".into(), session, true);
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
    fn non_steerable_feed_never_accepts_control() {
        // The Outcome loop's autonomous turn (`spawn_outcome_loop`) drives
        // itself with its own inert control channel and never reads the one
        // `begin` hands back; `steerable: false` must make `control` report
        // `NotLive` regardless, or a "successful" steer would silently go
        // nowhere while `steer_live` still records a ledger note claiming
        // it landed (finding 2 of the Task 8 fix round).
        let live = LiveSessions::default();
        let host = crate::host::Host::fixed(vec![]);
        let session = EntryId::new();
        let _rx = live.begin(std::sync::Arc::clone(&host), "c".into(), session, false);
        assert!(
            live.control(session, TurnControl::Interrupt).is_err(),
            "a non-steerable feed must never accept a control signal"
        );
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

    #[tokio::test]
    async fn a_channel_with_no_executable_subject_runs_in_a_scratch_directory() {
        let home = HomeGuard::new();
        let session = EntryId::new();
        let view = channel_view_with_subjects(&[]).await;

        let dir = session_workdir(home.path(), &view, session).expect("a workdir");
        assert!(dir.exists(), "the scratch directory must be created");
        assert!(
            dir.starts_with(home.path().join("scratch")),
            "scratch dirs live under the junto home, not in a repo: {}",
            dir.display()
        );
        assert!(
            !dir.join(".git").exists(),
            "a scratch dir is deliberately not a git repo"
        );
    }

    #[tokio::test]
    async fn a_mounted_repo_subject_wins_over_the_scratch_directory() {
        let home = HomeGuard::new();
        let repo = git_repo();
        let uri = junto_kernel::Uri::new("git+https://example.com/a.git").expect("valid uri");
        crate::mounts::remember_mount(home.path(), &uri, repo.path()).unwrap();

        let subject = junto_kernel::Subject::new(junto_kernel::SubjectKind::Repo, uri);
        let view = channel_view_with_subjects(&[subject]).await;

        let dir = session_workdir(home.path(), &view, EntryId::new()).expect("a workdir");
        assert_eq!(dir, dunce::canonicalize(repo.path()).unwrap());
    }

    /// A `ChannelView` carrying just the subjects a workdir test needs.
    ///
    /// Built through the kernel's in-memory substrate rather than a git-refs
    /// one: `session_workdir` reads only `view.subjects`, so an in-memory
    /// ledger is the honest minimum here.
    async fn channel_view_with_subjects(subjects: &[junto_kernel::Subject]) -> ChannelView {
        use junto_kernel::{
            ChannelId, EntryId, EntryPayload, InMemorySubstrate, Ledger, LedgerEntry, Member,
            Timestamp,
        };
        let mut ledger = Ledger::new(InMemorySubstrate::new());
        let channel = ChannelId::new();
        let dan = Member::human("Dan", "dan@example.com");
        let mut millis: i64 = 1;
        let mut payloads = vec![EntryPayload::ChannelOpened {
            name: "workdir".into(),
        }];
        for subject in subjects {
            payloads.push(EntryPayload::SubjectAttached {
                subject: subject.clone(),
            });
        }
        for payload in payloads {
            ledger
                .append(LedgerEntry {
                    signature: None,
                    id: EntryId::new(),
                    channel,
                    author: dan.clone(),
                    timestamp: Timestamp::from_millis(millis),
                    payload,
                })
                .await
                .expect("append entry");
            millis += 1;
        }
        ledger.project(&channel).await.expect("project")
    }

    #[test]
    fn a_mounted_repo_workspace_is_not_a_scratch_workdir() {
        let home = HomeGuard::new();
        let repo = git_repo();
        assert!(
            !is_scratch_workdir(home.path(), repo.path()),
            "a real mount must never be mistaken for a scratch dir"
        );
    }

    #[test]
    fn a_workdir_under_the_scratch_root_is_a_scratch_workdir() {
        let home = HomeGuard::new();
        let scratch = home.path().join("scratch").join(EntryId::new().to_string());
        std::fs::create_dir_all(&scratch).unwrap();
        assert!(
            is_scratch_workdir(home.path(), &scratch),
            "session_workdir's own scratch fallback must be recognized as one"
        );
    }
}
