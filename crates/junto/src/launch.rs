//! Launching Agent Sessions from the surface (`docs/adr/0023`).
//!
//! The **Workspace** is the machine-local mapping channel → repo(s) — where a
//! channel's Agent Sessions execute (`~/.junto/workspaces.toml`). Paths never
//! enter the ledger: they are machine facts and don't sync. The harness
//! session-id mapping (`~/.junto/harness-sessions.toml`) is machine-local for
//! the same reason.
//!
//! v1 invocation is **`oneshot-exec`**: spawn `claude -p` in the workspace
//! with `--dangerously-skip-permissions`, parse the JSON result, attach the
//! result memo + workspace `git diff` as artifacts (content written under
//! `~/.junto/artifacts/`, referenced by `file://` URI + sha256 digest —
//! never blobs in the ledger), and mark the session done/error. Steering is
//! a later `--resume <harness-session-id>` turn; state lives in the
//! harness's own session storage, so host restarts are harmless. **No
//! `AgentHarnessAdapter` trait yet** — rule of three; this is the first
//! concrete harness, extracted when OpenCode lands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use tokio::sync::broadcast;

use junto_kernel::{
    ChannelId, ContentDigest, EntryId, EntryPayload, LedgerEntry, Member, ProvenanceRef,
    SessionState, Timestamp, Uri,
};

use crate::host::Host;

/// The launched harness's member identity — sessions are authored as the
/// agent, never as the operator (`docs/adr/0012`/`0020`). One concrete
/// harness for now (rule of three); this constant moves behind the
/// `AgentHarnessAdapter` trait when the second harness lands.
pub fn harness_member() -> Member {
    Member::agent("Claude Code", "claude-code@anthropic.com")
}

/// The harness command line, overridable for tests (`JUNTO_HARNESS_CMD`
/// names a program that accepts the same trailing arguments and prints a
/// `claude -p --output-format json`-shaped result).
fn harness_program() -> String {
    std::env::var("JUNTO_HARNESS_CMD").unwrap_or_else(|_| "claude".to_string())
}

/// How long a turn may run before the host kills it (docs/adr/0023).
const TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

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
    /// The harness's own session id (what `--resume` takes).
    harness: String,
    /// Turns run so far (names the artifact files).
    turns: u32,
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

fn record_turn(junto_home: &Path, junto: &EntryId, harness: Option<String>) -> Result<u32> {
    let mut file = load_harness_sessions(junto_home)?;
    let turn = match file.sessions.iter_mut().find(|r| r.junto == *junto) {
        Some(record) => {
            record.turns += 1;
            if let Some(harness) = harness {
                record.harness = harness;
            }
            record.turns
        }
        None => {
            file.sessions.push(HarnessSessionRecord {
                junto: *junto,
                harness: harness.unwrap_or_default(),
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
    /// `status` (lifecycle), `assistant` (model text), `tool` (a named
    /// action), `result` (final summary), or `error`.
    pub kind: String,
    /// A human-readable line — assistant text, or a tool action like
    /// `Bash: cargo test`.
    pub text: String,
}

impl LiveEvent {
    fn new(kind: &str, text: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            text: text.into(),
        }
    }
}

/// Per-session live feed: a bounded replay buffer (for a page that loads
/// mid-turn) plus a broadcast sender for the live tail.
struct LiveFeed {
    buffer: Vec<LiveEvent>,
    sender: broadcast::Sender<LiveEvent>,
}

/// The host's in-memory registry of running sessions' live feeds. Ephemeral —
/// nothing here is part of the durable record.
#[derive(Default)]
pub struct LiveSessions {
    inner: Mutex<HashMap<EntryId, LiveFeed>>,
}

impl LiveSessions {
    /// Open a fresh feed for a session about to run (replaces any stale one).
    fn begin(&self, session: EntryId) {
        let (sender, _rx) = broadcast::channel(256);
        let mut map = self.inner.lock().expect("live sessions registry lock");
        map.insert(
            session,
            LiveFeed {
                buffer: Vec::new(),
                sender,
            },
        );
    }

    /// Append an event: into the replay buffer (bounded) and to live tails.
    fn publish(&self, session: EntryId, event: LiveEvent) {
        let mut map = self.inner.lock().expect("live sessions registry lock");
        if let Some(feed) = map.get_mut(&session) {
            // Bound the replay buffer; live subscribers still get every event.
            if feed.buffer.len() < 1000 {
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

/// What one finished harness turn yielded.
struct TurnOutcome {
    /// The result text (the harness's final message, or the failure tail).
    result: String,
    /// The harness's session id, when the output carried one.
    harness_session: Option<String>,
    /// Whether the turn failed (non-zero exit, timeout, unparseable output).
    failed: bool,
}

/// Run one harness turn in `workspace`: the launch turn when `resume` is
/// `None`, a steer turn otherwise. Streams `stream-json` line by line,
/// publishing progress to the session's live feed as it arrives; returns the
/// final outcome. Callers run this inside a spawned task.
///
/// The prompt travels over **stdin**, never argv: prompts are multi-line,
/// and Windows refuses newline-bearing arguments to `.cmd` shims (which is
/// what an npm-installed `claude` is).
async fn run_turn(
    workspace: &Path,
    prompt: &str,
    resume: Option<&str>,
    live: &LiveSessions,
    session: EntryId,
) -> TurnOutcome {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    let mut command = tokio::process::Command::new(harness_program());
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
        .current_dir(workspace)
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
                result: format!("failed to spawn harness '{}': {err}", harness_program()),
                harness_session: None,
                failed: true,
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
            failed: true,
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

    let status = match tokio::time::timeout(TURN_TIMEOUT, drive).await {
        Ok(status) => status,
        Err(_) => {
            return TurnOutcome {
                result: format!(
                    "turn exceeded the {}-minute timeout and was killed (docs/adr/0023)",
                    TURN_TIMEOUT.as_secs() / 60
                ),
                harness_session,
                failed: true,
            };
        }
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
        failed: is_error || !exit_ok,
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
    let status = run(&["status", "--porcelain"])?;
    if status.trim().is_empty() {
        return None;
    }
    let diff = run(&["diff", "HEAD"]).unwrap_or_default();
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
) -> Result<EntryId> {
    let session = EntryId::new();
    append(
        &host,
        &channel_ref,
        LedgerEntry {
            id: session,
            channel,
            author: harness_member(),
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
    spawn_turn(host, channel, channel_ref, workspace, session, prompt, None);
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
    append(
        &host,
        &channel_ref,
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
    .await?;
    spawn_turn(
        host,
        channel,
        channel_ref,
        workspace,
        session,
        message,
        Some(harness_session),
    );
    Ok(())
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
) {
    tokio::spawn(async move {
        host.live().begin(session);
        let outcome = run_turn(&workspace, &prompt, resume.as_deref(), host.live(), session).await;
        if let Err(err) =
            record_outcome(&host, &channel_ref, channel, session, &workspace, &outcome).await
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
) -> Result<()> {
    let junto_home = crate::host::junto_home()?;
    let turn = record_turn(&junto_home, &session, outcome.harness_session.clone())?;

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
            author: harness_member(),
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
                author: harness_member(),
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

    let (state, note) = if outcome.failed {
        (
            SessionState::Error,
            format!("turn {turn} failed: {}", snippet(&outcome.result, 160)),
        )
    } else {
        (
            SessionState::Done,
            format!("turn {turn} complete: {}", snippet(&outcome.result, 160)),
        )
    };
    append(
        host,
        channel_ref,
        LedgerEntry {
            id: EntryId::new(),
            channel,
            author: harness_member(),
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

        live.begin(session);
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
    fn harness_session_mapping_round_trips() {
        let home = HomeGuard::new();
        let session = EntryId::new();
        assert!(
            harness_session_for(home.path(), &session)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            record_turn(home.path(), &session, Some("h-123".into())).unwrap(),
            1
        );
        assert_eq!(
            harness_session_for(home.path(), &session)
                .unwrap()
                .as_deref(),
            Some("h-123")
        );
        // A later turn increments and may refresh the harness id.
        assert_eq!(record_turn(home.path(), &session, None).unwrap(), 2);
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
