//! SPIKE — a native junto surface in Iced (`docs/native-ui-toolkit-assessment.md`).
//!
//! A tmux-style **2D pane workspace** (`iced::widget::pane_grid`): each pane is a
//! junto channel, rendered from the host's structured JSON read-API
//! (`/channels/{name}/view.json`) into native widgets — a **lineage strip** (the
//! split/side-quest history), the party, and the **entry timeline** as
//! colour-coded cards. Open a channel from the left blade, then split → / split ↓
//! to divide the workspace on either axis, nestable to any depth; drag dividers
//! to resize, drag a pane's title bar to reorder. The point is to feel whether
//! native (Iced) beats the webview as the desktop power-surface.

mod pointing;
mod popover;
mod shell;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use iced::futures::channel::mpsc;
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::widget::pane_grid;
use iced::widget::{
    Space, button, checkbox, column, combo_box, container, markdown, mouse_area, pick_list, row,
    scrollable, text, text_input, tooltip,
};
use iced::{
    Background, Border, Center, Color, Element, Fill, Length, Padding, Point, Rectangle, Renderer,
    Size, Task, Theme, mouse,
};
use junto_kernel::{
    Anchor, Annotation, AnnotationId, CodeAnchor, CommitOid, ContentDigest, EntryId, Member,
    RecordAnchor, SigningKey, Span, StreamAnchor, Timestamp,
};
use junto_live::{Frame as WireFrame, LiveDoc, Presence};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use pointing::{diff_row_targets, drag_lines, popup_anchor_line, watch_identity, watcher_initials};
use popover::Popover;

const HOST: &str = "http://127.0.0.1:1727";

// The web surface's exact palette (Catppuccin Mocha — same hex as render.rs).
const SURFACE: Color = Color::from_rgb(0.118, 0.118, 0.180); // --card #1e1e2e
const BORDER: Color = Color::from_rgb(0.192, 0.196, 0.267); // --border #313244
const TEXT: Color = Color::from_rgb(0.804, 0.839, 0.957); // --text #cdd6f4
const MUTED: Color = Color::from_rgb(0.498, 0.518, 0.612); // --muted #7f849c
const TEAL: Color = Color::from_rgb(0.580, 0.886, 0.835); // --teal #94e2d5
const GREEN: Color = Color::from_rgb(0.651, 0.890, 0.631); // --green #a6e3a1
const RED: Color = Color::from_rgb(0.953, 0.545, 0.659); // --red #f38ba8
const YELLOW: Color = Color::from_rgb(0.976, 0.886, 0.686); // --yellow #f9e2af
const MAUVE: Color = Color::from_rgb(0.796, 0.651, 0.969); // mauve #cba6f7
const BLUE: Color = Color::from_rgb(0.537, 0.706, 0.980); // --accent #89b4fa

fn main() -> iced::Result {
    let icon = iced::window::icon::from_file_data(include_bytes!("../icon.png"), None).ok();
    // 0.14 takes the boot function FIRST and sets the title separately; the
    // old `application(title, ..).run_with(boot)` shape is gone.
    iced::application(App::new, App::update, App::view)
        .title("junto — native spike")
        .subscription(App::subscription)
        .theme(|_: &App| Theme::CatppuccinMocha)
        // The web uses `Inter, system-ui, sans-serif`; on Windows system-ui is
        // Segoe UI (used by name at runtime — not bundled, so no redistribution).
        // Cross-platform parity later = bundle Inter (OFL, MIT-compatible).
        .default_font(iced::Font::with_name("Segoe UI"))
        .window(iced::window::Settings {
            icon,
            // Tall by default so more of a channel's bottom content is visible.
            size: Size::new(1400.0, 1040.0),
            ..Default::default()
        })
        .run()
}

struct App {
    /// The pane workspace: rendered directly by `pane_grid::PaneGrid` in
    /// `view`, which is what buys arbitrary 2D nesting — split any pane on
    /// either axis, at any depth.
    panes: pane_grid::State<Pane>,
    focus: Option<pane_grid::Pane>,
    /// Three-pane shell layout — collapse, widths, active blade views.
    /// Loaded at startup and written back on every change (`shell::save`).
    shell: shell::ShellState,
    /// Available channel names for the type-ahead picker.
    channels: combo_box::State<String>,
    /// The same names as a plain list, for widgets that need to OFFER them
    /// rather than type-ahead them — the converge target picker. Kept beside
    /// `channels` because `combo_box::State` consumes its options and two
    /// `combo_box`es sharing one `State` would share its filter text too.
    channel_names: Vec<String>,
    /// The whole lineage DAG, drawn as the always-visible top branch graph.
    lineage: Option<LineageGraphDto>,
    /// Cross-channel "needs you" items — the focus board.
    focus_items: Vec<FocusItem>,
    /// Configured Agents the launch picker offers (`/agents.json`).
    agents: Vec<AgentDto>,
    /// Distinct workspace repos, most-recent first — the inferred launch default.
    recent_workspaces: Vec<String>,
    /// The name typed into the "new channel" box.
    new_channel: String,
    /// The last create-channel error, if any.
    new_channel_error: Option<String>,
    /// Registered home substrates; a new channel opens in the chosen one.
    substrates: Vec<String>,
    /// The substrate selected for the next new channel.
    new_channel_repo: Option<String>,
    /// Which admin view is open (settings / agents), if any — replaces the
    /// channel workspace when set.
    admin: Option<AdminView>,
    /// Machine settings for the settings view (`/settings.json`).
    settings: Option<SettingsDto>,
    /// The repo-setup form (register a home substrate, the GUI `junto init`).
    repo_path: String,
    repo_channel: String,
    repo_msg: Option<Result<String, String>>,
    /// The agent create/edit form. `agent_slug` Some = editing, None = creating.
    agent_slug: Option<String>,
    agent_name: String,
    agent_harness: Option<HarnessRef>,
    agent_role: String,
    agent_model: String,
    agent_msg: Option<String>,
    /// Advanced agent config rows: MCP servers (name, url), skills, plugin paths.
    agent_mcp: Vec<(String, String)>,
    agent_skills: Vec<String>,
    agent_plugins: Vec<String>,
    /// The join box (Settings → this device): a pasted invite that mints
    /// this machine's key pair via `POST /devices/enroll`. The GUI never
    /// mints a key itself — `join_result` only ever holds what the host
    /// handed back.
    join_invite: String,
    join_pending: bool,
    join_error: Option<String>,
    join_result: Option<EnrolledDto>,
    /// This device's signing-key fingerprint for the current git identity,
    /// if one is on file — computed once, on `SettingsLoaded`/`JoinDone`
    /// (never from the view: `load_signing_key` is a blocking file read).
    device_key_fingerprint: Option<String>,
}

/// The admin views behind the top-bar buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdminView {
    Settings,
    Agents,
}

struct Pane {
    channel: String,
    content: Content,
    /// The session currently being viewed in this pane, if any.
    watched: Option<String>,
    /// Whether an SSE subscription is actively streaming the watched session's
    /// turn. Distinct from `watched`: a landed session stays selected (so its
    /// record + steer box show) but is not streaming.
    streaming: bool,
    /// Accumulated live events (with parsed Markdown) for the current turn.
    feed: Vec<FeedItem>,
    /// A base URL override for this pane's REST calls and live subscription
    /// (`Pane::base`); `None` uses the local `HOST`. Set from the pane's
    /// remote-watch text input — this is how a pane watches a session on
    /// ANOTHER machine's host.
    remote: Option<String>,
    /// The member email this pane authenticates the live websocket as (the
    /// signing key comes from this machine's `keys.toml`, `load_signing_key`).
    watch_email: String,
    /// Who else is watching the currently-streamed remote session, from the
    /// live websocket's presence — empty when not remote-watching.
    watchers: Vec<String>,
    /// The write half of the live websocket, wired up once the connection
    /// authenticates — the annotation composer sends signed `Frame`s
    /// through it (Task 10).
    annotate_tx: Option<mpsc::Sender<WireFrame>>,
    /// The email the live websocket actually authenticated as, captured
    /// once at `Message::LiveConnected` time. The composer signs and
    /// authors with THIS, never a live re-read of `watch_email`: the
    /// subscription id deliberately excludes `watch_email` (`remote_row`'s
    /// "applies on next watch" caption), so a mid-watch edit to that field
    /// would otherwise sign as an identity the socket was never
    /// authenticated as, which the host's `validate_annotation_update`
    /// rejects outright.
    annotate_email: Option<String>,
    /// The live doc's current `conversation` container length, mirrored
    /// from the stream task (`Message::ConversationLen`). A `StreamAnchor`
    /// with no block picked falls back to `conversation_len - 1` — the most
    /// recent event's own CONTAINER index, which is not necessarily
    /// `feed.len() - 1` (the feed also carries synthetic, non-document
    /// error lines). An empty container with nothing picked has no index to
    /// anchor to and is refused outright rather than saturating to a
    /// nonexistent index 0 — see the composer's `AnnotateSubmit` handler.
    conversation_len: usize,
    /// The `conversation` container index the reviewer POINTED AT by
    /// clicking a feed block's gutter (`Message::AnchorStream`), or `None`
    /// to mean "the newest event" — the pre-pointing behaviour, kept as the
    /// default so an un-aimed comment still lands somewhere real.
    /// Mutually exclusive with `annotate_path`: pointing at a line clears
    /// this, and pointing at a block clears the path.
    annotate_op: Option<usize>,
    /// The record content the composer is aimed at: `(entry id, content
    /// digest)`, set by clicking a line of any artifact that is not a diff row
    /// with a known commit.
    ///
    /// This is the third anchor kind (`junto_kernel::RecordAnchor`, Dan's call
    /// 2026-08-23): content already IN the record — a memo, a log, a snapshot,
    /// an uncommitted diff's text — which is immutable and digest-addressable,
    /// so it needs none of the re-anchoring machinery a file span does.
    /// Mutually exclusive with both `annotate_path` and `annotate_op`.
    annotate_record: Option<(String, String)>,
    /// The row a press-drag started on, in the units of whatever is aimed
    /// (`Some` only between press and release). A drag extends the selection to
    /// each row entered while this is set; releasing, or leaving the artifact,
    /// clears it.
    drag_from: Option<u32>,
    /// The row under the cursor, keyed by its target so two artifacts sharing a
    /// line number cannot both light up. Tracked by hand because these rows are
    /// `mouse_area`s, not `button`s: a button reports no drag state, which is
    /// why the first version of this gesture had to be directional clicking.
    hover: Option<(String, u32)>,
    /// The most recent commit oid seen in a `{"kind":"diff","commit":…}`
    /// worktree event on this pane's live doc (`Message::WorktreeDiff`) —
    /// the ONLY source a `CodeAnchor`'s commit may come from. `None` means
    /// the composer must emit a `StreamAnchor`, never a fabricated oid.
    worktree_commit: Option<String>,
    /// The annotation composer's typed file path; empty comments on the
    /// live stream itself instead of a code span.
    annotate_path: String,
    /// The annotation composer's typed line range (`"12"` or `"12-14"`,
    /// `parse_span`).
    annotate_lines: String,
    /// The annotation composer's comment body — cleared after a successful
    /// send, but `annotate_path`/`annotate_lines` persist (a reviewer
    /// usually comments repeatedly on the same region).
    annotate_body: String,
    /// The annotation composer's "urgent" checkbox.
    annotate_urgent: bool,
    launch_intent: String,
    steer_text: String,
    /// Which configured Agent runs the next launch (None → host default).
    /// Moot after the channel's agent is established (one per channel, adr/0024).
    launch_agent: Option<AgentDto>,
    /// When true, launch runs the code-PR verify/Grader push-gate loop
    /// (`mode=outcome`); otherwise a single turn (`mode=single`).
    launch_outcome: bool,
    /// The workspace repo for the launch; empty falls back to the channel's
    /// remembered mapping on the host.
    launch_workspace: String,
    /// A launch is in flight — disables the launch button and shows "launching…".
    launching: bool,
    /// The last launch's error message, if it failed (e.g. no workspace).
    launch_error: Option<String>,
    /// An entry to surface at the top of the timeline — set when a focus-board
    /// chip jumps here so the card needing attention is immediately visible.
    highlight_entry: Option<String>,
    /// Per-entry rationale drafts for inline verification acts, keyed by entry id.
    act_drafts: HashMap<String, String>,
    /// Per-entry error messages from a failed act, keyed by entry id.
    act_errors: HashMap<String, String>,
    /// Entry ids with a verification act in flight — drives the "recording…"
    /// feedback and disables the buttons until the host responds.
    act_pending: HashSet<String>,
    /// The timeline scrollable's id, so we can snap it to the newest entry.
    scroll_id: iced::widget::Id,
    /// Expanded artifacts' inline content, keyed by artifact entry id. Absent =
    /// collapsed; present = expanded (loading / loaded / error).
    artifacts: HashMap<String, ArtifactContent>,
    /// Whether the launch form's options (agent / workspace / mode) are shown.
    /// Collapsed by default to keep the launch bar to a single line.
    launch_expanded: bool,
    /// Whether the full entry history is shown; false = a brief of recent
    /// entries with a "show full history" toggle.
    show_full_history: bool,
    /// Parsed Markdown for session memo entries, keyed by entry id (parsed on
    /// load so memo notes render formatted, not as plain text).
    entry_md: HashMap<String, Vec<markdown::Item>>,
    /// After the next load, auto-select and stream the newest session — set on
    /// launch so you immediately watch the agent work (Claude-Code-like).
    watch_newest: bool,
    /// Bumped each time streaming (re)starts, so the SSE subscription id changes
    /// and Iced actually restarts it for a new turn (a finished subscription
    /// with an unchanged id is never restarted — the Iced footgun).
    stream_nonce: u64,
    /// The channel lifecycle form currently open in this pane, if any.
    lifecycle: Option<LifecycleKind>,
    /// The lifecycle form's text input (a rationale or a side-quest name).
    lifecycle_text: String,
    /// The converge target channel name.
    lifecycle_target: String,
    /// A lifecycle act is in flight.
    lifecycle_pending: bool,
    /// The last lifecycle act's error, if it failed.
    lifecycle_error: Option<String>,
    /// The channel's curated brief (recall bridge) as parsed Markdown — shown at
    /// the top of the pane; the full entry history is a click away.
    brief_md: Option<Vec<markdown::Item>>,
    /// The humanized brief text, kept so it can be copied to the clipboard.
    brief_text: Option<String>,
    /// The channel's key roster (`keys.json`) — the members-and-devices
    /// disclosure's data (device-key-enrollment plan, Task 13). `None`
    /// until the first fetch lands.
    keys: Option<KeysDto>,
    /// The last `keys.json` fetch's error, if it failed — rendered by the
    /// disclosure instead of silently reading as "no members" (a fetch
    /// failure and a genuinely empty roster must never look the same).
    keys_error: Option<String>,
    /// Whether this pane has already auto-expanded a session's newest diff.
    /// Set once so a manual collapse is not undone by the next refetch.
    auto_expanded: bool,
    /// Whether the members disclosure is expanded.
    members_open: bool,
    /// The founder identity act currently open in this pane, if any — the
    /// `IdentityForm` analogue of `lifecycle`.
    identity_form: Option<IdentityForm>,
    /// An identity act is in flight.
    identity_pending: bool,
    /// The last identity act's error, if it failed.
    identity_error: Option<String>,
    /// A freshly minted invite, shown with its countdown until dismissed
    /// or replaced by a fresh mint.
    invite_minted: Option<InviteMintedDto>,
    /// A redeem form's pre-append preview (`POST /devices/preview`),
    /// shown before the kind picker and the confirm that actually appends.
    redeem_preview: Option<EnrollPreviewDto>,
    /// A redeem form's per-channel outcomes, persisting until the form is
    /// cancelled.
    redeem_outcomes: Vec<RedeemOutcomeDto>,
    /// The invite form's member-email input.
    identity_member: String,
    /// The invite form's channel checkboxes: (channel name, ticked).
    identity_channels: Vec<(String, bool)>,
    /// The redeem form's pasted enroll code.
    identity_paste: String,
    /// The redeem form's picked member kind — `""` until the founder
    /// picks one; never defaulted (`docs/adr/0035`).
    identity_kind: String,
    /// The retire/revoke forms' rationale input.
    identity_rationale: String,
    /// A retire/revoke act's success confirmation ("parked N grant(s)"),
    /// shown until dismissed — the form itself has already closed by
    /// then, so this is the only feedback the founder gets that it
    /// actually happened.
    identity_notice: Option<String>,
}

enum Content {
    Loading,
    Loaded(ChannelDto),
    Error(String),
}

#[derive(Debug, Clone, Deserialize)]
struct LineageGraphDto {
    nodes: Vec<GNode>,
    edges: Vec<GEdge>,
}

#[derive(Debug, Clone, Deserialize)]
struct GNode {
    id: String,
    name: String,
    first_ms: Option<i64>,
    last_ms: Option<i64>,
    #[serde(default)]
    milestones: Vec<MilestoneDto>,
}

#[derive(Debug, Clone, Deserialize)]
struct MilestoneDto {
    ms: i64,
    label: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GEdge {
    from: String,
    to: String,
    relation: String,
}

/// A configured Agent the launch picker offers (mirrors `/agents.json`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct AgentDto {
    slug: String,
    name: String,
    harness: String,
    #[serde(default)]
    model: Option<String>,
    /// The agent's role / system prompt, if any (for the management form).
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    mcp_servers: Vec<McpServerDto>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    plugins: Vec<String>,
}

/// One MCP server an agent offers (name + streamable-HTTP url).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct McpServerDto {
    name: String,
    url: String,
}

/// Machine settings (`/settings.json`) for the native settings view.
#[derive(Debug, Clone, Deserialize)]
struct SettingsDto {
    harness: HarnessStatusDto,
    harnesses: Vec<HarnessRef>,
    substrates: Vec<String>,
    identity: Option<IdentityDto>,
    version: String,
}

#[derive(Debug, Clone, Deserialize)]
struct HarnessStatusDto {
    protocol: String,
    detail: String,
    backend: String,
    auth: String,
    hint: Option<String>,
}

/// A harness id+label — also the agent form's harness-picker option.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct HarnessRef {
    id: String,
    label: String,
}

impl std::fmt::Display for HarnessRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct IdentityDto {
    name: String,
    email: String,
}

/// `POST /devices/enroll`'s response — mirrors
/// `crates/junto/src/web.rs::EnrolledDto` exactly. A device enrolling under
/// the current payload version always mints both keypairs in the same
/// step, so neither fingerprint is ever optional here (contrast
/// `keys.json`'s per-grant transport fingerprint, which IS optional for a
/// grant made before transport keys existed — that DTO belongs to the
/// members-disclosure task).
#[derive(Debug, Clone, Deserialize)]
struct EnrolledDto {
    url: String,
    email: String,
    fingerprint: String,
    transport_fingerprint: String,
}

impl std::fmt::Display for AgentDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // "name · harness" (with model when overridden) — what the dropdown shows.
        match &self.model {
            Some(model) => write!(f, "{} · {} ({model})", self.name, self.harness),
            None => write!(f, "{} · {}", self.name, self.harness),
        }
    }
}

/// One frame of a session's live feed (mirrors the host's `LiveEvent`).
#[derive(Debug, Clone, Deserialize)]
struct LiveEvent {
    kind: String,
    text: String,
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    html: bool,
    /// Raw Markdown behind a rendered segment, when the host supplies it.
    #[serde(default)]
    markdown: Option<String>,
}

/// A live-feed entry plus its parsed Markdown (parsed once on arrival so the
/// `markdown` widget can render it without re-parsing each frame).
struct FeedItem {
    event: LiveEvent,
    md: Option<Vec<markdown::Item>>,
    /// This item's own index in the live doc's `conversation` container — the
    /// `op_id` a `StreamAnchor` on this block needs. `None` for a synthetic
    /// line the app made up locally (`error_event`) or an SSE-streamed local
    /// session, neither of which exists in the document, so neither can be
    /// pointed at.
    op: Option<usize>,
}

/// The raw Markdown to render for an event, if any: the host's `markdown`
/// field, or model prose that arrived as plain text (assistant/thinking/result).
fn feed_markdown(event: &LiveEvent) -> Option<Vec<markdown::Item>> {
    let raw = event.markdown.as_deref().or_else(|| {
        (!event.html && matches!(event.kind.as_str(), "assistant" | "thinking" | "result"))
            .then_some(event.text.as_str())
    })?;
    if raw.trim().is_empty() {
        return None;
    }
    Some(markdown::parse(raw).collect())
}

// --- the host's view.json shape ---

#[derive(Debug, Clone, Deserialize)]
struct ChannelDto {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    name: Option<String>,
    #[allow(dead_code)]
    closed: bool,
    /// Superseded by `keys.json`'s roster (Task 13's members disclosure);
    /// kept for DTO fidelity with the host, unread by the surface.
    #[allow(dead_code)]
    party: Vec<String>,
    /// The channel's remembered workspace repo, if any (a returning channel).
    #[serde(default)]
    workspace: Option<String>,
    sessions: Vec<SessionDto>,
    entries: Vec<EntryDto>,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionDto {
    id: String,
    state: String,
    intent: String,
}

/// One cross-channel "needs you" item on the focus board.
#[derive(Debug, Clone, Deserialize)]
struct FocusItem {
    kind: String,
    #[allow(dead_code)]
    entry_id: String,
    #[allow(dead_code)]
    channel: String,
    channel_name: Option<String>,
    /// The proposer's display name (shown on the chip).
    author: String,
    summary: String,
}

#[derive(Debug, Clone, Deserialize)]
struct EntryDto {
    id: String,
    author: String,
    kind: String,
    summary: String,
    status: Option<String>,
    unrecognized: bool,
    /// Recognized but the signature is missing or does not verify against
    /// the author's recorded key (`docs/adr/0033`). Absent from a host that
    /// predates the field.
    #[serde(default)]
    unverified: bool,
    /// The entry this one acts on (e.g. a session memo/artifact → its session).
    #[serde(default)]
    target: Option<String>,
    /// Pre-baked decision-frame options (`docs/adr/0019`); empty when none.
    #[serde(default)]
    frame: Vec<FrameOptionDto>,
}

/// One decision-frame option — a labelled, pre-baked rationale for one act.
#[derive(Debug, Clone, Deserialize)]
struct FrameOptionDto {
    label: String,
    act: String,
    rationale: String,
}

/// A channel lifecycle act the pane can perform (`docs/adr/0022`/`0027`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleKind {
    Close,
    Reopen,
    Diverge,
    Converge,
    Rename,
}

impl LifecycleKind {
    fn label(self) -> &'static str {
        match self {
            LifecycleKind::Close => "close channel",
            LifecycleKind::Reopen => "reopen channel",
            LifecycleKind::Diverge => "diverge (side-quest)",
            LifecycleKind::Converge => "converge into…",
            LifecycleKind::Rename => "rename",
        }
    }
}

/// What a finished lifecycle act asks the pane to do next.
#[derive(Debug, Clone)]
enum LifecycleResult {
    /// Just refresh this pane (close / reopen / converge).
    Done,
    /// Open the named new side-quest in its own pane (diverge).
    OpenChild(String),
    /// This channel was renamed — rebind the pane to the new name.
    Renamed(String),
}

/// The fetch state of an artifact's inline content (`/artifacts/{id}/content.json`).
enum ArtifactContent {
    Loading,
    Loaded {
        format: String,
        body: String,
        /// Parsed Markdown, for memo-format artifacts (parsed once on load).
        md: Option<Vec<markdown::Item>>,
        /// The digest of `body`, computed once on arrival rather than per frame,
        /// and by the same formula the host used when it stored these bytes
        /// (`ContentDigest::sha256_of`) so a `RecordAnchor` built here matches
        /// the artifact's recorded provenance.
        digest: String,
    },
    Error(String),
}

/// The host's artifact content payload.
#[derive(Debug, Clone, Deserialize)]
struct ArtifactDto {
    format: String,
    content: String,
}

/// `keys.json`'s response — mirrors `crates/junto/src/web.rs::KeysDto`
/// field-for-field (device-key-enrollment plan, Task 13).
#[derive(Debug, Clone, Deserialize)]
struct KeysDto {
    founder_email: String,
    /// The git identity this host writes as. `None` when `git_user` fails
    /// (no git config) — the endpoint still answers 200 (reading a roster
    /// needs no identity of its own). Kept for DTO fidelity with the host
    /// even though the surface only ever reads `viewer_is_founder`.
    #[allow(dead_code)]
    viewer_email: Option<String>,
    viewer_is_founder: bool,
    /// `view.party` order — founder first; never sorted here, the host's
    /// replicas already agree on it.
    members: Vec<KeyMemberDto>,
}

/// One party member, mirrors `KeyMemberDto`.
#[derive(Debug, Clone, Deserialize)]
struct KeyMemberDto {
    display_name: String,
    email: String,
    /// "human" | "agent".
    kind: String,
    /// Canonical grant order — never sorted here either.
    devices: Vec<KeyGrantDto>,
    /// Every grant retired (`docs/adr/0035`'s all-retired rule). Never
    /// implies the member left the party — they stay listed.
    revoked: bool,
}

/// One key grant, mirrors `KeyGrantDto`.
#[derive(Debug, Clone, Deserialize, Default)]
struct KeyGrantDto {
    /// 16 hex chars of the signing key.
    fingerprint: String,
    /// 16 hex chars of the transport key — `None` for a grant made before
    /// transport keys existed. Kept for DTO fidelity; the disclosure's
    /// device line shows the signing fingerprint only.
    #[allow(dead_code)]
    transport_fingerprint: Option<String>,
    /// The entry id that authorized this key.
    granted_by: String,
    /// Epoch millis, when retired.
    retired_at: Option<i64>,
}

/// `POST /invites`'s response, mirrors `InviteMintedDto`.
#[derive(Debug, Clone, Deserialize)]
struct InviteMintedDto {
    url: String,
    expires_at: i64,
    channels: Vec<String>,
}

/// `POST /devices/preview`'s response, mirrors `EnrollPreviewDto` — what
/// an enroll code would grant if redeemed right now. Appends nothing.
#[derive(Debug, Clone, Deserialize)]
struct EnrollPreviewDto {
    email: String,
    display_name: String,
    fingerprint: String,
    transport_fingerprint: String,
    channels: Vec<PreviewChannelDto>,
}

/// One channel an enroll code covers, mirrors `PreviewChannelDto`.
#[derive(Debug, Clone, Deserialize)]
struct PreviewChannelDto {
    id: String,
    name: Option<String>,
}

/// `POST /members`'s response, mirrors `RedeemedDto`.
#[derive(Debug, Clone, Deserialize)]
struct RedeemedDto {
    outcomes: Vec<RedeemOutcomeDto>,
}

/// One channel's redeem outcome, mirrors `RedeemOutcomeDto`. `result` is
/// snake_case: `granted` / `already_a_member` / `invite_already_used` /
/// `not_founder` / `failed`. `warning` carries the ADR 0035 re-admission
/// warning on a `granted` outcome (`None` otherwise) — the HTTP path's
/// only way to see it, since there is no host log to read a CLI
/// `println!` from.
#[derive(Debug, Clone, Deserialize)]
struct RedeemOutcomeDto {
    channel: String,
    channel_name: Option<String>,
    result: String,
    detail: Option<String>,
    warning: Option<String>,
}

/// A founder identity act the members disclosure can perform
/// (device-key-enrollment plan, Task 13) — the `IdentityForm` analogue of
/// `LifecycleKind`. `Retire`/`Revoke` carry their target directly (the
/// grant's authorizing entry id, the member's email) instead of a
/// separate pane field, since each is opened right from that row.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IdentityForm {
    Invite,
    Redeem,
    Retire { grant: String },
    Revoke { email: String },
}

impl IdentityForm {
    fn label(&self) -> &'static str {
        match self {
            IdentityForm::Invite => "invite a device",
            IdentityForm::Redeem => "redeem an enrollment",
            IdentityForm::Retire { .. } => "retire",
            IdentityForm::Revoke { .. } => "revoke",
        }
    }
}

/// Which identity-form text input `Message::IdentityInput` edits — one
/// multiplexed editor for every open `IdentityForm`'s free-text fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityField {
    Member,
    Paste,
    Kind,
    Rationale,
}

/// What a finished identity act asks the pane to do next — the
/// `IdentityResult` analogue of `LifecycleResult`.
#[derive(Debug, Clone)]
enum IdentityResult {
    /// A preview of what an enroll code would grant (`/devices/preview`)
    /// — appends nothing; the redeem form shows this before the kind
    /// picker.
    Previewed(EnrollPreviewDto),
    Minted(InviteMintedDto),
    Redeemed(RedeemedDto),
    /// Retire/revoke: the number of grants parked.
    Parked(usize),
}

/// What a rendered row points at — the two anchor kinds a row can make.
///
/// Carried by the pointing messages so one row widget serves both, and so the
/// `update` handler never has to guess which claim a click meant from the state
/// it happens to find lying around.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AnchorTarget {
    /// A line of a file at the session's commit (`CodeAnchor`): the new-file
    /// path. The stronger claim, available only where a diff row occupies a real
    /// new-file line.
    Code(String),
    /// A line of content already in the record (`RecordAnchor`): the entry that
    /// attached it, and that content's digest.
    Record(String, String),
}

impl AnchorTarget {
    /// The identity a hover or a drag is scoped to, so a drag can never jump
    /// between two files (or two artifacts) mid-gesture and produce a span whose
    /// halves came from different content.
    fn key(&self) -> &str {
        match self {
            Self::Code(path) => path,
            Self::Record(entry, _) => entry,
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    ChannelsLoaded(Vec<String>),
    ChannelPicked(String),
    /// A focus-board chip: open/focus the channel and jump to the entry.
    FocusChipPicked(String, String),
    /// Dismiss the pinned attention card in a pane.
    ClearHighlight(pane_grid::Pane),
    /// Collapse or expand the left blade.
    ToggleLeftBlade,
    /// Collapse or expand the right blade.
    ToggleRightBlade,
    /// Switch the left blade's view beneath the pinned nav.
    LeftViewPicked(shell::LeftView),
    /// Switch the right blade's view.
    RightViewPicked(shell::RightView),
    /// Edit the rationale draft for an inline verification act (pane, entry, text).
    ActRationaleChanged(pane_grid::Pane, String, String),
    /// Submit a verification act on an entry (pane, entry, act route, rationale).
    /// Frame options pass their pre-baked rationale; the free-text form passes
    /// the typed draft.
    Act(pane_grid::Pane, String, String, String),
    /// The result of a verification act (pane, entry, Ok or an error message).
    Acted(pane_grid::Pane, String, Result<(), String>),
    /// Expand/collapse an artifact's inline content (pane, artifact entry id).
    ToggleArtifact(pane_grid::Pane, String),
    /// An artifact's content arrived (pane, artifact id, Ok or error).
    ArtifactLoaded(pane_grid::Pane, String, Result<ArtifactDto, String>),
    LineageGraphLoaded(Option<LineageGraphDto>),
    FocusLoaded(Vec<FocusItem>),
    AgentsLoaded(Vec<AgentDto>),
    WorkspacesLoaded(Vec<String>),
    Fetched(pane_grid::Pane, Result<ChannelDto, String>),
    Refresh(pane_grid::Pane),
    Close(pane_grid::Pane),
    /// A divider drag between panes.
    PaneResized(pane_grid::ResizeEvent),
    /// A pane dragged to a new position.
    PaneDragged(pane_grid::DragEvent),
    /// A pane was clicked — retargets the blades to it.
    PaneClicked(pane_grid::Pane),
    /// Split the focused pane along `axis`.
    SplitPane(pane_grid::Axis),
    // Live session pane.
    Watch(pane_grid::Pane, String),
    /// Close the session view, returning the pane to its timeline.
    CloseSession(pane_grid::Pane),
    /// A live event to append to a pane's feed (session, the event's own
    /// `conversation` container index, the event). The index is `None` for a
    /// line that exists only locally — a synthetic `error_event`, or anything
    /// from the local SSE stream, which is not a live document — and those
    /// lines are therefore not pointable (`FeedItem::op`).
    Live(String, Option<usize>, LiveEvent),
    LiveEnded(String),
    /// The pane's remote-watch base URL text input changed (empty → local).
    RemoteChanged(pane_grid::Pane, String),
    /// The pane's "watch as (email)" text input changed.
    WatchEmailChanged(pane_grid::Pane, String),
    /// A live websocket's presence updated (session, sorted watcher emails).
    Watchers(String, Vec<String>),
    /// A live websocket authenticated and is ready to carry outbound frames
    /// (session, the write-half sender, the email it actually
    /// authenticated as) — stored as `Pane::annotate_tx`/`annotate_email`
    /// for the annotation composer.
    LiveConnected(String, mpsc::Sender<WireFrame>, String),
    /// The live doc's `conversation` container grew (session, new length) —
    /// mirrored into `Pane::conversation_len` so the composer's
    /// `StreamAnchor` can point at a real CONTAINER index.
    ConversationLen(String, usize),
    /// A worktree `{"kind":"diff","commit":…}` event arrived (session,
    /// commit oid) — mirrored into `Pane::worktree_commit`, the only source
    /// the composer's `CodeAnchor` may ever take a commit from.
    WorktreeDiff(String, String),
    // --- pointing: press-drag-release across rendered rows to aim the composer ---
    /// The mouse went down on a rendered row (pane, what that row points at,
    /// the row's line in that target's own units) — aims the composer at a
    /// single line and begins a drag.
    AnchorPress(pane_grid::Pane, AnchorTarget, u32),
    /// The cursor entered a rendered row. Always updates the hover paint; while
    /// a drag is in progress on the SAME target it also extends the selection
    /// (`pointing::drag_lines`).
    AnchorOver(pane_grid::Pane, AnchorTarget, u32),
    /// The mouse came up, or left the artifact entirely — ends any drag.
    AnchorRelease(pane_grid::Pane),
    /// A rendered feed block's gutter was clicked (pane, the block's own
    /// `conversation` container index) — aims the `StreamAnchor` there
    /// instead of at the newest event.
    AnchorStream(pane_grid::Pane, usize),
    /// Drop the aimed anchor, returning the composer to commenting on the
    /// newest live event.
    AnchorClear(pane_grid::Pane),
    /// The annotation composer's `path` text input changed.
    AnnotatePathChanged(pane_grid::Pane, String),
    /// The annotation composer's `lines` text input changed.
    AnnotateLinesChanged(pane_grid::Pane, String),
    /// The annotation composer's comment body text input changed.
    AnnotateBodyChanged(pane_grid::Pane, String),
    /// The annotation composer's "urgent" checkbox toggled.
    AnnotateUrgentToggled(pane_grid::Pane, bool),
    /// Submit the annotation composer: sign and send the comment.
    AnnotateSubmit(pane_grid::Pane),
    LaunchIntentChanged(pane_grid::Pane, String),
    LaunchAgentPicked(pane_grid::Pane, AgentDto),
    LaunchModeChanged(pane_grid::Pane, bool),
    LaunchWorkspaceChanged(pane_grid::Pane, String),
    /// Open the native folder picker for the launch workspace.
    BrowseWorkspace(pane_grid::Pane),
    /// The folder the picker returned (None = cancelled).
    WorkspacePicked(pane_grid::Pane, Option<String>),
    Launch(pane_grid::Pane),
    /// The result of a launch (pane, Ok or an error message).
    Launched(pane_grid::Pane, Result<(), String>),
    SteerTextChanged(pane_grid::Pane, String),
    Steer(pane_grid::Pane),
    /// The result of a steer POST (pane, Ok or error) — re-streams the resumed turn.
    Steered(pane_grid::Pane, Result<(), String>),
    Interrupt(pane_grid::Pane),
    Posted(pane_grid::Pane),
    /// Show/hide the launch options (agent / workspace / mode).
    ToggleLaunchOptions(pane_grid::Pane),
    /// Show all entries vs. the recent brief.
    ToggleHistory(pane_grid::Pane),
    // Channel lifecycle acts.
    /// Open (or toggle off) a lifecycle form in a pane.
    LifecycleSelect(pane_grid::Pane, LifecycleKind),
    /// Cancel the open lifecycle form.
    LifecycleCancel(pane_grid::Pane),
    LifecycleTextChanged(pane_grid::Pane, String),
    LifecycleTargetChanged(pane_grid::Pane, String),
    /// Submit the open lifecycle act.
    LifecycleSubmit(pane_grid::Pane),
    /// A lifecycle act finished — the Ok variant says what to do next.
    LifecycleDone(pane_grid::Pane, Result<LifecycleResult, String>),
    SubstratesLoaded(Vec<String>),
    NewChannelRepoChanged(String),
    /// The "new channel" name box.
    NewChannelChanged(String),
    /// Create a channel from the new-channel box.
    CreateChannel,
    /// A channel was created (Ok = its name to open) or failed.
    ChannelCreated(Result<String, String>),
    /// The curated brief markdown for a pane's channel (None if unavailable).
    BriefLoaded(pane_grid::Pane, Option<String>),
    /// Open a clicked Markdown link in the OS browser.
    OpenUrl(String),
    /// Copy text to the system clipboard.
    Copy(String),
    /// Refresh everything — channels, lineage, focus, agents, settings, and
    /// every open pane (a manual full reload).
    RefreshAll,
    /// Periodic ambient refresh — channels, lineage, focus only (cheap, doesn't
    /// disturb pane scroll/streaming).
    AutoRefresh,
    // --- admin: settings, repo setup, agents ---
    /// Open an admin view, or `None` to return to the channels workspace.
    OpenAdmin(Option<AdminView>),
    SettingsLoaded(Option<SettingsDto>),
    /// The join box's pasted-invite text input changed (Settings → this device).
    JoinInviteChanged(String),
    /// Submit the join box: mint this device's key pair on the host from
    /// the pasted invite (`POST /devices/enroll`).
    JoinSubmit,
    /// The result of a join POST — Ok carries the enrolled identity
    /// (enroll code + both fingerprints) to show; the GUI never mints
    /// anything itself.
    JoinDone(Result<EnrolledDto, String>),
    RepoPathChanged(String),
    BrowseRepo,
    RepoPicked(Option<String>),
    RepoChannelChanged(String),
    SetupRepo,
    RepoSetupDone(Result<(), String>),
    AgentEdit(AgentDto),
    AgentNew,
    AgentNameChanged(String),
    AgentHarnessPicked(HarnessRef),
    AgentRoleChanged(String),
    AgentModelChanged(String),
    SaveAgent,
    AgentSaved(Result<(), String>),
    DeleteAgent(String),
    AgentDeleted(Result<(), String>),
    // Advanced agent config rows.
    McpNameChanged(usize, String),
    McpUrlChanged(usize, String),
    McpAddRow,
    McpRemove(usize),
    SkillChanged(usize, String),
    SkillAddRow,
    SkillRemove(usize),
    PluginChanged(usize, String),
    PluginAddRow,
    PluginRemove(usize),
    PluginBrowse(usize),
    PluginPicked(usize, Option<String>),
    // --- members & devices disclosure: invite / redeem / retire / revoke ---
    /// `keys.json` arrived for a pane (device-key-enrollment plan, Task 13).
    KeysFetched(pane_grid::Pane, Result<KeysDto, String>),
    /// Expand/collapse the members disclosure.
    MembersToggle(pane_grid::Pane),
    /// Open (or toggle off) an identity act form.
    IdentitySelect(pane_grid::Pane, IdentityForm),
    /// Cancel the open identity form.
    IdentityCancel(pane_grid::Pane),
    IdentityInput(pane_grid::Pane, IdentityField, String),
    /// Toggle one channel checkbox in the invite form (index into
    /// `Pane::identity_channels`).
    IdentityChannelToggle(pane_grid::Pane, usize),
    /// Submit the open identity act — for the redeem form, the first
    /// submit previews (`/devices/preview`); the second, once a kind is
    /// picked, actually redeems.
    IdentitySubmit(pane_grid::Pane),
    /// An identity act finished.
    IdentityDone(pane_grid::Pane, Result<IdentityResult, String>),
    /// A 1-second tick, live only while a minted invite is on screen and
    /// unexpired (`App::subscription`) — drives the countdown.
    Tick,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let (panes, first) = pane_grid::State::new(Pane::loading("junto-dev"));
        let app = App {
            panes,
            focus: Some(first),
            channels: combo_box::State::new(Vec::new()),
            channel_names: Vec::new(),
            lineage: None,
            focus_items: Vec::new(),
            agents: Vec::new(),
            recent_workspaces: Vec::new(),
            new_channel: String::new(),
            new_channel_error: None,
            substrates: Vec::new(),
            new_channel_repo: None,
            admin: None,
            settings: None,
            repo_path: String::new(),
            repo_channel: String::new(),
            repo_msg: None,
            agent_slug: None,
            agent_name: String::new(),
            agent_harness: None,
            agent_role: String::new(),
            agent_model: String::new(),
            agent_msg: None,
            agent_mcp: Vec::new(),
            agent_skills: Vec::new(),
            agent_plugins: Vec::new(),
            join_invite: String::new(),
            join_pending: false,
            join_error: None,
            join_result: None,
            device_key_fingerprint: None,
            shell: shell::load(&shell_state_path()),
        };
        (
            app,
            Task::batch([
                fetch(first, HOST.to_string(), "junto-dev"),
                fetch_channels(),
                fetch_lineage_graph(),
                fetch_focus(),
                fetch_agents(),
                fetch_workspaces(),
                fetch_substrates(),
                fetch_settings(),
            ]),
        )
    }

    /// Focus the existing pane for `name`, or split a new one. Returns the pane
    /// (when resolved) and the fetch task for a freshly-opened pane.
    fn open_or_focus(&mut self, name: &str) -> (Option<pane_grid::Pane>, Task<Message>) {
        if let Some(existing) = self
            .panes
            .iter()
            .find(|(_, state)| state.channel == name)
            .map(|(id, _)| *id)
        {
            self.focus = Some(existing);
            return (Some(existing), Task::none());
        }
        // `focus` is set at startup and only ever reassigned to `Some`, so this
        // fallback is unreachable today; it exists for the type. Unlike the old
        // `order.last()` (the most-recently-opened, rightmost pane), a
        // `pane_grid::State` has no spatial "last" — this falls back to the
        // lowest-id (oldest) pane instead.
        let Some(target) = self
            .focus
            .or_else(|| self.panes.iter().next().map(|(id, _)| *id))
        else {
            return (None, Task::none());
        };
        if let Some((new_pane, _)) =
            self.panes
                .split(pane_grid::Axis::Vertical, target, Pane::loading(name))
        {
            self.focus = Some(new_pane);
            return (Some(new_pane), fetch(new_pane, HOST.to_string(), name));
        }
        (None, Task::none())
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ChannelsLoaded(names) => {
                self.channel_names = names.clone();
                self.channels = combo_box::State::new(names);
                Task::none()
            }
            Message::LineageGraphLoaded(graph) => {
                self.lineage = graph;
                Task::none()
            }
            Message::FocusLoaded(items) => {
                self.focus_items = items;
                Task::none()
            }
            Message::AgentsLoaded(agents) => {
                self.agents = agents;
                Task::none()
            }
            Message::WorkspacesLoaded(workspaces) => {
                self.recent_workspaces = workspaces;
                Task::none()
            }
            Message::ChannelPicked(name) => {
                let (_, task) = self.open_or_focus(&name);
                task
            }
            Message::FocusChipPicked(name, entry_id) => {
                let (pane, task) = self.open_or_focus(&name);
                if let Some(pane) = pane
                    && let Some(state) = self.panes.get_mut(pane)
                {
                    // Show the timeline (not a live feed) so the card is visible,
                    // and pin the attention entry to the top.
                    state.watched = None;
                    state.highlight_entry = Some(entry_id);
                }
                task
            }
            Message::ClearHighlight(pane) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.highlight_entry = None;
                }
                Task::none()
            }
            Message::ToggleLeftBlade => {
                self.shell.toggle_left();
                self.persist_shell();
                Task::none()
            }
            Message::ToggleRightBlade => {
                self.shell.toggle_right();
                self.persist_shell();
                Task::none()
            }
            Message::LeftViewPicked(view) => {
                self.shell.left_view = view;
                self.persist_shell();
                Task::none()
            }
            Message::RightViewPicked(view) => {
                self.shell.right_view = view;
                self.persist_shell();
                Task::none()
            }
            Message::ActRationaleChanged(pane, entry_id, value) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.act_drafts.insert(entry_id, value);
                }
                Task::none()
            }
            Message::Act(pane, entry_id, act, rationale) => {
                let rationale = rationale.trim().to_string();
                if rationale.is_empty() {
                    return Task::none();
                }
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                let base = state.base().to_string();
                let channel = state.channel.clone();
                state.act_errors.remove(&entry_id); // clear any stale error
                state.act_pending.insert(entry_id.clone()); // show "recording…"
                post_verify(pane, base, channel, entry_id, act, rationale)
            }
            Message::Acted(pane, entry_id, result) => match result {
                Ok(()) => {
                    let target = self.panes.get_mut(pane).map(|state| {
                        state.act_pending.remove(&entry_id);
                        state.act_errors.remove(&entry_id);
                        state.act_drafts.remove(&entry_id);
                        // The resolved entry is no longer "needs you" — drop its
                        // pinned attention card.
                        if state.highlight_entry.as_deref() == Some(entry_id.as_str()) {
                            state.highlight_entry = None;
                        }
                        (state.base().to_string(), state.channel.clone())
                    });
                    // Refetch the pane AND the focus board (so the resolved item
                    // leaves the top shelf) and the lineage.
                    target.map_or_else(Task::none, |(base, c)| {
                        Task::batch([fetch(pane, base, &c), fetch_focus(), fetch_lineage_graph()])
                    })
                }
                Err(err) => {
                    if let Some(state) = self.panes.get_mut(pane) {
                        state.act_pending.remove(&entry_id);
                        state.act_errors.insert(entry_id, err);
                    }
                    Task::none()
                }
            },
            Message::Fetched(pane, result) => {
                // The inferred default workspace for a fresh channel.
                let default_workspace = self.recent_workspaces.first().cloned();
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                match result {
                    Ok(dto) => {
                        // Pre-fill the workspace: the channel's remembered repo,
                        // else the most-recently-used one — so the user rarely
                        // has to pick a directory.
                        if state.launch_workspace.trim().is_empty()
                            && let Some(ws) = dto.workspace.clone().or(default_workspace)
                        {
                            state.launch_workspace = ws;
                        }
                        // Pre-parse session memo notes so they render as Markdown.
                        state.entry_md = dto
                            .entries
                            .iter()
                            .filter(|e| e.kind == "session")
                            .map(|e| (e.id.clone(), markdown::parse(&e.summary).collect()))
                            .collect();
                        // After a launch, jump straight into streaming the new
                        // session so the agent's work is immediately visible.
                        if state.watch_newest {
                            state.watch_newest = false;
                            if let Some(newest) = dto.sessions.last() {
                                state.watched = Some(newest.id.clone());
                                state.streaming = true;
                                state.stream_nonce += 1;
                                state.feed.clear();
                                state.watchers.clear();
                                state.annotate_tx = None;
                                state.annotate_email = None;
                                state.conversation_len = 0;
                                state.worktree_commit = None;
                            }
                        }
                        // Put the code on screen without hunting for it: a
                        // watched session's newest diff artifact is expanded
                        // once, automatically (ledger `532826c2`). Once only,
                        // so collapsing it by hand is not undone by the next
                        // refetch.
                        let auto_expand = state
                            .watched
                            .as_deref()
                            .filter(|_| !state.auto_expanded)
                            .and_then(|session| newest_diff_artifact(&dto.entries, session))
                            .map(|entry| entry.id.clone());
                        if auto_expand.is_some() {
                            state.auto_expanded = true;
                        }
                        state.content = Content::Loaded(dto);
                        let base = state.base().to_string();
                        let channel = state.channel.clone();
                        // Jump to the newest entry (bottom), refresh the
                        // brief, and refresh the key roster the members
                        // disclosure reads (device-key-enrollment plan,
                        // Task 13) — every view.json load keeps keys.json
                        // in step.
                        let mut tasks = vec![
                            iced::widget::operation::snap_to_end(state.scroll_id.clone()),
                            fetch_brief(pane, base.clone(), channel.clone()),
                            fetch_keys(pane, base, &channel),
                        ];
                        if let Some(artifact) = auto_expand {
                            tasks.push(Task::done(Message::ToggleArtifact(pane, artifact)));
                        }
                        Task::batch(tasks)
                    }
                    Err(err) => {
                        state.content = Content::Error(err);
                        Task::none()
                    }
                }
            }
            Message::Refresh(pane) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    let base = state.base().to_string();
                    let channel = state.channel.clone();
                    state.content = Content::Loading;
                    return fetch(pane, base, &channel);
                }
                Task::none()
            }
            Message::Close(pane) => {
                if let Some((_, sibling)) = self.panes.close(pane) {
                    self.focus = Some(sibling);
                }
                Task::none()
            }
            Message::PaneResized(pane_grid::ResizeEvent { split, ratio }) => {
                self.panes.resize(split, ratio);
                Task::none()
            }
            Message::PaneDragged(pane_grid::DragEvent::Dropped { pane, target }) => {
                self.panes.drop(pane, target);
                Task::none()
            }
            Message::PaneDragged(_) => Task::none(),
            Message::PaneClicked(pane) => {
                self.focus = Some(pane);
                Task::none()
            }
            Message::SplitPane(axis) => {
                let Some(focus) = self.focus else {
                    return Task::none();
                };
                // A split shows what the pane being split showed — duplicate
                // the focused pane's channel AND its remote override rather
                // than opening a pane on the empty string, which is
                // unfetchable (no channel to ask the host for) and, worse,
                // becomes the very next `open_or_focus`'s split target,
                // silently orphaning it. Dropping `remote` here used to
                // silently retarget a remote-watched split at THIS
                // machine's local channel of the same name (or error, if
                // none existed) — the one case where the comment above was
                // false; `base`/`remote` now travel together, matching how
                // `Message::Refresh` already resolves a pane's effective
                // host.
                let Some((channel, remote)) = self
                    .panes
                    .get(focus)
                    .map(|state| (state.channel.clone(), state.remote.clone()))
                else {
                    return Task::none();
                };
                let base = remote.clone().unwrap_or_else(|| HOST.to_string());
                let mut new_state = Pane::loading(&channel);
                new_state.remote = remote;
                if let Some((new_pane, _)) = self.panes.split(axis, focus, new_state) {
                    self.focus = Some(new_pane);
                    return fetch(new_pane, base, &channel);
                }
                Task::none()
            }
            Message::Watch(pane, session) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.watched = Some(session);
                    state.streaming = true; // try to stream; ends fast if not live
                    state.stream_nonce += 1;
                    state.feed.clear();
                    state.watchers.clear();
                    state.annotate_tx = None;
                    state.annotate_email = None;
                    state.conversation_len = 0;
                    state.worktree_commit = None;
                    // A block index belongs to the document being left; the
                    // feed is cleared here, so keeping it would aim at an op
                    // from a different session.
                    state.annotate_op = None;
                    // A different session has a different newest diff.
                    state.auto_expanded = false;
                }
                Task::none()
            }
            Message::CloseSession(pane) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.watched = None;
                    state.streaming = false;
                    state.feed.clear();
                    state.watchers.clear();
                    state.annotate_tx = None;
                    state.annotate_email = None;
                    state.conversation_len = 0;
                    state.worktree_commit = None;
                    state.annotate_op = None;
                    // A different session has a different newest diff.
                    state.auto_expanded = false;
                }
                Task::none()
            }
            Message::Live(session, op, event) => {
                let mut scroll = None;
                for (_, state) in self.panes.iter_mut() {
                    if state.watched.as_deref() == Some(session.as_str()) {
                        let item = FeedItem {
                            md: feed_markdown(&event),
                            event,
                            op,
                        };
                        // Coalesce streaming Markdown segments by seq.
                        match state.feed.last_mut() {
                            Some(last)
                                if item.event.seq != 0 && last.event.seq == item.event.seq =>
                            {
                                *last = item;
                            }
                            _ => state.feed.push(item),
                        }
                        scroll = Some(state.scroll_id.clone());
                        break;
                    }
                }
                // Keep the newest live output in view.
                scroll.map_or_else(Task::none, iced::widget::operation::snap_to_end)
            }
            Message::LiveEnded(session) => {
                let mut to_refresh = None;
                for (pane, state) in self.panes.iter_mut() {
                    if state.watched.as_deref() == Some(session.as_str()) {
                        // Stop streaming but stay on the session, so its landed
                        // record + steer (resume) box remain. Refetch to pick up
                        // the persisted memo/artifacts.
                        state.streaming = false;
                        state.watchers.clear();
                        state.annotate_tx = None;
                        state.annotate_email = None;
                        state.conversation_len = 0;
                        state.worktree_commit = None;
                        // The picked block belonged to a document that is
                        // gone; a stale index would anchor at the wrong op
                        // on the next turn.
                        state.annotate_op = None;
                        to_refresh = Some(*pane);
                        break;
                    }
                }
                match to_refresh {
                    Some(pane) => {
                        let target = self
                            .panes
                            .get(pane)
                            .map(|p| (p.base().to_string(), p.channel.clone()));
                        target.map_or_else(Task::none, |(base, c)| fetch(pane, base, &c))
                    }
                    None => Task::none(),
                }
            }
            Message::RemoteChanged(pane, value) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    let value = value.trim().to_string();
                    state.remote = (!value.is_empty()).then_some(value);
                }
                Task::none()
            }
            Message::WatchEmailChanged(pane, value) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    // Trimmed on store, exactly as `remote` already is
                    // (`Message::RemoteChanged`, above): a pasted email
                    // with surrounding whitespace must still resolve
                    // `load_signing_key`'s lookup and match verbatim what
                    // the `Auth` frame sends.
                    state.watch_email = value.trim().to_string();
                }
                Task::none()
            }
            Message::Watchers(session, watchers) => {
                for (_, state) in self.panes.iter_mut() {
                    if state.watched.as_deref() == Some(session.as_str()) {
                        state.watchers = watchers;
                        break;
                    }
                }
                Task::none()
            }
            Message::LiveConnected(session, tx, email) => {
                for (_, state) in self.panes.iter_mut() {
                    if state.watched.as_deref() == Some(session.as_str()) {
                        state.annotate_tx = Some(tx);
                        state.annotate_email = Some(email);
                        break;
                    }
                }
                Task::none()
            }
            Message::ConversationLen(session, len) => {
                for (_, state) in self.panes.iter_mut() {
                    if state.watched.as_deref() == Some(session.as_str()) {
                        state.conversation_len = len;
                        break;
                    }
                }
                Task::none()
            }
            Message::WorktreeDiff(session, commit) => {
                for (_, state) in self.panes.iter_mut() {
                    if state.watched.as_deref() == Some(session.as_str()) {
                        state.worktree_commit = Some(commit);
                        break;
                    }
                }
                Task::none()
            }
            Message::AnchorPress(pane, target, line) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.aim_at(&target, drag_lines(line, line));
                    state.drag_from = Some(line);
                }
                Task::none()
            }
            Message::AnchorOver(pane, target, line) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.hover = Some((target.key().to_string(), line));
                    // Extend only within the target the drag started on: a span
                    // whose ends came from two different files would be a signed
                    // claim about code that never existed.
                    if let Some(from) = state.drag_from
                        && state.aimed_key() == Some(target.key())
                    {
                        state.aim_at(&target, drag_lines(from, line));
                    }
                }
                Task::none()
            }
            Message::AnchorRelease(pane) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.drag_from = None;
                    state.hover = None;
                }
                Task::none()
            }
            Message::AnchorStream(pane, op) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.annotate_op = Some(op);
                    // Clearing the path is what makes `AnnotateSubmit` build a
                    // `StreamAnchor` rather than a `CodeAnchor`.
                    state.annotate_path.clear();
                    state.annotate_lines.clear();
                    state.annotate_record = None;
                }
                Task::none()
            }
            Message::AnchorClear(pane) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.annotate_op = None;
                    state.annotate_record = None;
                    // A different session has a different newest diff.
                    state.auto_expanded = false;
                    state.annotate_path.clear();
                    state.annotate_lines.clear();
                }
                Task::none()
            }
            Message::AnnotatePathChanged(pane, value) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.annotate_path = value;
                }
                Task::none()
            }
            Message::AnnotateLinesChanged(pane, value) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.annotate_lines = value;
                }
                Task::none()
            }
            Message::AnnotateBodyChanged(pane, value) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.annotate_body = value;
                }
                Task::none()
            }
            Message::AnnotateUrgentToggled(pane, on) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.annotate_urgent = on;
                }
                Task::none()
            }
            Message::AnnotateSubmit(pane) => {
                // NOTE: the author is NOT taken from this machine's git
                // identity. It is resolved below from the channel's own roster
                // by the email the socket authenticated as — see `author_for`.
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                let push_error = |state: &mut Pane, text: String| {
                    state.feed.push(FeedItem {
                        md: None,
                        event: error_event(text),
                        // Locally invented; it exists in no document.
                        op: None,
                    });
                };
                let body = state.annotate_body.trim().to_string();
                if body.is_empty() {
                    return Task::none();
                }
                let Some(session_str) = state.watched.clone() else {
                    return Task::none();
                };
                let Some(mut tx) = state.annotate_tx.clone() else {
                    return Task::none();
                };
                let Ok(session) = session_str.parse::<EntryId>() else {
                    push_error(state, "malformed session id".to_string());
                    return Task::none();
                };
                // The email the socket actually authenticated as — never a
                // live re-read of `watch_email`, which may have been edited
                // since the socket connected (see `Pane::annotate_email`'s
                // docs: doing so would sign as an identity the socket was
                // never authenticated as, which the host rejects outright).
                let Some(email) = state.annotate_email.clone() else {
                    push_error(
                        state,
                        "not authenticated yet — wait for the connection to finish".to_string(),
                    );
                    return Task::none();
                };
                // Sign with the authenticated identity's key on file — never
                // send unsigned, since the host would reject it anyway.
                let Some(signing_key) = load_signing_key(&email) else {
                    push_error(state, format!("no signing key on file for '{email}'"));
                    return Task::none();
                };
                let path = state.annotate_path.trim().to_string();
                // Anchor-sourcing rule, now three-way. A `RecordAnchor` is
                // preferred when the reviewer clicked record content, because
                // its claim is exactly true by construction: the entry id and
                // the digest name bytes that an append-only log can never
                // change, so there is nothing to fabricate and nothing to
                // re-anchor. A `CodeAnchor` is the stronger claim where it is
                // available, but it may only be built from a commit oid this
                // pane has actually SEEN arrive over the wire
                // (`Pane::worktree_commit`, set only from a real
                // `{"kind":"diff","commit":…}` worktree event) — never
                // fabricated, never a placeholder. Anything else, including an
                // empty path, is a `StreamAnchor` on a conversation event.
                // Clicking a row only fills in what a reviewer used to type; it
                // does not relax any of this.
                let anchor = if let Some((entry, digest)) = state.annotate_record.clone() {
                    let Ok(entry) = entry.parse::<EntryId>() else {
                        push_error(
                            state,
                            "malformed entry id for the clicked content".to_string(),
                        );
                        return Task::none();
                    };
                    let Ok(digest) = ContentDigest::new(digest) else {
                        push_error(
                            state,
                            "malformed digest for the clicked content".to_string(),
                        );
                        return Task::none();
                    };
                    let Some(span) = parse_span(&state.annotate_lines) else {
                        push_error(
                            state,
                            format!(
                                "invalid line range '{}' — use \"12\" or \"12-14\"",
                                state.annotate_lines
                            ),
                        );
                        return Task::none();
                    };
                    Anchor::Record(RecordAnchor {
                        entry,
                        digest,
                        span,
                    })
                } else if path.is_empty() {
                    // The block the reviewer pointed at, else the newest
                    // event. Nothing picked and an empty container means
                    // there is no real index to name, which is refused
                    // rather than saturated to a nonexistent index 0.
                    let Some(op) = state
                        .annotate_op
                        .or_else(|| state.conversation_len.checked_sub(1))
                    else {
                        push_error(
                            state,
                            "nothing to anchor to yet — wait for the first live event".to_string(),
                        );
                        return Task::none();
                    };
                    Anchor::Stream(StreamAnchor {
                        session,
                        op_id: op.to_string(),
                    })
                } else {
                    let Some(commit_str) = state.worktree_commit.clone() else {
                        push_error(
                            state,
                            "no commit seen yet for this session's worktree — clear the path to \
                             comment on the live stream instead"
                                .to_string(),
                        );
                        return Task::none();
                    };
                    let Ok(commit) = CommitOid::new(commit_str) else {
                        push_error(
                            state,
                            "malformed commit oid from the worktree feed".to_string(),
                        );
                        return Task::none();
                    };
                    let Some(span) = parse_span(&state.annotate_lines) else {
                        push_error(
                            state,
                            format!(
                                "invalid line range '{}' — use \"12\" or \"12-14\"",
                                state.annotate_lines
                            ),
                        );
                        return Task::none();
                    };
                    Anchor::Code(CodeAnchor {
                        commit,
                        path,
                        // Blob pinning is drift *detection* only (v1 doesn't
                        // do it, and `reanchor` never reads this field) —
                        // deliberately left unpinned, not forgotten.
                        blob: ContentDigest::new("sha256:unpinned")
                            .expect("static digest literal is always valid"),
                        span,
                    })
                };
                let mut annotation = Annotation {
                    id: AnnotationId::new(),
                    author: author_for(state.keys.as_ref(), &email),
                    anchor,
                    body,
                    excerpt: None,
                    supersedes: None,
                    urgent: state.annotate_urgent,
                    timestamp: Timestamp::now(),
                    signature: None,
                };
                if annotation.sign(&signing_key).is_err() {
                    push_error(state, "failed to sign annotation".to_string());
                    return Task::none();
                }
                let local = LiveDoc::new();
                if local.insert_annotation(&annotation).is_err() {
                    push_error(state, "failed to build annotation update".to_string());
                    return Task::none();
                }
                let frame = WireFrame::update(&local.export_snapshot());
                if tx.try_send(frame).is_err() {
                    push_error(
                        state,
                        "failed to send annotation — the connection may have dropped".to_string(),
                    );
                    return Task::none();
                }
                state.annotate_body.clear();
                Task::none()
            }
            Message::LaunchIntentChanged(pane, value) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.launch_intent = value;
                }
                Task::none()
            }
            Message::LaunchAgentPicked(pane, agent) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.launch_agent = Some(agent);
                }
                Task::none()
            }
            Message::LaunchModeChanged(pane, outcome) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.launch_outcome = outcome;
                }
                Task::none()
            }
            Message::LaunchWorkspaceChanged(pane, value) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.launch_workspace = value;
                }
                Task::none()
            }
            Message::BrowseWorkspace(pane) => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Pick the workspace repo for this session")
                        .pick_folder()
                        .await
                        .map(|handle| handle.path().display().to_string())
                },
                move |picked| Message::WorkspacePicked(pane, picked),
            ),
            Message::WorkspacePicked(pane, picked) => {
                if let (Some(state), Some(path)) = (self.panes.get_mut(pane), picked) {
                    state.launch_workspace = path;
                }
                Task::none()
            }
            Message::Launch(pane) => {
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                let intent = state.launch_intent.trim().to_string();
                if intent.is_empty() || state.launching {
                    return Task::none();
                }
                let base = state.base().to_string();
                let channel = state.channel.clone();
                let agent = state.launch_agent.as_ref().map(|a| a.slug.clone());
                let mode = if state.launch_outcome {
                    "outcome"
                } else {
                    "single"
                };
                let workspace = state.launch_workspace.trim().to_string();
                // Keep the intent until the launch succeeds, so a failed launch
                // doesn't lose what was typed.
                state.launching = true;
                state.launch_error = None;
                post_launch(pane, base, channel, intent, agent, mode, workspace)
            }
            Message::Launched(pane, result) => {
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                state.launching = false;
                match result {
                    Ok(()) => {
                        state.launch_error = None;
                        state.launch_intent.clear();
                        state.watch_newest = true; // stream the new session on load
                        let base = state.base().to_string();
                        let channel = state.channel.clone();
                        fetch(pane, base, &channel)
                    }
                    Err(err) => {
                        state.launch_error = Some(err);
                        Task::none()
                    }
                }
            }
            Message::ToggleArtifact(pane, artifact_id) => {
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                if state.artifacts.remove(&artifact_id).is_some() {
                    return Task::none(); // was expanded → collapse
                }
                state
                    .artifacts
                    .insert(artifact_id.clone(), ArtifactContent::Loading);
                let base = state.base().to_string();
                let channel = state.channel.clone();
                fetch_artifact(pane, base, channel, artifact_id)
            }
            Message::ArtifactLoaded(pane, artifact_id, result) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    let content = match result {
                        Ok(dto) => {
                            let md = (dto.format == "markdown")
                                .then(|| markdown::parse(&dto.content).collect::<Vec<_>>());
                            // Digested here, from the bytes actually received,
                            // so a `RecordAnchor` names exactly what the
                            // reviewer was looking at.
                            let digest = ContentDigest::sha256_of(dto.content.as_bytes())
                                .as_str()
                                .to_string();
                            ArtifactContent::Loaded {
                                format: dto.format,
                                body: dto.content,
                                md,
                                digest,
                            }
                        }
                        Err(err) => ArtifactContent::Error(err),
                    };
                    state.artifacts.insert(artifact_id, content);
                }
                Task::none()
            }
            Message::SteerTextChanged(pane, value) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.steer_text = value;
                }
                Task::none()
            }
            Message::Steer(pane) => {
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                let (Some(session), text) =
                    (state.watched.clone(), state.steer_text.trim().to_string())
                else {
                    return Task::none();
                };
                if text.is_empty() {
                    return Task::none();
                }
                let base = state.base().to_string();
                let channel = state.channel.clone();
                state.steer_text.clear();
                // Echo the message immediately so the exchange reads like a chat.
                state.feed.push(FeedItem {
                    event: LiveEvent {
                        kind: "you".into(),
                        text: text.clone(),
                        seq: 0,
                        html: false,
                        markdown: None,
                    },
                    md: None,
                    // A local echo of your own steer — the host's own copy
                    // arrives separately, with a real index.
                    op: None,
                });
                let scroll = state.scroll_id.clone();
                Task::batch([
                    post_steer(pane, base, channel, session, text),
                    iced::widget::operation::snap_to_end(scroll),
                ])
            }
            Message::Steered(pane, result) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    match result {
                        // A landed session resumes a new turn → start a fresh
                        // subscription (new nonce). A live one is already
                        // streaming in place — leave it (re-subscribing would
                        // replay and duplicate the feed).
                        Ok(()) => {
                            if !state.streaming {
                                state.streaming = true;
                                state.stream_nonce += 1;
                            }
                        }
                        Err(err) => state.feed.push(FeedItem {
                            event: LiveEvent {
                                kind: "error".into(),
                                text: err,
                                seq: 0,
                                html: false,
                                markdown: None,
                            },
                            md: None,
                            op: None,
                        }),
                    }
                }
                Task::none()
            }
            Message::Interrupt(pane) => {
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                let Some(session) = state.watched.clone() else {
                    return Task::none();
                };
                let base = state.base().to_string();
                let channel = state.channel.clone();
                post_act(pane, base, channel, session, "interrupt", None)
            }
            Message::Posted(pane) => {
                let target = self
                    .panes
                    .get(pane)
                    .map(|p| (p.base().to_string(), p.channel.clone()));
                target.map_or_else(Task::none, |(base, c)| fetch(pane, base, &c))
            }
            Message::ToggleLaunchOptions(pane) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.launch_expanded = !state.launch_expanded;
                }
                Task::none()
            }
            Message::ToggleHistory(pane) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.show_full_history = !state.show_full_history;
                }
                Task::none()
            }
            Message::LifecycleSelect(pane, kind) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    // Toggle off if the same form is already open.
                    state.lifecycle = (state.lifecycle != Some(kind)).then_some(kind);
                    state.lifecycle_text.clear();
                    state.lifecycle_target.clear();
                    state.lifecycle_error = None;
                }
                Task::none()
            }
            Message::LifecycleCancel(pane) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.lifecycle = None;
                    state.lifecycle_error = None;
                }
                Task::none()
            }
            Message::LifecycleTextChanged(pane, value) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.lifecycle_text = value;
                }
                Task::none()
            }
            Message::LifecycleTargetChanged(pane, value) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.lifecycle_target = value;
                }
                Task::none()
            }
            Message::LifecycleSubmit(pane) => {
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                let Some(kind) = state.lifecycle else {
                    return Task::none();
                };
                let text = state.lifecycle_text.trim().to_string();
                let target = state.lifecycle_target.trim().to_string();
                // Validate the inputs each act needs.
                let valid = match kind {
                    LifecycleKind::Close | LifecycleKind::Reopen | LifecycleKind::Diverge => {
                        !text.is_empty()
                    }
                    LifecycleKind::Converge | LifecycleKind::Rename => {
                        !text.is_empty() && !target.is_empty()
                    }
                };
                if !valid {
                    return Task::none();
                }
                state.lifecycle_pending = true;
                state.lifecycle_error = None;
                let base = state.base().to_string();
                post_lifecycle(pane, base, state.channel.clone(), kind, text, target)
            }
            Message::LifecycleDone(pane, result) => {
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                state.lifecycle_pending = false;
                match result {
                    Ok(outcome) => {
                        state.lifecycle = None;
                        state.lifecycle_error = None;
                        // Rename rebinds this pane to the new channel name.
                        if let LifecycleResult::Renamed(new_name) = &outcome {
                            state.channel = new_name.clone();
                        }
                        let base = state.base().to_string();
                        let channel = state.channel.clone();
                        let mut tasks = vec![
                            fetch(pane, base, &channel),
                            fetch_lineage_graph(),
                            fetch_channels(),
                        ];
                        // Diverge yields a child to open in its own pane.
                        if let LifecycleResult::OpenChild(name) = outcome {
                            let (_, task) = self.open_or_focus(&name);
                            tasks.push(task);
                        }
                        Task::batch(tasks)
                    }
                    Err(err) => {
                        state.lifecycle_error = Some(err);
                        Task::none()
                    }
                }
            }
            // --- members & devices disclosure: invite / redeem / retire / revoke ---
            Message::KeysFetched(pane, result) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    match result {
                        Ok(dto) => {
                            state.keys = Some(dto);
                            state.keys_error = None;
                        }
                        Err(err) => state.keys_error = Some(err),
                    }
                }
                Task::none()
            }
            Message::MembersToggle(pane) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.members_open = !state.members_open;
                }
                Task::none()
            }
            Message::IdentitySelect(pane, form) => {
                let all_channels: Vec<String> = self.channels.options().to_vec();
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                // Toggle off if the same form is already open — same
                // pattern as `LifecycleSelect`. Closing runs the same
                // cleanup as `IdentityCancel`: a minted code must never
                // outlive the form that shows it, or the countdown-tick
                // subscription (gated on the form being open) and the
                // retained code disagree.
                if state.identity_form.as_ref() == Some(&form) {
                    state.identity_form = None;
                    state.identity_error = None;
                    state.invite_minted = None;
                    state.redeem_preview = None;
                    state.redeem_outcomes.clear();
                    state.identity_notice = None;
                } else {
                    let is_invite = form == IdentityForm::Invite;
                    let current_channel = state.channel.clone();
                    state.identity_form = Some(form);
                    state.identity_member.clear();
                    state.identity_paste.clear();
                    state.identity_kind.clear();
                    state.identity_rationale.clear();
                    state.identity_error = None;
                    state.invite_minted = None;
                    state.redeem_preview = None;
                    state.redeem_outcomes.clear();
                    state.identity_notice = None;
                    if is_invite {
                        state.identity_channels = all_channels
                            .into_iter()
                            .map(|c| {
                                let ticked = c == current_channel;
                                (c, ticked)
                            })
                            .collect();
                    }
                }
                Task::none()
            }
            Message::IdentityCancel(pane) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.identity_form = None;
                    state.identity_error = None;
                    state.invite_minted = None;
                    state.redeem_preview = None;
                    state.redeem_outcomes.clear();
                    state.identity_notice = None;
                }
                Task::none()
            }
            Message::IdentityInput(pane, field, value) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    match field {
                        IdentityField::Member => state.identity_member = value,
                        IdentityField::Paste => state.identity_paste = value,
                        IdentityField::Kind => state.identity_kind = value,
                        IdentityField::Rationale => state.identity_rationale = value,
                    }
                }
                Task::none()
            }
            Message::IdentityChannelToggle(pane, idx) => {
                if let Some(state) = self.panes.get_mut(pane)
                    && let Some(entry) = state.identity_channels.get_mut(idx)
                {
                    entry.1 = !entry.1;
                }
                Task::none()
            }
            Message::IdentitySubmit(pane) => {
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                // A second submit while one is already in flight must never
                // reach the network — retire/revoke/invite all append to the
                // ledger, and a double-fire would double-park or double-mint.
                if state.identity_pending {
                    return Task::none();
                }
                let Some(form) = state.identity_form.clone() else {
                    return Task::none();
                };
                let base = state.base().to_string();
                let channel = state.channel.clone();
                match form {
                    IdentityForm::Invite => {
                        let member = state.identity_member.trim().to_string();
                        let channels: Vec<String> = state
                            .identity_channels
                            .iter()
                            .filter(|(_, on)| *on)
                            .map(|(name, _)| name.clone())
                            .collect();
                        if member.is_empty() || channels.is_empty() {
                            return Task::none();
                        }
                        state.identity_pending = true;
                        state.identity_error = None;
                        post_invite(pane, base, member, channels)
                    }
                    IdentityForm::Redeem => {
                        let enroll = state.identity_paste.trim().to_string();
                        if enroll.is_empty() {
                            return Task::none();
                        }
                        if state.redeem_preview.is_some() {
                            let kind = state.identity_kind.clone();
                            if kind.is_empty() {
                                return Task::none();
                            }
                            state.identity_pending = true;
                            state.identity_error = None;
                            post_redeem(pane, base, enroll, kind)
                        } else {
                            state.identity_pending = true;
                            state.identity_error = None;
                            post_preview_enroll(pane, base, enroll)
                        }
                    }
                    IdentityForm::Retire { .. } | IdentityForm::Revoke { .. } => {
                        let rationale = state.identity_rationale.trim().to_string();
                        if rationale.is_empty() {
                            return Task::none();
                        }
                        state.identity_pending = true;
                        state.identity_error = None;
                        post_park(pane, base, channel, form, rationale)
                    }
                }
            }
            Message::IdentityDone(pane, result) => {
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                state.identity_pending = false;
                match result {
                    Ok(IdentityResult::Previewed(dto)) => {
                        state.redeem_preview = Some(dto);
                        state.identity_error = None;
                        Task::none()
                    }
                    // Minted/Redeemed/Parked all changed host state — refetch
                    // `keys.json` (via `fetch`'s own chained fetch_keys) and
                    // the pane's `view.json` so the panel and the timeline
                    // agree.
                    Ok(IdentityResult::Minted(dto)) => {
                        state.invite_minted = Some(dto);
                        state.identity_error = None;
                        let base = state.base().to_string();
                        let channel = state.channel.clone();
                        fetch(pane, base, &channel)
                    }
                    Ok(IdentityResult::Redeemed(dto)) => {
                        state.redeem_outcomes = dto.outcomes;
                        state.redeem_preview = None;
                        state.identity_error = None;
                        let base = state.base().to_string();
                        let channel = state.channel.clone();
                        fetch(pane, base, &channel)
                    }
                    Ok(IdentityResult::Parked(n)) => {
                        state.identity_form = None;
                        state.identity_error = None;
                        state.identity_notice =
                            Some(format!("parked {n} grant{}", if n == 1 { "" } else { "s" }));
                        let base = state.base().to_string();
                        let channel = state.channel.clone();
                        fetch(pane, base, &channel)
                    }
                    Err(err) => {
                        state.identity_error = Some(err);
                        Task::none()
                    }
                }
            }
            Message::Tick => Task::none(),
            Message::SubstratesLoaded(paths) => {
                // Default the new-channel substrate to the first registered one.
                self.new_channel_repo = paths.first().cloned();
                self.substrates = paths;
                Task::none()
            }
            Message::NewChannelRepoChanged(repo) => {
                self.new_channel_repo = Some(repo);
                Task::none()
            }
            Message::NewChannelChanged(value) => {
                self.new_channel = value;
                Task::none()
            }
            Message::CreateChannel => {
                let name = self.new_channel.trim().to_string();
                if name.is_empty() {
                    return Task::none();
                }
                self.new_channel_error = None;
                // Pass the chosen substrate only when more than one is registered
                // (the host requires it then; with one it infers it).
                let repo = (self.substrates.len() > 1)
                    .then(|| self.new_channel_repo.clone())
                    .flatten();
                post_create_channel(name, repo)
            }
            Message::ChannelCreated(result) => match result {
                Ok(name) => {
                    self.new_channel.clear();
                    self.new_channel_error = None;
                    let (_, open) = self.open_or_focus(&name);
                    Task::batch([open, fetch_channels(), fetch_lineage_graph()])
                }
                Err(err) => {
                    self.new_channel_error = Some(err);
                    Task::none()
                }
            },
            Message::BriefLoaded(pane, md) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    // The brief is the agent-facing recall text; strip the id
                    // noise (UUIDs, @timestamps, digests) the human doesn't care
                    // about before rendering.
                    let humanized = md
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| humanize_brief(&s));
                    state.brief_md = humanized.as_deref().map(|s| markdown::parse(s).collect());
                    state.brief_text = humanized;
                }
                Task::none()
            }
            Message::OpenUrl(url) => {
                let _ = open::that(url);
                Task::none()
            }
            Message::Copy(text) => iced::clipboard::write(text),
            Message::RefreshAll => {
                let mut tasks = vec![
                    fetch_channels(),
                    fetch_lineage_graph(),
                    fetch_focus(),
                    fetch_agents(),
                    fetch_settings(),
                ];
                // Order doesn't matter for a refresh fan-out — every open pane
                // gets refetched regardless of iteration order.
                for (id, state) in self.panes.iter() {
                    tasks.push(fetch(*id, state.base().to_string(), &state.channel));
                }
                Task::batch(tasks)
            }
            Message::AutoRefresh => {
                // Ambient only — never refetch panes (would yank scroll/streaming).
                Task::batch([fetch_channels(), fetch_lineage_graph(), fetch_focus()])
            }
            Message::OpenAdmin(view) => {
                self.admin = view;
                Task::none()
            }
            Message::SettingsLoaded(settings) => {
                // Read once here, not from the view — `load_signing_key`
                // is a blocking file read + TOML parse, and views run
                // every frame.
                self.device_key_fingerprint = settings
                    .as_ref()
                    .and_then(|s| s.identity.as_ref())
                    .and_then(|i| load_signing_key(&i.email))
                    .map(|key| device_fingerprint(key.public_key().as_str()));
                self.settings = settings;
                Task::none()
            }
            Message::JoinInviteChanged(value) => {
                self.join_invite = value;
                Task::none()
            }
            Message::JoinSubmit => {
                let invite = self.join_invite.trim().to_string();
                if invite.is_empty() || self.join_pending {
                    return Task::none();
                }
                self.join_pending = true;
                self.join_error = None;
                // Never leave a stale success block on screen behind a new
                // attempt's error — the founder must never be handed a
                // previous paste's enroll code as if it were this one's.
                self.join_result = None;
                post_device_enroll(HOST.to_string(), invite, None)
            }
            Message::JoinDone(result) => {
                self.join_pending = false;
                match result {
                    Ok(enrolled) => {
                        // The mint may have been for this device's own git
                        // identity (if it matches the invite's email) —
                        // refresh the cached fingerprint so "this device"
                        // doesn't keep reporting "no device key" after a
                        // successful join.
                        if let Some(identity) =
                            self.settings.as_ref().and_then(|s| s.identity.as_ref())
                        {
                            self.device_key_fingerprint = load_signing_key(&identity.email)
                                .map(|key| device_fingerprint(key.public_key().as_str()));
                        }
                        self.join_result = Some(enrolled);
                        self.join_invite.clear();
                    }
                    Err(err) => self.join_error = Some(err),
                }
                Task::none()
            }
            Message::RepoPathChanged(value) => {
                self.repo_path = value;
                Task::none()
            }
            Message::BrowseRepo => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Pick a git repo to register as a home substrate")
                        .pick_folder()
                        .await
                        .map(|h| h.path().display().to_string())
                },
                Message::RepoPicked,
            ),
            Message::RepoPicked(picked) => {
                if let Some(path) = picked {
                    self.repo_path = path;
                }
                Task::none()
            }
            Message::RepoChannelChanged(value) => {
                self.repo_channel = value;
                Task::none()
            }
            Message::SetupRepo => {
                let path = self.repo_path.trim().to_string();
                if path.is_empty() {
                    return Task::none();
                }
                self.repo_msg = None;
                post_setup_repo(path, self.repo_channel.trim().to_string())
            }
            Message::RepoSetupDone(result) => {
                match result {
                    Ok(()) => {
                        self.repo_msg = Some(Ok("registered".into()));
                        self.repo_path.clear();
                        self.repo_channel.clear();
                        // A new substrate + ambient channel exist now.
                        return Task::batch([
                            fetch_settings(),
                            fetch_substrates(),
                            fetch_channels(),
                            fetch_lineage_graph(),
                        ]);
                    }
                    Err(err) => self.repo_msg = Some(Err(err)),
                }
                Task::none()
            }
            Message::AgentEdit(agent) => {
                self.agent_slug = Some(agent.slug);
                self.agent_name = agent.name;
                self.agent_harness = self
                    .settings
                    .as_ref()
                    .and_then(|s| s.harnesses.iter().find(|h| h.id == agent.harness).cloned());
                self.agent_role = agent.role.unwrap_or_default();
                self.agent_model = agent.model.unwrap_or_default();
                self.agent_mcp = agent
                    .mcp_servers
                    .iter()
                    .map(|s| (s.name.clone(), s.url.clone()))
                    .collect();
                self.agent_skills = agent.skills.clone();
                self.agent_plugins = agent.plugins.clone();
                self.agent_msg = None;
                Task::none()
            }
            Message::AgentNew => {
                self.agent_slug = None;
                self.agent_name.clear();
                self.agent_harness = None;
                self.agent_role.clear();
                self.agent_model.clear();
                self.agent_mcp.clear();
                self.agent_skills.clear();
                self.agent_plugins.clear();
                self.agent_msg = None;
                Task::none()
            }
            Message::McpNameChanged(i, v) => {
                if let Some(row) = self.agent_mcp.get_mut(i) {
                    row.0 = v;
                }
                Task::none()
            }
            Message::McpUrlChanged(i, v) => {
                if let Some(row) = self.agent_mcp.get_mut(i) {
                    row.1 = v;
                }
                Task::none()
            }
            Message::McpAddRow => {
                self.agent_mcp.push((String::new(), String::new()));
                Task::none()
            }
            Message::McpRemove(i) => {
                if i < self.agent_mcp.len() {
                    self.agent_mcp.remove(i);
                }
                Task::none()
            }
            Message::SkillChanged(i, v) => {
                if let Some(s) = self.agent_skills.get_mut(i) {
                    *s = v;
                }
                Task::none()
            }
            Message::SkillAddRow => {
                self.agent_skills.push(String::new());
                Task::none()
            }
            Message::SkillRemove(i) => {
                if i < self.agent_skills.len() {
                    self.agent_skills.remove(i);
                }
                Task::none()
            }
            Message::PluginChanged(i, v) => {
                if let Some(p) = self.agent_plugins.get_mut(i) {
                    *p = v;
                }
                Task::none()
            }
            Message::PluginAddRow => {
                self.agent_plugins.push(String::new());
                Task::none()
            }
            Message::PluginRemove(i) => {
                if i < self.agent_plugins.len() {
                    self.agent_plugins.remove(i);
                }
                Task::none()
            }
            Message::PluginBrowse(i) => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Pick a local plugin directory")
                        .pick_folder()
                        .await
                        .map(|h| h.path().display().to_string())
                },
                move |picked| Message::PluginPicked(i, picked),
            ),
            Message::PluginPicked(i, picked) => {
                if let (Some(p), Some(path)) = (self.agent_plugins.get_mut(i), picked) {
                    *p = path;
                }
                Task::none()
            }
            Message::AgentNameChanged(value) => {
                self.agent_name = value;
                Task::none()
            }
            Message::AgentHarnessPicked(h) => {
                self.agent_harness = Some(h);
                Task::none()
            }
            Message::AgentRoleChanged(value) => {
                self.agent_role = value;
                Task::none()
            }
            Message::AgentModelChanged(value) => {
                self.agent_model = value;
                Task::none()
            }
            Message::SaveAgent => {
                let name = self.agent_name.trim().to_string();
                if name.is_empty() {
                    self.agent_msg = Some("a name is required".into());
                    return Task::none();
                }
                let harness = self
                    .agent_harness
                    .as_ref()
                    .map(|h| h.id.clone())
                    .or_else(|| {
                        self.settings
                            .as_ref()
                            .and_then(|s| s.harnesses.first().map(|h| h.id.clone()))
                    })
                    .unwrap_or_else(|| "claude".into());
                self.agent_msg = None;
                post_save_agent(
                    self.agent_slug.clone(),
                    name,
                    harness,
                    self.agent_role.trim().to_string(),
                    self.agent_model.trim().to_string(),
                    self.agent_mcp.clone(),
                    self.agent_skills.clone(),
                    self.agent_plugins.clone(),
                )
            }
            Message::AgentSaved(result) => match result {
                Ok(()) => {
                    self.agent_msg = None;
                    self.agent_slug = None;
                    self.agent_name.clear();
                    self.agent_harness = None;
                    self.agent_role.clear();
                    self.agent_model.clear();
                    self.agent_mcp.clear();
                    self.agent_skills.clear();
                    self.agent_plugins.clear();
                    fetch_agents()
                }
                Err(err) => {
                    self.agent_msg = Some(err);
                    Task::none()
                }
            },
            Message::DeleteAgent(slug) => post_delete_agent(slug),
            Message::AgentDeleted(result) => match result {
                Ok(()) => fetch_agents(),
                Err(err) => {
                    self.agent_msg = Some(err);
                    Task::none()
                }
            },
        }
    }

    /// Write shell state to disk, ignoring failure. A layout preference that
    /// cannot be saved is a lost preference, not an error worth a surface.
    fn persist_shell(&self) {
        let _ = shell::save(&shell_state_path(), &self.shell);
    }

    fn subscription(&self) -> iced::Subscription<Message> {
        // One live subscription per pane that is watching a session. The
        // authenticated websocket is preferred WHEREVER an identity to watch as
        // can be resolved — including the local host, since `Pane::base()`
        // already yields `HOST` when no remote is set. SSE remains the fallback
        // for a machine with no identity on file at all.
        //
        // This is deliberate, and it is a fix rather than a tidy-up: the
        // annotation composer only exists once a websocket reports
        // `Message::LiveConnected` (that is the only source of
        // `Pane::annotate_tx`), so gating the websocket on a NON-EMPTY `remote`
        // meant a reviewer watching their own session on their own machine got
        // no composer and therefore no clickable diff rows — the whole pointing
        // gesture was unreachable unless they knew to type this host's own URL
        // into a field labelled "remote". Ledger `02bded62`.
        let identity_email = self
            .settings
            .as_ref()
            .and_then(|s| s.identity.as_ref())
            .map(|i| i.email.as_str());
        let streams: Vec<_> = self
            .panes
            .iter()
            .filter_map(|(_, state)| {
                state
                    .watched
                    .as_ref()
                    .filter(|_| state.streaming)
                    .map(|session| {
                        // The typed override, else this machine's own identity.
                        let email = watch_identity(&state.watch_email, identity_email);
                        // 0.14 replaced `run_with_id(id, stream)` with
                        // `run_with(data, builder)`, where `builder` is a plain
                        // fn pointer and `data` is BOTH the stream's input and
                        // its identity — so the nonce rides in the data, which
                        // is what makes a new turn restart the subscription.
                        match email {
                            Some(email) => iced::Subscription::run_with(
                                (
                                    state.base().to_string(),
                                    state.channel.clone(),
                                    session.clone(),
                                    email,
                                    state.stream_nonce,
                                ),
                                |(base, channel, session, email, _nonce): &(
                                    String,
                                    String,
                                    String,
                                    String,
                                    u64,
                                )| {
                                    live_ws_stream(
                                        base.clone(),
                                        channel.clone(),
                                        session.clone(),
                                        email.clone(),
                                    )
                                },
                            ),
                            // No identity anywhere: a websocket would only fail
                            // its handshake, so keep the unauthenticated local
                            // progress feed (and no composer, honestly).
                            None => iced::Subscription::run_with(
                                (state.channel.clone(), session.clone(), state.stream_nonce),
                                |(channel, session, _nonce): &(String, String, u64)| {
                                    session_stream(channel.clone(), session.clone())
                                },
                            ),
                        }
                    })
            })
            .collect();
        // A slow ambient refresh so the focus board / lineage stay live as
        // agents work and sync pulls entries (panes are left alone).
        let tick =
            iced::time::every(std::time::Duration::from_secs(20)).map(|_| Message::AutoRefresh);
        // The invite countdown's 1-second tick — live ONLY while some
        // pane has its invite form open AND showing a minted, unexpired
        // code, so the app never wakes every second once the form is
        // closed, dismissed, or the code has expired.
        let now = now_millis();
        let counting_down = self.panes.iter().any(|(_, state)| {
            invite_countdown_live(
                state.identity_form.as_ref() == Some(&IdentityForm::Invite),
                state.invite_minted.as_ref().map(|dto| dto.expires_at),
                now,
            )
        });
        let countdown_tick = counting_down
            .then(|| iced::time::every(std::time::Duration::from_secs(1)).map(|_| Message::Tick));
        // Zed's dock bindings, since that is the reference point. These add no
        // state: they fire the same messages the chevrons do. The
        // command/control guard is still load-bearing, but not against a
        // FOCUSED text field: `iced::keyboard::listen()` only ever yields
        // events the rest of the UI left `Status::Ignored` (`iced_futures`'s
        // `keyboard::listen`), and a focused `text_input` marks its own key
        // events `Captured` — so a bare "b" typed into the new-channel
        // field or the composer never reaches this filter at all. The guard
        // instead protects the case where NOTHING is focused: without it, a
        // bare "b" or "r" typed anywhere else in the shell (e.g. right after
        // a click elsewhere clears focus) would toggle a blade rather than
        // being silently dropped.
        let keys = iced::keyboard::listen().filter_map(|event| {
            let iced::keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
                return None;
            };
            if !(modifiers.command() || modifiers.control()) {
                return None;
            }
            match key.as_ref() {
                iced::keyboard::Key::Character("b") => Some(Message::ToggleLeftBlade),
                iced::keyboard::Key::Character("r") => Some(Message::ToggleRightBlade),
                _ => None,
            }
        });
        iced::Subscription::batch(
            streams
                .into_iter()
                .chain([tick, keys])
                .chain(countdown_tick),
        )
    }

    fn view(&self) -> Element<'_, Message> {
        // Admin views replace the channel workspace when open.
        if let Some(view) = self.admin {
            let panel = match view {
                AdminView::Settings => settings_panel(self),
                AdminView::Agents => agents_panel(self),
            };
            return column![admin_toolbar(self.admin), panel]
                .spacing(10)
                .padding(10)
                .into();
        }

        // The channel workspace. `pane_grid::State` has always been the store;
        // this is the widget finally rendering it, which is what buys
        // arbitrary 2D nesting — split any pane on either axis, at any depth.
        let grid = pane_grid::PaneGrid::new(&self.panes, |id, pane, _maximized| {
            pane_grid::Content::new(channel_pane(self, id, pane)).title_bar(
                pane_grid::TitleBar::new(text(pane.channel.as_str()).size(15))
                    .controls(Element::from(
                        row![
                            button("↻").on_press(Message::Refresh(id)).padding(4),
                            // `State::close` removes nothing and returns `None`
                            // when `pane` has no sibling (the single-pane case,
                            // which is also the app's startup state) — disable
                            // rather than publish a click that does nothing.
                            button("×")
                                .on_press_maybe(
                                    (self.panes.len() > 1).then_some(Message::Close(id))
                                )
                                .padding(4),
                        ]
                        .spacing(6),
                    ))
                    .always_show_controls()
                    .padding(6),
            )
        })
        .on_resize(10, Message::PaneResized)
        .on_drag(Message::PaneDragged)
        .on_click(Message::PaneClicked)
        .width(Fill)
        .height(Fill)
        .spacing(6);

        let top_bar = container(admin_toolbar(self.admin)).padding(Padding {
            top: 10.0,
            right: 10.0,
            bottom: 0.0,
            left: 10.0,
        });
        let center: Element<Message> = container(column![grid].spacing(10).padding(10))
            .id(iced::widget::Id::new("center-grid-column"))
            .into();

        // The three-pane shell: collapsible blades either side of the channel
        // workspace. Each blade collapses to a stub rather than to zero so the
        // attention badge stays legible even when the blade is put away
        // (docs/attention.md — attention is the spine).
        let left: Element<Message> = if self.shell.left_collapsed {
            blade_stub(Side::Left, Some(self.focus_items.len()))
        } else {
            container(left_blade(self))
                .width(Length::Fixed(self.shell.left_width.get()))
                .height(Fill)
                .into()
        };
        let right: Element<Message> = if self.shell.right_collapsed {
            blade_stub(Side::Right, None)
        } else {
            container(right_blade(self))
                .width(Length::Fixed(self.shell.right_width.get()))
                .height(Fill)
                .into()
        };

        column![
            top_bar,
            row![
                left,
                container(center)
                    .id(iced::widget::Id::new("center-pane-grid"))
                    .width(Fill),
                right
            ]
            .spacing(0),
        ]
        .into()
    }
}

/// The always-visible top tab bar: channels · settings · agents.
fn admin_toolbar(current: Option<AdminView>) -> Element<'static, Message> {
    let tab = |label: &'static str, target: Option<AdminView>| {
        let active = current == target;
        button(text(label).size(13))
            .on_press(Message::OpenAdmin(target))
            .padding([4, 12])
            .style(move |_t, _s| tab_style(active))
    };
    row![
        text("junto").size(15),
        Space::new().width(16),
        tab("channels", None),
        tab("settings", Some(AdminView::Settings)),
        tab("agents", Some(AdminView::Agents)),
        Space::new().width(Fill),
        button(text("↻ refresh").size(12))
            .on_press(Message::RefreshAll)
            .padding([4, 12])
            .style(|_t, _s| chip_style(MUTED, false)),
    ]
    .spacing(4)
    .align_y(Center)
    .into()
}

/// Which side of the shell a blade sits on — used only to point its stub's
/// chevron outward and to route the stub's click to the right message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

/// The collapsed form of a blade: a narrow rail carrying a chevron to reopen
/// it and, on the left, the count of items wanting attention. Collapsing must
/// not be able to hide that count entirely.
fn blade_stub<'a>(side: Side, badge: Option<usize>) -> Element<'a, Message> {
    let (glyph, message) = match side {
        Side::Left => ("›", Message::ToggleLeftBlade),
        Side::Right => ("‹", Message::ToggleRightBlade),
    };
    let mut rail = column![button(text(glyph).size(13)).on_press(message).padding(4)]
        .spacing(6)
        .align_x(Center);
    if let Some(count) = badge.filter(|count| *count > 0) {
        rail = rail.push(text(count.to_string()).size(11).color(RED));
    }
    container(rail)
        .width(Length::Fixed(24.0))
        .height(Fill)
        .into()
}

/// The open/create channel controls: a type-ahead picker for existing
/// channels plus a name field, substrate picker, and error display for a
/// new one.
fn adder(app: &App) -> Element<'_, Message> {
    let open_row = row![
        text("open ▸").size(13).color(MUTED),
        combo_box(
            &app.channels,
            "type to search channels…",
            None,
            Message::ChannelPicked,
        )
        .width(Fill),
    ]
    .spacing(8)
    .align_y(Center);
    let new_row = row![
        text("· new ▸").size(13).color(MUTED),
        text_input("new channel name…", &app.new_channel)
            .on_input(Message::NewChannelChanged)
            .on_submit(Message::CreateChannel)
            .width(Fill)
            .padding(6),
        button("create").on_press(Message::CreateChannel).padding(6),
    ]
    .spacing(8)
    .align_y(Center);
    let mut adder_col = column![open_row, new_row].spacing(6);
    // When several substrates are registered, the host needs to know which.
    if app.substrates.len() > 1 {
        adder_col = adder_col.push(
            pick_list(
                app.substrates.clone(),
                app.new_channel_repo.clone(),
                Message::NewChannelRepoChanged,
            )
            .text_size(12)
            .padding(6),
        );
    }
    match &app.new_channel_error {
        Some(err) => adder_col
            .push(text(format!("⚠ {err}")).size(11).color(RED))
            .into(),
        None => adder_col.into(),
    }
}

/// Pinned navigation: the open channels, then the controls to open or create
/// one. Lives at the top of the left blade and never toggles away.
fn channel_nav(app: &App) -> Element<'_, Message> {
    let mut list = column![].spacing(2);
    for name in &app.channel_names {
        list = list.push(
            button(text(name.as_str()).size(12))
                .on_press(Message::ChannelPicked(name.clone()))
                .padding(4)
                .width(Fill)
                .style(|_t, _s| chip_style(MUTED, false)),
        );
    }
    // Axis-aware splitting of the focused pane — the workspace-level
    // counterpart to `adder`'s "open a channel into a pane".
    let split_row = row![
        button(text("split →").size(12))
            .on_press(Message::SplitPane(pane_grid::Axis::Vertical))
            .width(Fill)
            .padding(4),
        button(text("split ↓").size(12))
            .on_press(Message::SplitPane(pane_grid::Axis::Horizontal))
            .width(Fill)
            .padding(4),
    ]
    .spacing(4);
    column![scrollable(list).height(Fill), adder(app), split_row]
        .spacing(6)
        .into()
}

/// One focus-board chip: a tagged, coloured summary of a cross-channel
/// "needs you" item that jumps to its entry when clicked.
fn focus_chip(item: &FocusItem) -> Element<'_, Message> {
    let (tag, color) = match item.kind.as_str() {
        "gate" => ("gate", YELLOW),
        "awaiting-execution" => ("exec", MAUVE),
        _ => ("ratify", BLUE),
    };
    let chan = item.channel_name.clone().unwrap_or_default();
    let label = format!(
        "{tag} · {chan} · {}: {}",
        item.author,
        truncate(&item.summary, 40)
    );
    let mut chip = button(text(label).size(11))
        .padding([3, 9])
        .style(move |_t, _s| chip_style(color, false));
    if let Some(name) = &item.channel_name {
        chip = chip.on_press(Message::FocusChipPicked(
            name.clone(),
            item.entry_id.clone(),
        ));
    }
    chip.into()
}

/// The cross-channel "needs you" items — the focus board, relocated out of
/// the permanent top banner into the left blade where it can be put away.
fn attention_view(app: &App) -> Element<'_, Message> {
    // Body moved verbatim from the former top-banner block; it becomes a
    // vertical list rather than a horizontal chip strip, since the blade is
    // tall and narrow rather than short and wide.
    if app.focus_items.is_empty() {
        return text("focus · all clear").size(13).color(GREEN).into();
    }
    let mut items = column![
        text(format!("needs you ({}) ▸", app.focus_items.len()))
            .size(13)
            .color(YELLOW)
    ]
    .spacing(4);
    for item in &app.focus_items {
        items = items.push(focus_chip(item));
    }
    scrollable(items).height(Fill).into()
}

/// The left blade: pinned channel navigation above a switchable
/// Attention/Sessions view. Nav is pinned rather than switchable so changing
/// channels never costs a round trip through a view switcher.
fn left_blade(app: &App) -> Element<'_, Message> {
    let switcher = row![
        button(text("attention").size(12))
            .on_press(Message::LeftViewPicked(shell::LeftView::Attention))
            .padding(4)
            .style(move |_t, _s| tab_style(app.shell.left_view == shell::LeftView::Attention)),
        button(text("sessions").size(12))
            .on_press(Message::LeftViewPicked(shell::LeftView::Sessions))
            .padding(4)
            .style(move |_t, _s| tab_style(app.shell.left_view == shell::LeftView::Sessions)),
    ]
    .spacing(4);

    let body: Element<Message> = match app.shell.left_view {
        shell::LeftView::Attention => attention_view(app),
        shell::LeftView::Sessions => sessions_view(app),
    };

    column![
        button(text("‹").size(13))
            .on_press(Message::ToggleLeftBlade)
            .padding(4),
        container(channel_nav(app))
            .id(iced::widget::Id::new("left-blade-nav"))
            .height(Length::FillPortion(
                (app.shell.left_split.get() * 100.0) as u16
            )),
        switcher,
        container(body)
            .id(iced::widget::Id::new("left-blade-body"))
            .height(Length::FillPortion(
                ((1.0 - app.shell.left_split.get()) * 100.0) as u16
            )),
    ]
    .spacing(6)
    .padding(8)
    .into()
}

/// The right blade: a switchable Artifacts/Lineage view.
fn right_blade(app: &App) -> Element<'_, Message> {
    let switcher = row![
        button(text("artifacts").size(12))
            .on_press(Message::RightViewPicked(shell::RightView::Artifacts))
            .padding(4)
            .style(move |_t, _s| tab_style(app.shell.right_view == shell::RightView::Artifacts)),
        button(text("lineage").size(12))
            .on_press(Message::RightViewPicked(shell::RightView::Lineage))
            .padding(4)
            .style(move |_t, _s| tab_style(app.shell.right_view == shell::RightView::Lineage)),
    ]
    .spacing(4);

    let body: Element<Message> = match app.shell.right_view {
        shell::RightView::Artifacts => artifacts_view(app),
        shell::RightView::Lineage => lineage_view(app),
    };

    column![
        row![
            switcher,
            button(text("›").size(13))
                .on_press(Message::ToggleRightBlade)
                .padding(4)
        ]
        .spacing(6),
        body,
    ]
    .spacing(6)
    .padding(8)
    .into()
}

/// The whole lineage DAG, relocated from the always-visible top ribbon into
/// the right blade. Freed from the top band it no longer needs the 150px
/// scroll cap the ribbon imposed — it gets the blade's full height.
///
/// The scrollable here is deliberately vertical-only, NOT both-axis. A
/// `scrollable::Direction::Both` arms width-compression on the canvas's own
/// `Limits` (`Scrollable::layout`, `iced_widget-0.14.2/src/scrollable.rs:
/// 447-462`), which makes `Canvas::layout`'s `Length::Fill` width resolve to
/// its zero intrinsic size instead of the blade's real width
/// (`Limits::resolve`, `iced_core-0.14.0/src/layout/limits.rs:167-171`) —
/// `Canvas::draw` then bails out on `bounds.width < 1.0`
/// (`canvas.rs:290-294`) and the whole graph goes blank at every blade
/// width, not just `BladeWidth::MIN`. `LineageCanvas` only carries a
/// `height` field and derives its horizontal `right` edge from the ACTUAL
/// bounds at draw time, so there is no fixed content width to hand a
/// horizontal scrollbar either. The narrow-blade clip this leaves
/// unresolved is a known limitation of the graph's provisional placement in
/// a blade rather than its original full-window-width ribbon.
fn lineage_view(app: &App) -> Element<'_, Message> {
    match &app.lineage {
        Some(graph) => {
            let open: HashSet<String> = app
                .panes
                .iter()
                .map(|(_, pane)| pane.channel.clone())
                .collect();
            let canvas = LineageCanvas::layout(graph, &open);
            let content_h = canvas.height.max(60.0);
            scrollable(
                Canvas::new(canvas)
                    .width(Fill)
                    .height(Length::Fixed(content_h)),
            )
            .height(Fill)
            .into()
        }
        None => text("no lineage yet").size(12).color(MUTED).into(),
    }
}

/// Artifacts attached to the focused channel — diffs, logs, charts. Rendered
/// from the focused pane's existing artifact state rather than a new fetch.
fn artifacts_view(app: &App) -> Element<'_, Message> {
    let Some(id) = app.focus else {
        return text("no channel focused").size(12).color(MUTED).into();
    };
    let Some(pane) = app.panes.get(id) else {
        return text("no channel focused").size(12).color(MUTED).into();
    };
    match &pane.content {
        Content::Loading => return text("loading…").size(12).color(MUTED).into(),
        Content::Error(err) => return text(format!("⚠ {err}")).size(12).color(RED).into(),
        Content::Loaded(_) => {}
    }
    let mut items = column![].spacing(4);
    for entry in pane.artifact_entries() {
        items = items.push(artifact_row(id, pane, entry));
    }
    scrollable(items).height(Fill).into()
}

/// Agent sessions for the focused channel.
fn sessions_view(app: &App) -> Element<'_, Message> {
    let Some(id) = app.focus else {
        return text("no channel focused").size(12).color(MUTED).into();
    };
    let Some(pane) = app.panes.get(id) else {
        return text("no channel focused").size(12).color(MUTED).into();
    };
    match &pane.content {
        Content::Loading => return text("loading…").size(12).color(MUTED).into(),
        Content::Error(err) => return text(format!("⚠ {err}")).size(12).color(RED).into(),
        Content::Loaded(_) => {}
    }
    let mut items = column![].spacing(4);
    for session in pane.session_list() {
        items = items.push(session_row(id, pane, session));
    }
    scrollable(items).height(Fill).into()
}

/// One artifact entry in the right blade's Artifacts view: the same kind
/// badge, `Message::ToggleArtifact` wiring, and (once expanded) the same
/// `artifact_body` rendering the pane's own entry card uses with no pointing
/// aim — the code path `entry_card` already takes whenever the pane's
/// annotate composer isn't live — rather than new artifact markup.
fn artifact_row<'a>(
    id: pane_grid::Pane,
    pane: &'a Pane,
    entry: &'a EntryDto,
) -> Element<'a, Message> {
    let expanded = pane.artifacts.get(&entry.id);
    let toggle_label = if expanded.is_some() {
        "hide content ▾"
    } else {
        "show content ▸"
    };
    let mut card = column![
        row![
            badge(artifact_label(&entry.summary), kind_color(&entry.kind)),
            text(truncate(&entry.author, 24)).size(11).color(MUTED),
        ]
        .spacing(8),
        button(text(toggle_label).size(11))
            .on_press(Message::ToggleArtifact(id, entry.id.clone()))
            .padding([2, 8])
            .style(|_t, _s| chip_style(TEAL, false)),
    ]
    .spacing(6);
    match expanded {
        Some(ArtifactContent::Loading) => {
            card = card.push(text("loading…").size(11).color(MUTED));
        }
        Some(ArtifactContent::Error(err)) => {
            card = card.push(text(format!("⚠ {err}")).size(11).color(RED));
        }
        Some(ArtifactContent::Loaded {
            format,
            body,
            md,
            digest,
        }) => {
            card = card.push(artifact_body(
                id,
                &entry.id,
                digest,
                format,
                body,
                md.as_deref(),
                None,
            ));
        }
        None => {}
    }
    container(card)
        .padding(8)
        .width(Fill)
        .style(|_theme| container::Style {
            background: Some(Background::Color(SURFACE)),
            border: Border {
                color: BORDER,
                width: 1.0,
                radius: 6.0.into(),
            },
            text_color: Some(TEXT),
            ..container::Style::default()
        })
        .into()
}

/// One session chip in the left blade's Sessions view: the same intent/state
/// label, colour, and `Message::Watch` wiring as the pane's own inline
/// session-chip row, stacked full-width instead of run inline so it fits the
/// blade rather than overflowing it.
fn session_row<'a>(
    id: pane_grid::Pane,
    pane: &Pane,
    session: &'a SessionDto,
) -> Element<'a, Message> {
    let watching = pane.watched.as_deref() == Some(session.id.as_str());
    let label = format!("{} · {}", truncate(&session.intent, 22), session.state);
    button(text(label).size(11))
        .on_press(Message::Watch(id, session.id.clone()))
        .width(Fill)
        .padding([3, 8])
        .style(move |_t, _s| chip_style(status_color(&session.state), watching))
        .into()
}

/// A tab button: the active tab is filled + accented; the rest are plain.
fn tab_style(active: bool) -> button::Style {
    button::Style {
        background: active.then_some(Background::Color(Color { a: 0.22, ..MAUVE })),
        text_color: if active { TEXT } else { MUTED },
        border: Border {
            color: if active { MAUVE } else { Color::TRANSPARENT },
            width: 1.0,
            radius: 6.0.into(),
        },
        ..button::Style::default()
    }
}

/// A bordered card container used by the admin panels.
fn admin_card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(10)
        .width(Fill)
        .style(|_theme| container::Style {
            background: Some(Background::Color(Color { a: 0.4, ..SURFACE })),
            border: Border {
                color: BORDER,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// The settings view: read-only machine status + the register-a-repo form.
fn settings_panel(app: &App) -> Element<'_, Message> {
    let mut col = column![text("settings").size(18)].spacing(12);
    if let Some(s) = &app.settings {
        let kv = |k: &str, v: &str| {
            row![
                text(format!("{k}:")).size(12).color(MUTED).width(110),
                text(v.to_string()).size(12).color(TEXT),
            ]
            .spacing(6)
        };
        let mut harness = column![
            text("harness").size(13).color(TEAL),
            kv("protocol", &s.harness.protocol),
            kv("backend", &s.harness.backend),
            kv("auth", &s.harness.auth),
            kv("detail", &s.harness.detail),
        ]
        .spacing(3);
        if let Some(hint) = &s.harness.hint {
            harness = harness.push(text(format!("hint: {hint}")).size(12).color(YELLOW));
        }
        col = col.push(admin_card(harness));
        let mut subs = column![text("home substrates").size(13).color(TEAL)].spacing(3);
        for p in &s.substrates {
            subs = subs.push(text(p.clone()).size(12).color(TEXT));
        }
        col = col.push(admin_card(subs));
        let mut device = column![text("this device").size(13).color(TEAL)].spacing(3);
        match &s.identity {
            Some(i) => {
                device = device.push(
                    text(format!("{} <{}>", i.name, i.email))
                        .size(12)
                        .color(TEXT),
                );
                device = device.push(match &app.device_key_fingerprint {
                    Some(fp) => row![
                        badge("key on file", GREEN),
                        text(fp.clone()).size(11).color(MUTED),
                    ]
                    .spacing(6)
                    .align_y(Center),
                    None => row![
                        text("no device key on file — join a channel below to mint one")
                            .size(12)
                            .color(YELLOW)
                    ],
                });
            }
            None => {
                device = device.push(text("(no git identity)").size(12).color(MUTED));
            }
        }
        col = col.push(admin_card(device));
        col = col.push(text(format!("junto {}", s.version)).size(11).color(MUTED));
    } else {
        col = col.push(text("loading…").size(12).color(MUTED));
    }

    // Join a channel: paste a founder's invite to mint this device's key
    // pair (`POST /devices/enroll`) — the joiner half of pairing a second
    // machine, replacing `junto enroll` in a terminal.
    let mut join = column![text("join a channel").size(13).color(TEAL)].spacing(6);
    join = join.push(
        text_input("paste an invite (junto://enroll?code=…)…", &app.join_invite)
            .on_input(Message::JoinInviteChanged)
            .size(12)
            .padding(6),
    );
    let can_join = !app.join_pending && !app.join_invite.trim().is_empty();
    join = join.push(
        button(text(if app.join_pending {
            "joining…"
        } else {
            "join"
        }))
        .on_press_maybe(can_join.then_some(Message::JoinSubmit))
        .padding(6),
    );
    if let Some(err) = &app.join_error {
        join = join.push(text(format!("⚠ {err}")).size(11).color(RED));
    }
    if let Some(enrolled) = &app.join_result {
        join = join.push(
            column![
                text(format!("joined as {}", enrolled.email))
                    .size(12)
                    .color(GREEN),
                row![
                    text("enroll code (hand this to the founder)")
                        .size(12)
                        .color(MUTED),
                    copy_button(enrolled.url.clone()),
                ]
                .spacing(6)
                .align_y(Center),
                text(enrolled.url.clone()).size(10).color(TEXT),
                text(format!(
                    "fingerprint (read aloud): {}",
                    enrolled.fingerprint
                ))
                .size(11)
                .color(TEXT),
                text(format!(
                    "transport fingerprint: {}",
                    enrolled.transport_fingerprint
                ))
                .size(11)
                .color(TEXT),
                text("your secret key never leaves this machine")
                    .size(11)
                    .color(MUTED),
            ]
            .spacing(4),
        );
    }
    col = col.push(admin_card(join));

    // Register a repo as a home substrate — the GUI `junto init`.
    let mut repo = column![
        text("register a repo (home substrate)")
            .size(13)
            .color(TEAL)
    ]
    .spacing(6);
    repo = repo.push(
        row![
            text_input("git repo path…", &app.repo_path)
                .on_input(Message::RepoPathChanged)
                .padding(6),
            button("browse…").on_press(Message::BrowseRepo).padding(6),
        ]
        .spacing(6)
        .align_y(Center),
    );
    repo = repo.push(
        text_input(
            "ambient channel name (optional; defaults to the dir name)",
            &app.repo_channel,
        )
        .on_input(Message::RepoChannelChanged)
        .size(12)
        .padding(6),
    );
    repo = repo.push(button("register").on_press(Message::SetupRepo).padding(6));
    if let Some(msg) = &app.repo_msg {
        let (label, color) = match msg {
            Ok(m) => (m.clone(), GREEN),
            Err(e) => (format!("⚠ {e}"), RED),
        };
        repo = repo.push(text(label).size(11).color(color));
    }
    col = col.push(admin_card(repo));
    scrollable(col).height(Fill).into()
}

/// The agents view: the configured agents with edit/delete, plus a create/edit
/// form (core fields — name, harness, role, model).
fn agents_panel(app: &App) -> Element<'_, Message> {
    let mut list = column![text("agents").size(18)].spacing(8);
    if app.agents.is_empty() {
        list = list.push(text("no agents configured").size(12).color(MUTED));
    }
    for a in &app.agents {
        let detail = a
            .model
            .clone()
            .map(|m| format!(" · {m}"))
            .unwrap_or_default();
        let role = a.role.clone().unwrap_or_default();
        let entry = row![
            column![
                text(format!("{} · {}{}", a.name, a.harness, detail)).size(13),
                text(truncate(&role, 70)).size(11).color(MUTED),
            ]
            .spacing(2),
            Space::new().width(Fill),
            button(text("edit").size(11))
                .on_press(Message::AgentEdit(a.clone()))
                .padding([2, 8])
                .style(|_t, _s| chip_style(BLUE, false)),
            button(text("delete").size(11))
                .on_press(Message::DeleteAgent(a.slug.clone()))
                .padding([2, 8])
                .style(|_t, _s| chip_style(RED, false)),
        ]
        .spacing(6)
        .align_y(Center);
        list = list.push(admin_card(entry));
    }

    let editing = app.agent_slug.is_some();
    let harnesses: Vec<HarnessRef> = app
        .settings
        .as_ref()
        .map(|s| s.harnesses.clone())
        .unwrap_or_default();
    let mut form = column![
        text(if editing { "edit agent" } else { "new agent" })
            .size(13)
            .color(TEAL)
    ]
    .spacing(6);
    form = form.push(
        text_input("name (e.g. Security Reviewer)", &app.agent_name)
            .on_input(Message::AgentNameChanged)
            .padding(6),
    );
    if !harnesses.is_empty() {
        form = form.push(
            pick_list(
                harnesses,
                app.agent_harness.clone(),
                Message::AgentHarnessPicked,
            )
            .placeholder("harness")
            .text_size(12)
            .padding(6),
        );
    }
    form = form.push(
        text_input("role / system prompt (optional)", &app.agent_role)
            .on_input(Message::AgentRoleChanged)
            .size(12)
            .padding(6),
    );
    form = form.push(
        text_input("model override (optional)", &app.agent_model)
            .on_input(Message::AgentModelChanged)
            .size(12)
            .padding(6),
    );

    // --- advanced config: MCP servers, skills, local plugins ---
    let remove_btn = |msg: Message| {
        button(text("×").size(12))
            .on_press(msg)
            .padding([2, 8])
            .style(|_t, _s| chip_style(RED, false))
    };
    let add_btn = |label: &'static str, msg: Message| {
        button(text(label).size(11))
            .on_press(msg)
            .padding([2, 8])
            .style(|_t, _s| chip_style(MUTED, false))
    };

    let mut mcp = column![text("MCP servers").size(12).color(MUTED)].spacing(4);
    for (i, (name, url)) in app.agent_mcp.iter().enumerate() {
        mcp = mcp.push(
            row![
                text_input("name", name)
                    .on_input(move |v| Message::McpNameChanged(i, v))
                    .size(12)
                    .padding(6)
                    .width(Length::FillPortion(1)),
                text_input("https://…/mcp", url)
                    .on_input(move |v| Message::McpUrlChanged(i, v))
                    .size(12)
                    .padding(6)
                    .width(Length::FillPortion(2)),
                remove_btn(Message::McpRemove(i)),
            ]
            .spacing(6)
            .align_y(Center),
        );
    }
    mcp = mcp.push(add_btn("+ add server", Message::McpAddRow));
    form = form.push(mcp);

    let mut skills = column![text("skills").size(12).color(MUTED)].spacing(4);
    for (i, s) in app.agent_skills.iter().enumerate() {
        skills = skills.push(
            row![
                text_input("skill name (or plugin:skill)", s)
                    .on_input(move |v| Message::SkillChanged(i, v))
                    .size(12)
                    .padding(6),
                remove_btn(Message::SkillRemove(i)),
            ]
            .spacing(6)
            .align_y(Center),
        );
    }
    skills = skills.push(add_btn("+ add skill", Message::SkillAddRow));
    form = form.push(skills);

    let mut plugins = column![text("local plugins").size(12).color(MUTED)].spacing(4);
    for (i, p) in app.agent_plugins.iter().enumerate() {
        plugins = plugins.push(
            row![
                text_input("absolute plugin directory", p)
                    .on_input(move |v| Message::PluginChanged(i, v))
                    .size(12)
                    .padding(6),
                add_btn("browse…", Message::PluginBrowse(i)),
                remove_btn(Message::PluginRemove(i)),
            ]
            .spacing(6)
            .align_y(Center),
        );
    }
    plugins = plugins.push(add_btn("+ add plugin", Message::PluginAddRow));
    form = form.push(plugins);

    let mut actions = row![button("save").on_press(Message::SaveAgent).padding(6)].spacing(6);
    if editing {
        actions = actions.push(
            button(text("new").size(13))
                .on_press(Message::AgentNew)
                .padding(6)
                .style(|_t, _s| chip_style(MUTED, false)),
        );
    }
    form = form.push(actions);
    if let Some(msg) = &app.agent_msg {
        form = form.push(text(format!("⚠ {msg}")).size(11).color(RED));
    }
    scrollable(column![list, admin_card(form)].spacing(16))
        .height(Fill)
        .into()
}

/// A pane's remote-watch controls: the host base URL to watch a session on
/// (blank = this machine, `HOST`) and the member email to authenticate the
/// live websocket as (`load_signing_key` reads that identity's key from
/// this machine's `keys.toml`). Always visible, not just while watching a
/// session — set before picking a session chip.
///
/// Both fields are OVERRIDES, not requirements: blank means this machine and
/// this machine's identity (`pointing::watch_identity`). The email placeholder
/// names the identity that will actually be used, because leaving it blank used
/// to mean "no websocket, and therefore no annotation composer" with nothing on
/// screen saying so (ledger `02bded62`).
fn remote_row<'a>(
    id: pane_grid::Pane,
    pane: &'a Pane,
    machine_email: Option<&'a str>,
) -> Element<'a, Message> {
    let watch_placeholder = match machine_email {
        Some(email) => format!("watch as — default: {email}"),
        None => "watch as (email) — no identity on this machine".to_string(),
    };
    let inputs = row![
        text("remote ▸").size(11).color(MUTED),
        text_input("host (blank = local)", pane.remote.as_deref().unwrap_or(""))
            .on_input(move |v| Message::RemoteChanged(id, v))
            .size(11)
            .padding(4)
            .width(Length::FillPortion(2)),
        text_input(&watch_placeholder, &pane.watch_email)
            .on_input(move |v| Message::WatchEmailChanged(id, v))
            .size(11)
            .padding(4)
            .width(Length::FillPortion(1)),
    ]
    .spacing(6)
    .align_y(Center);
    let mut col = column![inputs].spacing(2);
    // The live subscription's id is keyed on (session, stream_nonce) — not on
    // these fields — so it keeps a keystroke from tearing down and
    // reconnecting the socket on every character. The cost is that editing
    // either field has no effect on an already-running watch.
    if pane.streaming {
        col = col.push(
            text("applies on next watch — doesn't affect the running connection")
                .size(10)
                .color(MUTED),
        );
    }
    col.into()
}

/// One channel pane's body: the remote-watch controls plus the entry feed.
/// The channel name and refresh/close controls live in the pane's
/// `pane_grid::TitleBar` instead (built at the call site in `view`) — only a
/// `TitleBar`'s area is draggable, so that's what gives PaneGrid's
/// drag-to-reorder a grab handle.
fn channel_pane<'a>(app: &'a App, id: pane_grid::Pane, pane: &'a Pane) -> Element<'a, Message> {
    let machine_email = app
        .settings
        .as_ref()
        .and_then(|s| s.identity.as_ref())
        .map(|i| i.email.as_str());
    container(
        column![
            remote_row(id, pane, machine_email),
            pane_body(id, pane, &app.agents, &app.channel_names)
        ]
        .spacing(8),
    )
    .width(Fill)
    .height(Fill)
    .padding(8)
    .style(|_theme| container::Style {
        background: Some(Background::Color(Color { a: 0.4, ..SURFACE })),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    })
    .into()
}

/// The curated brief (recall bridge) rendered as Markdown in a card at the top
/// of a pane — standing decisions + what needs attention.
fn brief_panel<'a>(items: &'a [markdown::Item], raw: &str) -> Element<'a, Message> {
    let body =
        markdown::view(items, Theme::CatppuccinMocha).map(|url| Message::OpenUrl(url.to_string()));
    let head = row![
        text("brief").size(11).color(TEAL),
        Space::new().width(Fill),
        copy_button(raw.to_string()),
    ]
    .align_y(Center);
    container(column![head, body].spacing(6))
        .padding(10)
        .width(Fill)
        .style(|_theme| container::Style {
            background: Some(Background::Color(Color { a: 0.5, ..SURFACE })),
            border: Border {
                color: TEAL,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// The inline form for a channel lifecycle act: the inputs it needs, a
/// confirm/cancel row, and any error.
///
/// `channels` are the names a converge may target. Converge and Rename used to
/// share one free-text box, but they are opposites: Rename invents a NEW name,
/// while Converge must name a channel that ALREADY EXISTS — and typing an
/// existing name by hand, exactly, from a list the app is already holding, is
/// the failure mode Dan hit converging the dogfood channel.
fn lifecycle_form<'a>(
    id: pane_grid::Pane,
    pane: &'a Pane,
    kind: LifecycleKind,
    channels: &'a [String],
) -> Element<'a, Message> {
    let mut col = column![].spacing(6);
    match kind {
        LifecycleKind::Diverge => {
            col = col.push(
                text_input("side-quest name…", &pane.lifecycle_text)
                    .on_input(move |v| Message::LifecycleTextChanged(id, v))
                    .on_submit(Message::LifecycleSubmit(id))
                    .size(12)
                    .padding(6),
            );
        }
        LifecycleKind::Converge => {
            // A channel cannot converge into itself, so it is not offered.
            let targets: Vec<String> = channels
                .iter()
                .filter(|name| *name != &pane.channel)
                .cloned()
                .collect();
            let selected =
                (!pane.lifecycle_target.trim().is_empty()).then(|| pane.lifecycle_target.clone());
            col = col
                .push(
                    pick_list(targets, selected, move |name: String| {
                        Message::LifecycleTargetChanged(id, name)
                    })
                    .placeholder("converge into which channel?")
                    .text_size(12)
                    .padding(6)
                    .width(Fill),
                )
                .push(
                    text_input("rationale (required)…", &pane.lifecycle_text)
                        .on_input(move |v| Message::LifecycleTextChanged(id, v))
                        .on_submit(Message::LifecycleSubmit(id))
                        .size(12)
                        .padding(6),
                );
        }
        LifecycleKind::Rename => {
            col = col
                .push(
                    text_input("new channel name…", &pane.lifecycle_target)
                        .on_input(move |v| Message::LifecycleTargetChanged(id, v))
                        .size(12)
                        .padding(6),
                )
                .push(
                    text_input("rationale (required)…", &pane.lifecycle_text)
                        .on_input(move |v| Message::LifecycleTextChanged(id, v))
                        .on_submit(Message::LifecycleSubmit(id))
                        .size(12)
                        .padding(6),
                );
        }
        LifecycleKind::Close | LifecycleKind::Reopen => {
            col = col.push(
                text_input("rationale (required)…", &pane.lifecycle_text)
                    .on_input(move |v| Message::LifecycleTextChanged(id, v))
                    .on_submit(Message::LifecycleSubmit(id))
                    .size(12)
                    .padding(6),
            );
        }
    }
    let confirm_label = if pane.lifecycle_pending {
        "working…"
    } else {
        kind.label()
    };
    let mut confirm = button(text(confirm_label).size(11))
        .padding([3, 10])
        .style(|_t, _s| chip_style(GREEN, true));
    if !pane.lifecycle_pending {
        confirm = confirm.on_press(Message::LifecycleSubmit(id));
    }
    col = col.push(
        row![
            confirm,
            button(text("cancel").size(11))
                .on_press(Message::LifecycleCancel(id))
                .padding([3, 10])
                .style(|_t, _s| chip_style(MUTED, false)),
        ]
        .spacing(6),
    );
    if let Some(err) = &pane.lifecycle_error {
        col = col.push(text(format!("⚠ {err}")).size(11).color(RED));
    }
    container(col)
        .padding(8)
        .width(Fill)
        .style(|_theme| container::Style {
            background: Some(Background::Color(Color { a: 0.4, ..SURFACE })),
            border: Border {
                color: BORDER,
                width: 1.0,
                radius: 4.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// The party disclosure that replaces the old flat `party: …` text row: a
/// header (member count + total device count) that expands into one row
/// per member, each with its own device rows underneath, plus — for a
/// founder — the invite/redeem acts bar. Read-only until `Pane::keys` has
/// loaded.
fn members_disclosure<'a>(id: pane_grid::Pane, pane: &'a Pane) -> Element<'a, Message> {
    let Some(keys) = &pane.keys else {
        return match &pane.keys_error {
            Some(err) => text(format!("members: {err}")).size(12).color(RED).into(),
            None => text("members · loading…").size(12).color(MUTED).into(),
        };
    };
    let device_count: usize = keys.members.iter().map(|m| m.devices.len()).sum();
    let header = button(
        text(format!(
            "{} members ({}) · devices: {device_count}",
            if pane.members_open { "▾" } else { "▸" },
            keys.members.len(),
        ))
        .size(12),
    )
    .on_press(Message::MembersToggle(id))
    .padding(6)
    .style(|_t, _s| chip_style(MUTED, false));

    let mut col = column![header].spacing(6);
    if let Some(err) = &pane.keys_error {
        col = col.push(text(format!("⚠ {err}")).size(11).color(RED));
    }
    if pane.members_open {
        for member in &keys.members {
            col = col.push(member_row(id, pane, keys, member));
        }
        if keys.viewer_is_founder {
            let mut acts = row![text("members ▸").size(11).color(MUTED)]
                .spacing(6)
                .align_y(Center);
            for form in [IdentityForm::Invite, IdentityForm::Redeem] {
                let active = pane.identity_form.as_ref() == Some(&form);
                let label = form.label();
                acts = acts.push(
                    button(text(label).size(11))
                        .on_press(Message::IdentitySelect(id, form))
                        .padding([2, 8])
                        .style(move |_t, _s| chip_style(TEAL, active)),
                );
            }
            col = col.push(acts);
            if let Some(form @ (IdentityForm::Invite | IdentityForm::Redeem)) = &pane.identity_form
            {
                col = col.push(identity_form(id, pane, form));
            }
            if let Some(notice) = &pane.identity_notice {
                col = col.push(
                    row![
                        text(notice.clone()).size(11).color(GREEN),
                        button(text("dismiss").size(10))
                            .on_press(Message::IdentityCancel(id))
                            .padding([1, 6])
                            .style(|_t, _s| chip_style(MUTED, false)),
                    ]
                    .spacing(8)
                    .align_y(Center),
                );
            }
        }
    }
    col.into()
}

/// One member row: display name, kind badge, and — for a founder, on a
/// still-active non-founder member — a revoke button; then the
/// member-summary line and one row per device, each with its own retire
/// button while active. The retire/revoke form opens directly under its
/// target row (never a separate global picker).
fn member_row<'a>(
    id: pane_grid::Pane,
    pane: &'a Pane,
    keys: &'a KeysDto,
    member: &'a KeyMemberDto,
) -> Element<'a, Message> {
    let kind_badge_color = if member.kind == "agent" { MAUVE } else { TEAL };
    let mut head = row![
        text(&member.display_name).size(12),
        badge(&member.kind, kind_badge_color),
    ]
    .spacing(6)
    .align_y(Center);
    let can_revoke =
        keys.viewer_is_founder && !member.revoked && member.email != keys.founder_email;
    if can_revoke {
        let target = IdentityForm::Revoke {
            email: member.email.clone(),
        };
        let active = pane.identity_form.as_ref() == Some(&target);
        head = head.push(
            button(text("revoke").size(10))
                .on_press(Message::IdentitySelect(id, target))
                .padding([1, 6])
                .style(move |_t, _s| chip_style(RED, active)),
        );
    }

    let mut col = column![head, text(member_summary(member)).size(11).color(MUTED)]
        .spacing(3)
        .padding(Padding::default().left(14));
    for grant in &member.devices {
        let mut line = row![text(device_line(grant)).size(11).color(MUTED)]
            .spacing(6)
            .align_y(Center);
        let can_retire = keys.viewer_is_founder && grant.retired_at.is_none();
        if can_retire {
            let target = IdentityForm::Retire {
                grant: grant.granted_by.clone(),
            };
            let active = pane.identity_form.as_ref() == Some(&target);
            line = line.push(
                button(text("retire").size(10))
                    .on_press(Message::IdentitySelect(id, target))
                    .padding([1, 6])
                    .style(move |_t, _s| chip_style(YELLOW, active)),
            );
        }
        col = col.push(line);
        if let Some(form @ IdentityForm::Retire { grant: g }) = &pane.identity_form
            && *g == grant.granted_by
        {
            col = col.push(identity_form(id, pane, form));
        }
    }
    if let Some(form @ IdentityForm::Revoke { email }) = &pane.identity_form
        && *email == member.email
    {
        col = col.push(identity_form(id, pane, form));
    }
    col.into()
}

/// The inline form for a founder identity act — the `IdentityForm`
/// analogue of `lifecycle_form`: the inputs it needs, a confirm/cancel row
/// (confirm shows `working…` while `identity_pending`, cancel always
/// enabled), and any error underneath. Redeem is two-phase: a first
/// submit previews (`/devices/preview`, nothing appended yet); once a
/// preview is on hand, the kind picker appears (no default —
/// `docs/adr/0035`) and a second submit actually redeems.
fn identity_form<'a>(
    id: pane_grid::Pane,
    pane: &'a Pane,
    form: &'a IdentityForm,
) -> Element<'a, Message> {
    let mut col = column![].spacing(6);
    // Whether the confirm button may activate at all — each kind's own
    // required-input rule, checked here so an unmet requirement makes the
    // button simply absent-of-`on_press`, never present-but-silently-refusing.
    let mut ready = true;
    match form {
        IdentityForm::Invite => {
            col = col.push(
                text_input("member email…", &pane.identity_member)
                    .on_input(move |v| Message::IdentityInput(id, IdentityField::Member, v))
                    .size(12)
                    .padding(6),
            );
            let mut channels = row![text("channels ▸").size(11).color(MUTED)]
                .spacing(6)
                .align_y(Center);
            for (idx, (name, ticked)) in pane.identity_channels.iter().enumerate() {
                channels = channels.push(
                    checkbox(*ticked)
                        .label(name.clone())
                        .on_toggle(move |_| Message::IdentityChannelToggle(id, idx))
                        .size(13)
                        .text_size(11),
                );
            }
            col = col.push(channels);
            let any_channel = pane.identity_channels.iter().any(|(_, on)| *on);
            ready = !pane.identity_member.trim().is_empty() && any_channel;
            if let Some(dto) = &pane.invite_minted {
                let remaining = countdown(dto.expires_at, now_millis());
                let status: Element<Message> = if remaining == "expired" {
                    text("expired — mint another").size(11).color(YELLOW).into()
                } else {
                    column![
                        row![
                            text(dto.url.clone()).size(11).color(TEAL),
                            copy_button(dto.url.clone()),
                            text(remaining).size(11).color(MUTED),
                        ]
                        .spacing(8)
                        .align_y(Center),
                        text(format!("covers: {}", dto.channels.join(", ")))
                            .size(10)
                            .color(MUTED),
                    ]
                    .spacing(2)
                    .into()
                };
                col = col.push(status);
            }
        }
        IdentityForm::Redeem => {
            if !pane.redeem_outcomes.is_empty() {
                let mut outcomes = column![].spacing(3);
                for outcome in &pane.redeem_outcomes {
                    let color = match outcome.result.as_str() {
                        "granted" => GREEN,
                        "already_a_member" | "invite_already_used" => YELLOW,
                        _ => RED,
                    };
                    let name = outcome
                        .channel_name
                        .clone()
                        .unwrap_or_else(|| outcome.channel.clone());
                    let detail = outcome
                        .detail
                        .as_deref()
                        .map(|d| format!(": {d}"))
                        .unwrap_or_default();
                    outcomes = outcomes.push(
                        text(format!("{name} — {}{detail}", outcome.result))
                            .size(11)
                            .color(color),
                    );
                    if let Some(warning) = outcome.warning.as_deref() {
                        outcomes =
                            outcomes.push(text(format!("  {warning}")).size(10).color(YELLOW));
                    }
                }
                col = col.push(outcomes);
            } else {
                col = col.push(
                    text_input("enroll code (pasted invite)…", &pane.identity_paste)
                        .on_input(move |v| Message::IdentityInput(id, IdentityField::Paste, v))
                        .size(12)
                        .padding(6),
                );
                if let Some(preview) = &pane.redeem_preview {
                    let channel_set = preview
                        .channels
                        .iter()
                        .map(|c| c.name.clone().unwrap_or_else(|| c.id.clone()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    col = col.push(
                        text(format!(
                            "{} <{}> · {} · transport {} · {channel_set}",
                            preview.display_name,
                            preview.email,
                            preview.fingerprint,
                            preview.transport_fingerprint
                        ))
                        .size(11)
                        .color(MUTED),
                    );
                    let mut kind_row = row![text("kind ▸").size(11).color(MUTED)]
                        .spacing(6)
                        .align_y(Center);
                    for kind in ["human", "agent"] {
                        let active = pane.identity_kind == kind;
                        kind_row = kind_row.push(
                            button(text(kind).size(11))
                                .on_press(Message::IdentityInput(
                                    id,
                                    IdentityField::Kind,
                                    kind.to_string(),
                                ))
                                .padding([2, 8])
                                .style(move |_t, _s| chip_style(TEAL, active)),
                        );
                    }
                    col = col.push(kind_row);
                    ready = !pane.identity_kind.is_empty();
                } else {
                    ready = !pane.identity_paste.trim().is_empty();
                }
            }
        }
        IdentityForm::Retire { .. } => {
            col = col.push(
                text_input("rationale (required)…", &pane.identity_rationale)
                    .on_input(move |v| Message::IdentityInput(id, IdentityField::Rationale, v))
                    .on_submit(Message::IdentitySubmit(id))
                    .size(12)
                    .padding(6),
            );
            ready = !pane.identity_rationale.trim().is_empty();
        }
        IdentityForm::Revoke { .. } => {
            col = col.push(
                text("the member stays in the party; only their entries after now stop counting.")
                    .size(11)
                    .color(MUTED),
            );
            col = col.push(
                text_input("rationale (required)…", &pane.identity_rationale)
                    .on_input(move |v| Message::IdentityInput(id, IdentityField::Rationale, v))
                    .on_submit(Message::IdentitySubmit(id))
                    .size(12)
                    .padding(6),
            );
            ready = !pane.identity_rationale.trim().is_empty();
        }
    }

    let outcomes_shown = matches!(form, IdentityForm::Redeem) && !pane.redeem_outcomes.is_empty();
    if outcomes_shown {
        col = col.push(
            button(text("dismiss").size(11))
                .on_press(Message::IdentityCancel(id))
                .padding([3, 10])
                .style(|_t, _s| chip_style(MUTED, false)),
        );
    } else {
        let confirm_label = if pane.identity_pending {
            "working…"
        } else {
            form.label()
        };
        let mut confirm = button(text(confirm_label).size(11))
            .padding([3, 10])
            .style(|_t, _s| chip_style(GREEN, true));
        if !pane.identity_pending && ready {
            confirm = confirm.on_press(Message::IdentitySubmit(id));
        }
        col = col.push(
            row![
                confirm,
                button(text("cancel").size(11))
                    .on_press(Message::IdentityCancel(id))
                    .padding([3, 10])
                    .style(|_t, _s| chip_style(MUTED, false)),
            ]
            .spacing(6),
        );
    }
    if let Some(err) = &pane.identity_error {
        col = col.push(text(format!("⚠ {err}")).size(11).color(RED));
    }
    container(col)
        .padding(8)
        .width(Fill)
        .style(|_theme| container::Style {
            background: Some(Background::Color(Color { a: 0.4, ..SURFACE })),
            border: Border {
                color: BORDER,
                width: 1.0,
                radius: 4.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// The annotation composer: a signed, span-anchored (or stream-anchored)
/// comment on a remote-watched session. Rendered only while
/// `pane.annotate_tx.is_some()` — the same condition Task 9's write-half
/// wiring uses, since a composer has no meaning outside a live, authenticated
/// watch. Empty `path` comments on the live stream itself (a `StreamAnchor`
/// on the most recent conversation event); a typed `path` + `lines` comments
/// on code, but only once `Message::WorktreeDiff` has actually carried a real
/// commit oid for this session (`Message::AnnotateSubmit`'s anchor-sourcing
/// rule — never fabricated).
fn annotate_composer(id: pane_grid::Pane, pane: &Pane) -> Element<'_, Message> {
    let path_input = text_input("path (blank = comment on the stream)", &pane.annotate_path)
        .on_input(move |v| Message::AnnotatePathChanged(id, v))
        .size(12)
        .padding(6)
        .width(Length::FillPortion(2));
    let lines_input = text_input("lines (\"12\" or \"12-14\")", &pane.annotate_lines)
        .on_input(move |v| Message::AnnotateLinesChanged(id, v))
        .size(12)
        .padding(6)
        .width(Length::FillPortion(1));
    let body_input = text_input("annotate…", &pane.annotate_body)
        .on_input(move |v| Message::AnnotateBodyChanged(id, v))
        .on_submit(Message::AnnotateSubmit(id))
        .size(12)
        .padding(6);
    let urgent = checkbox(pane.annotate_urgent)
        .label("urgent")
        .on_toggle(move |on| Message::AnnotateUrgentToggled(id, on))
        .size(14)
        .text_size(11);
    let submit = button(text("comment").size(11))
        .on_press(Message::AnnotateSubmit(id))
        .padding(6);
    // What this comment will actually be anchored to, stated plainly. The
    // inputs alone can't say it: an empty path could mean the newest event or a
    // block that was pointed at, and those land on different ops.
    let mut aimed = row![text(aim_label(pane)).size(11).color(TEAL)]
        .spacing(6)
        .align_y(Center);
    if pane.annotate_op.is_some() || !pane.annotate_path.trim().is_empty() {
        aimed = aimed.push(
            button(text("clear").size(10))
                .on_press(Message::AnchorClear(id))
                .padding([1, 6])
                .style(|_t, _s| chip_style(MUTED, false)),
        );
    }
    column![
        row![text("annotate ▸").size(11).color(MUTED), aimed]
            .spacing(8)
            .align_y(Center),
        row![path_input, lines_input].spacing(6),
        row![body_input, urgent, submit].spacing(6).align_y(Center),
    ]
    .spacing(4)
    .into()
}

/// One line naming where the composer is aimed, matching exactly what
/// `AnnotateSubmit` will build: a code span, a pointed-at stream block, or the
/// newest live event (the default when nothing has been pointed at).
///
/// Uses `●` and plain words only. `◆`/`▸`/`▾` do NOT render in this app's
/// configured font (Segoe UI) and paint as tofu boxes — Dan could not find the
/// feed gutter at all because of it (ledger `02ff24be`). `●` and `×` do render.
fn aim_label(pane: &Pane) -> String {
    // Checked first because it is the only aim that can never be refused: the
    // bytes are already in the record, so there is no commit to wait for.
    if let Some((entry, _)) = &pane.annotate_record {
        return format!(
            "● line {} of entry {}",
            pane.annotate_lines,
            &entry[..8.min(entry.len())]
        );
    }
    let path = pane.annotate_path.trim();
    if !path.is_empty() {
        return match pane.worktree_commit.as_deref() {
            Some(commit) => format!("● {path}:{} @ {}", pane.annotate_lines, &commit[..7]),
            // No commit has arrived, so `AnnotateSubmit` will refuse this
            // rather than fabricate one — say so before they type.
            None => format!("no commit seen yet for this worktree — cannot anchor {path}"),
        };
    }
    match pane.annotate_op {
        Some(op) => format!("● stream block #{op}"),
        None => "newest live event".to_string(),
    }
}

/// The floating comment panel: the same signed-annotation composer, anchored to
/// the row it is about instead of pinned to the bottom of the pane.
///
/// Deliberately carries no `path`/`lines` inputs — the reviewer got here by
/// clicking, so there is nothing to type. `×` clears the aim and hands the
/// bottom composer back.
fn annotate_popup(id: pane_grid::Pane, pane: &Pane) -> Element<'_, Message> {
    let head = row![
        text(aim_label(pane)).size(11).color(TEAL),
        Space::new().width(Fill),
        button(text("×").size(12))
            .on_press(Message::AnchorClear(id))
            .padding([0, 6])
            .style(|_t, _s| chip_style(MUTED, false)),
    ]
    .spacing(6)
    .align_y(Center);
    let body_input = text_input("comment on these lines…", &pane.annotate_body)
        .on_input(move |v| Message::AnnotateBodyChanged(id, v))
        .on_submit(Message::AnnotateSubmit(id))
        .size(12)
        .padding(6);
    let urgent = checkbox(pane.annotate_urgent)
        .label("urgent")
        .on_toggle(move |on| Message::AnnotateUrgentToggled(id, on))
        .size(14)
        .text_size(11);
    let submit = button(text("comment").size(11))
        .on_press(Message::AnnotateSubmit(id))
        .padding(6);
    container(
        column![
            head,
            body_input,
            row![Space::new().width(Fill), urgent, submit]
                .spacing(8)
                .align_y(Center),
        ]
        .spacing(6),
    )
    .padding(10)
    .style(|_theme| container::Style {
        // Fully opaque: this floats over the diff, so anything translucent
        // would leave code showing through the comment box.
        background: Some(Background::Color(SURFACE)),
        border: Border {
            color: MAUVE,
            width: 1.0,
            radius: 6.0.into(),
        },
        text_color: Some(TEXT),
        ..container::Style::default()
    })
    .into()
}

fn pane_body<'a>(
    id: pane_grid::Pane,
    pane: &'a Pane,
    agents: &'a [AgentDto],
    channels: &'a [String],
) -> Element<'a, Message> {
    let dto = match &pane.content {
        Content::Loading => {
            return container(text("loading…").color(MUTED)).padding(12).into();
        }
        Content::Error(err) => {
            return container(text(format!("error: {err}")).color(RED))
                .padding(12)
                .into();
        }
        Content::Loaded(dto) => dto,
    };

    // An artifact entry's expanded inline content, if the user opened it.
    let artifact_for = |entry: &EntryDto| pane.artifacts.get(&entry.id);
    // Parsed Markdown for a session memo entry's summary, if any.
    let summary_md_for = |entry: &EntryDto| pane.entry_md.get(&entry.id).map(Vec::as_slice);

    // The channel's own header: the members disclosure, a closed badge,
    // and the lifecycle acts (lineage lives in the top window-wide branch
    // graph).
    let mut header = column![].spacing(6);
    header = header.push(members_disclosure(id, pane));
    if dto.closed {
        header = header.push(row![badge("closed", RED)].spacing(8).align_y(Center));
    }
    // Lifecycle act buttons; a closed channel only offers reopen.
    let acts: &[LifecycleKind] = if dto.closed {
        &[LifecycleKind::Reopen, LifecycleKind::Rename]
    } else {
        &[
            LifecycleKind::Diverge,
            LifecycleKind::Converge,
            LifecycleKind::Rename,
            LifecycleKind::Close,
        ]
    };
    let mut bar = row![text("channel ▸").size(11).color(MUTED)]
        .spacing(6)
        .align_y(Center);
    for &k in acts {
        let active = pane.lifecycle == Some(k);
        bar = bar.push(
            button(text(k.label()).size(11))
                .on_press(Message::LifecycleSelect(id, k))
                .padding([2, 8])
                .style(move |_t, _s| chip_style(MUTED, active)),
        );
    }
    header = header.push(bar);
    if let Some(kind) = pane.lifecycle {
        header = header.push(lifecycle_form(id, pane, kind, channels));
    }

    // Launch a session: intent + agent picker + mode toggle + workspace.
    let intent_input = text_input("launch a session — what should it do?", &pane.launch_intent)
        .on_input(move |v| Message::LaunchIntentChanged(id, v))
        .padding(6);
    // Don't accept submits/clicks while a launch is in flight.
    let intent_input = if pane.launching {
        intent_input
    } else {
        intent_input.on_submit(Message::Launch(id))
    };
    let mut launch_btn = button(text(if pane.launching {
        "launching…"
    } else {
        "launch"
    }))
    .padding(6);
    if !pane.launching {
        launch_btn = launch_btn.on_press(Message::Launch(id));
    }
    let options_toggle = button(
        text(if pane.launch_expanded {
            "options ▾"
        } else {
            "options ▸"
        })
        .size(12),
    )
    .on_press(Message::ToggleLaunchOptions(id))
    .padding(6)
    .style(|_t, _s| chip_style(MUTED, false));
    let intent_row = row![intent_input, launch_btn, options_toggle].spacing(6);
    let agent_picker: Element<Message> = if agents.is_empty() {
        text("no agents configured").size(11).color(MUTED).into()
    } else {
        pick_list(agents.to_vec(), pane.launch_agent.clone(), move |a| {
            Message::LaunchAgentPicked(id, a)
        })
        .placeholder("default agent")
        .text_size(12)
        .padding(6)
        .into()
    };
    // Mode as a checkbox (matches the web): unchecked = a single turn (default);
    // checked = the code-PR push-gate verify/Grader loop (docs/adr/0025).
    let mode_checkbox = checkbox(pane.launch_outcome)
        .label("code-PR push-gate (verify loop)")
        .on_toggle(move |on| Message::LaunchModeChanged(id, on))
        .size(16)
        .text_size(12);
    let options_row = row![
        agent_picker,
        text_input(
            "workspace repo path (remembered after first launch)",
            &pane.launch_workspace,
        )
        .on_input(move |v| Message::LaunchWorkspaceChanged(id, v))
        .size(12)
        .padding(6),
        button(text("browse…").size(12))
            .on_press(Message::BrowseWorkspace(id))
            .padding(6),
    ]
    .spacing(6)
    .align_y(Center);
    let mut launch = column![intent_row].spacing(6);
    if pane.launch_expanded {
        launch = launch.push(options_row).push(mode_checkbox);
    }
    if let Some(err) = &pane.launch_error {
        launch = launch.push(text(format!("⚠ {err}")).size(11).color(RED));
    }

    // Session chips — click to stream a session's live feed.
    let mut chips = row![].spacing(6);
    for session in &dto.sessions {
        let watching = pane.watched.as_deref() == Some(session.id.as_str());
        let label = format!("{} · {}", truncate(&session.intent, 22), session.state);
        let chip = button(text(label).size(11))
            .on_press(Message::Watch(id, session.id.clone()))
            .padding([3, 8])
            .style(move |_t, _s| chip_style(status_color(&session.state), watching));
        chips = chips.push(chip);
    }

    // Main area: the selected session's view (record + live turn + steer) or
    // the entry timeline.
    let main: Element<Message> = if let Some(session_id) = pane.watched.clone() {
        // Header: the session's intent + state, a live indicator, and a close ×.
        let session_dto = dto.sessions.iter().find(|s| s.id == session_id);
        let intent = session_dto.map(|s| s.intent.clone()).unwrap_or_default();
        let state_label = session_dto.map(|s| s.state.clone()).unwrap_or_default();
        let mut header = row![text(format!("session · {}", truncate(&intent, 36))).size(13)]
            .spacing(8)
            .align_y(Center);
        if !state_label.is_empty() {
            header = header.push(badge(&state_label, status_color(&state_label)));
        }
        if pane.streaming {
            header = header.push(text("● live").size(11).color(GREEN));
        }
        // Presence, in the header rather than buried in the feed: who else is
        // looking at this session right now (`Message::Watchers`).
        if !pane.watchers.is_empty() {
            header = header.push(watchers_chip(&pane.watchers));
        }
        header = header.push(Space::new().width(Fill));
        header = header.push(
            button(text("× close").size(11))
                .on_press(Message::CloseSession(id))
                .padding([2, 8])
                .style(|_t, _s| chip_style(MUTED, false)),
        );

        // Where the composer is aimed. `Some` exactly while the composer is on
        // screen (`annotate_tx`), so diff rows and feed gutters become click
        // targets and stop being them together with it.
        let aim = pane.annotate_tx.is_some().then(|| Aim {
            path: pane.annotate_path.as_str(),
            record: pane
                .annotate_record
                .as_ref()
                .map(|(entry, _)| entry.as_str()),
            span: parse_span(&pane.annotate_lines),
            popup_at: popup_anchor(pane),
            hover: pane.hover.as_ref().map(|(key, line)| (key.as_str(), *line)),
            pane,
        });
        // REVIEW-FIRST ARRANGEMENT (ledger `532826c2`). The session's newest
        // diff is the pane's PRIMARY object and the entry record becomes a side
        // panel, because a reviewer arrives wanting to look at code and used to
        // get a filing cabinet in which code was a collapsed row. Falls back to
        // the record-only column when the session has no diff at all, rather
        // than showing an empty code panel.
        let primary = newest_diff_artifact(&dto.entries, &session_id);

        // The session's persisted record: its SessionStarted entry plus every
        // entry targeting it (memos, artifacts), in timeline order. The primary
        // diff is omitted — it is already the main panel, and showing it twice
        // is how the record got long enough to hide things in.
        let mut record = column![].spacing(8);
        for entry in &dto.entries {
            let is_primary = primary.is_some_and(|p| p.id == entry.id);
            if !is_primary
                && (entry.id == session_id || entry.target.as_deref() == Some(session_id.as_str()))
            {
                record = record.push(timeline_entry(
                    id,
                    entry,
                    false,
                    "",
                    None,
                    false,
                    artifact_for(entry),
                    summary_md_for(entry),
                    aim,
                ));
            }
        }
        // The live exchange (your steers + the agent's streaming output). Kept
        // visible after the turn lands until you leave the session.
        if pane.streaming || !pane.feed.is_empty() {
            record = record.push(text("— live turn —").size(11).color(MUTED));
            let mut feed = column![].spacing(6);
            for item in &pane.feed {
                feed = feed.push(feed_block(id, item, pane.annotate_op));
            }
            if pane.streaming {
                feed = feed.push(text("● working…").size(11).color(YELLOW));
            }
            record = record.push(feed);
        }
        let record_scroll = scrollable(record).id(pane.scroll_id.clone()).height(Fill);

        // Steer (resumes a landed turn, or steers a live one) + interrupt.
        let placeholder = if pane.streaming {
            "steer the running turn…"
        } else {
            "steer — resume the session with a follow-up…"
        };
        let steer_input = text_input(placeholder, &pane.steer_text)
            .on_input(move |v| Message::SteerTextChanged(id, v))
            .on_submit(Message::Steer(id))
            .padding(6);
        let mut interrupt_btn = button("interrupt").padding(6);
        if pane.streaming {
            interrupt_btn = interrupt_btn.on_press(Message::Interrupt(id));
        }
        let steer = row![
            steer_input,
            button("steer").on_press(Message::Steer(id)).padding(6),
            interrupt_btn,
        ]
        .spacing(6);
        let main_area: Element<Message> = match primary {
            Some(artifact) => row![
                container(code_panel(id, pane, artifact, aim)).width(Length::FillPortion(3)),
                container(record_scroll).width(Length::FillPortion(2)),
            ]
            .spacing(10)
            .height(Fill)
            .into(),
            None => record_scroll.into(),
        };
        let mut session_col = column![header, main_area, steer].spacing(8);
        // The bottom composer is the fallback surface. While a floating panel is
        // anchored to the clicked row it IS the composer, so showing both would
        // put two comment boxes on screen for one comment.
        if pane.annotate_tx.is_some() {
            if popup_anchor(pane).is_none() {
                session_col = session_col.push(annotate_composer(id, pane));
            }
        } else {
            // Say why there is nothing to click. Pointing needs a live,
            // authenticated socket, because a `CodeAnchor`'s commit may only
            // come from a `Message::WorktreeDiff` that actually arrived on the
            // wire — so on a landed session the diff rows are deliberately
            // inert. They look identical either way, and the host closes a
            // finished session's socket silently, so without this line the
            // reviewer just finds that clicking does nothing.
            session_col = session_col.push(
                text(
                    "commenting needs a live turn — steer above to resume this \
                     session, then click a diff line",
                )
                .size(11)
                .color(MUTED),
            );
        }
        session_col.into()
    } else {
        let highlight = pane.highlight_entry.as_deref();
        // A focus-board jump pins the attention entry above the scroll so it's
        // immediately visible; it's lifted out of the scrolled list below.
        let draft_for = |entry: &EntryDto| {
            pane.act_drafts
                .get(&entry.id)
                .map(String::as_str)
                .unwrap_or("")
        };
        let error_for = |entry: &EntryDto| pane.act_errors.get(&entry.id).map(String::as_str);
        let pending_for = |entry: &EntryDto| pane.act_pending.contains(&entry.id);
        let pinned: Option<Element<Message>> = highlight.and_then(|hid| {
            dto.entries.iter().find(|e| e.id == hid).map(|entry| {
                let header = row![
                    text("▾ needs you").size(11).color(YELLOW),
                    Space::new().width(Fill),
                    button(text("dismiss").size(11).color(MUTED))
                        .on_press(Message::ClearHighlight(id))
                        .padding([2, 8])
                        .style(|_t, _s| chip_style(MUTED, false)),
                ]
                .align_y(Center);
                column![
                    header,
                    timeline_entry(
                        id,
                        entry,
                        true,
                        draft_for(entry),
                        error_for(entry),
                        pending_for(entry),
                        artifact_for(entry),
                        summary_md_for(entry),
                        // The composer only exists inside a watched
                        // session, so nothing here is pointable.
                        None
                    )
                ]
                .spacing(4)
                .into()
            })
        });
        let total = dto.entries.len();
        let mut timeline = column![].spacing(8);
        // Lead with the channel's curated brief (recall bridge): standing
        // decisions + what needs attention. The full entry history is a click
        // away — when there's no brief, fall back to the most recent entries.
        if let Some(items) = &pane.brief_md {
            let raw = pane.brief_text.as_deref().unwrap_or("");
            timeline = timeline.push(brief_panel(items, raw));
        }
        // History disclosure. Collapsed default: the brief alone (or, without a
        // brief, the recent entries). Expanded: the full timeline.
        const RECENT: usize = 12;
        let have_brief = pane.brief_md.is_some();
        let show_all = pane.show_full_history;
        let start = if show_all {
            0
        } else if have_brief {
            total // brief covers it — hide the entry list
        } else {
            total.saturating_sub(RECENT) // no brief → show the recent tail
        };
        if total > 0 {
            let label = if show_all {
                "▾ hide full history".to_string()
            } else {
                format!("▸ show full history ({total} entries)")
            };
            timeline = timeline.push(
                button(text(label).size(11))
                    .on_press(Message::ToggleHistory(id))
                    .padding([2, 8])
                    .style(|_t, _s| chip_style(MUTED, false)),
            );
        }
        for entry in dto.entries.iter().skip(start) {
            if highlight == Some(entry.id.as_str()) {
                continue; // pinned above
            }
            timeline = timeline.push(timeline_entry(
                id,
                entry,
                false,
                draft_for(entry),
                error_for(entry),
                pending_for(entry),
                artifact_for(entry),
                summary_md_for(entry),
                None,
            ));
        }
        let scroll = scrollable(timeline).id(pane.scroll_id.clone()).height(Fill);
        match pinned {
            Some(pinned) => column![pinned, scroll].spacing(8).into(),
            None => scroll.into(),
        }
    };

    column![header, launch, chips, main]
        .spacing(8)
        .padding([8.0_f32, 10.0])
        .into()
}

/// The fixed width of the feed's pointing gutter, so pointable and
/// unpointable blocks stay left-aligned with each other.
const GUTTER: Length = Length::Fixed(14.0);

/// One feed block plus its pointing gutter: a narrow click target left of the
/// rendered line that aims the composer's `StreamAnchor` at THIS block
/// (`Message::AnchorStream`) rather than at the newest event, which is what it
/// always used to be.
///
/// The gutter is a sibling of the line, not a wrapper around it: an Iced
/// `button` consumes its content's own interactions, so wrapping a Markdown
/// block would silently kill its links.
///
/// It draws a filled BAR rather than a glyph. The first version used `▸`/`◆`,
/// which do not exist in this app's font and painted as tofu boxes, so the only
/// affordance for anchoring a stream block was invisible — Dan could not find it
/// (ledger `02ff24be`). A coloured rectangle depends on no font at all.
fn feed_block<'a>(
    id: pane_grid::Pane,
    item: &'a FeedItem,
    picked: Option<usize>,
) -> Element<'a, Message> {
    let gutter: Element<Message> = match item.op {
        Some(op) => {
            let lit = picked == Some(op);
            button(
                Space::new()
                    .width(Length::Fixed(3.0))
                    .height(Length::Fixed(14.0)),
            )
            .on_press(Message::AnchorStream(id, op))
            .width(GUTTER)
            .padding([0, 5])
            .style(move |_theme, status| {
                let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
                button::Style {
                    background: Some(Background::Color(if lit {
                        MAUVE
                    } else if hovered {
                        Color { a: 0.75, ..MAUVE }
                    } else {
                        // Dim but present: a reviewer has to be able to see
                        // that the target exists before hovering it.
                        Color { a: 0.30, ..MUTED }
                    })),
                    border: Border {
                        radius: 2.0.into(),
                        ..Border::default()
                    },
                    ..button::Style::default()
                }
            })
            .into()
        }
        // A line the app invented locally exists in no document, so there is
        // no op id to name and nothing to point at.
        None => Space::new().width(GUTTER).into(),
    };
    row![gutter, feed_line(item)]
        .spacing(4)
        .align_y(iced::Top)
        .into()
}

/// One live-feed line — kind-coloured, with any HTML the host rendered stripped
/// back to plain text (native can't paint HTML).
fn feed_line(item: &FeedItem) -> Element<'_, Message> {
    // Your own steer messages, echoed as a chat-style "you ›" line.
    if item.event.kind == "you" {
        return container(
            text(format!("you › {}", item.event.text))
                .size(13)
                .color(BLUE),
        )
        .padding([3, 8])
        .width(Fill)
        .style(|_theme| container::Style {
            background: Some(Background::Color(Color { a: 0.12, ..BLUE })),
            border: Border {
                radius: 4.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into();
    }
    // Model prose renders as Markdown; status/tool/error lines stay plain.
    if let Some(md) = &item.md {
        return markdown::view(md, Theme::CatppuccinMocha)
            .map(|url| Message::OpenUrl(url.to_string()));
    }
    let event = &item.event;
    let body = if event.html {
        strip_html(&event.text)
    } else {
        event.text.clone()
    };
    let color = match event.kind.as_str() {
        "thinking" => MUTED,
        "tool" => TEAL,
        "error" => RED,
        "result" => GREEN,
        _ => TEXT,
    };
    text(body).size(13).color(color).into()
}

/// Strip agent-facing id noise from the curated brief before showing it to a
/// human: channel/entry UUIDs, `@<timestamp>` tokens, and content digests are
/// dropped (the human surface acts via buttons, not by id).
fn humanize_brief(md: &str) -> String {
    md.lines()
        .map(|line| {
            line.split(' ')
                .filter(|tok| !is_id_noise(tok))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a token is an id/hash/timestamp a human reader doesn't want — tested
/// after stripping wrapping punctuation like `()`, backticks, and brackets.
fn is_id_noise(token: &str) -> bool {
    let core = token.trim_matches(|c: char| "()`,[]<>".contains(c));
    if core.is_empty() {
        return false;
    }
    // A `@1781910552547`-style epoch token.
    if let Some(digits) = core.strip_prefix('@')
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    // A content digest, e.g. `sha256:abcd…`.
    if core.contains(':') && core.split(':').next().is_some_and(|a| a == "sha256") {
        return true;
    }
    is_uuid(core) || is_hex_id(core)
}

/// A canonical 8-4-4-4-12 hex UUID.
fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(n, p)| p.len() == *n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// A bare hex run that's an id rather than a word: ≥6 hex chars **with at least
/// one digit** — catches short shas / entry-id prefixes (e.g. `943677b5`) while
/// sparing all-letter hex words like "facade", "decade", "defaced".
fn is_hex_id(s: &str) -> bool {
    s.len() >= 6
        && s.chars().all(|c| c.is_ascii_hexdigit())
        && s.chars().any(|c| c.is_ascii_digit())
}

/// Crude tag-stripper for the host's sanitized-HTML feed segments.
fn strip_html(input: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Epoch millis → an ISO-8601 date (`YYYY-MM-DD`), UTC. A small
/// civil-calendar conversion (Howard Hinnant's `civil_from_days`) rather
/// than a new date/time dependency — a retired grant's `retired_at` is
/// the only place this crate needs the date portion of a timestamp.
fn iso_date(millis: i64) -> String {
    let (y, m, d) = civil_from_days(millis.div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since the Unix epoch → `(year, month, day)`, UTC. Standard
/// proleptic-Gregorian conversion (Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The current wall clock as epoch millis — `0` on a clock error (never
/// panics; only ever compared against a future `expires_at`, so losing to
/// `0` just reads as "not counting down" instead of crashing the view).
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Whether the invite countdown's 1-second tick should be running for one
/// pane: only while its invite form is open AND a minted, unexpired code
/// is actually on screen — never merely because a code is retained in
/// state (`Message::IdentitySelect`'s toggle-off already clears it on
/// close, but this predicate stays defensive so the subscription and the
/// visible form can never disagree about whether it should be ticking).
fn invite_countdown_live(form_open: bool, expires_at: Option<i64>, now: i64) -> bool {
    form_open && expires_at.is_some_and(|expires_at| now < expires_at)
}

/// One device row: `fingerprint · granted <entry-id-prefix> · active`, or
/// `· retired <iso-date>`. Never says anything else — a retired grant is
/// not the same as a removed member (`docs/adr/0035`).
fn device_line(grant: &KeyGrantDto) -> String {
    let prefix = truncate(&grant.granted_by, 8);
    match grant.retired_at {
        Some(ts) => format!(
            "{} · granted {prefix} · retired {}",
            grant.fingerprint,
            iso_date(ts)
        ),
        None => format!("{} · granted {prefix} · active", grant.fingerprint),
    }
}

/// One member's device-count summary: `N active device(s)`, or `no active
/// devices` when every grant is retired — deliberately never says
/// "removed": a revoked member stays in the party (`docs/adr/0035`).
fn member_summary(member: &KeyMemberDto) -> String {
    let active = member
        .devices
        .iter()
        .filter(|g| g.retired_at.is_none())
        .count();
    match active {
        0 => "no active devices".to_string(),
        1 => "1 active device".to_string(),
        n => format!("{n} active devices"),
    }
}

/// A minted invite's countdown, from its `expires_at` and the current
/// wall clock (both epoch millis) — `"expires in M:SS"`, or `"expired"`
/// at or past zero. Split from the wall-clock read so it's testable
/// without mocking time.
fn countdown(expires_at: i64, now: i64) -> String {
    let remaining_secs = (expires_at - now).div_euclid(1000);
    if remaining_secs <= 0 {
        return "expired".to_string();
    }
    format!(
        "expires in {}:{:02}",
        remaining_secs / 60,
        remaining_secs % 60
    )
}

/// Parse the annotation composer's "lines" field: `"12"` (a single line) or
/// `"12-14"` (an inclusive range). Pure delegation to [`Span::new`], which
/// already enforces 1-indexing and a non-inverted range — this only splits
/// the string and parses the two halves, never re-implementing those
/// checks. `None` for anything that doesn't parse as one or two `u32`s
/// either side of a single `-`, or that `Span::new` then rejects (`"0"`,
/// `"9-3"`).
fn parse_span(s: &str) -> Option<Span> {
    let s = s.trim();
    let (start, end) = match s.split_once('-') {
        Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
        None => {
            let n: u32 = s.parse().ok()?;
            (n, n)
        }
    };
    Span::new(start, end).ok()
}

/// The [`Member`] to author an annotation as, resolved from the CHANNEL'S OWN
/// ROSTER (`keys.json`) by the email the live socket actually authenticated as.
///
/// Neither the display name nor the kind may come from this machine's git
/// identity. Dogfooding this composer produced an annotation reading
/// `{"display_name":"Dan Cieslak","email":"omp@oh-my-pi.dev","kind":"Human"}`
/// — the operator's name and the wrong kind stapled to the agent's address,
/// because the name was read from `settings.identity` and the kind was
/// hardcoded `human`. The record then misattributes a *signed* claim, which is
/// the failure `b10ffdc6` is about: an agent must author as itself.
///
/// An email absent from the roster falls back to the email as its own display
/// name and `agent` — the conservative choice, since a human is only ever
/// asserted when the roster says so.
fn author_for(keys: Option<&KeysDto>, email: &str) -> Member {
    let member = keys
        .into_iter()
        .flat_map(|keys| keys.members.iter())
        .find(|candidate| candidate.email == email);
    match member {
        Some(member) if member.kind == "human" => Member::human(member.display_name.clone(), email),
        Some(member) => Member::agent(member.display_name.clone(), email),
        None => Member::agent(email, email),
    }
}

/// What an artifact entry actually IS, taken from the `kind: ` prefix the host
/// writes into its summary (`diff: …`, `memo: …`, `log: …`,
/// `live-snapshot: …`).
///
/// The card used to be badged with the bare word `artifact`, which is what made
/// a diff unfindable: a reviewer scrolling a session record had no way to tell
/// which anonymous grey box held the code (ledger `532826c2`). Falls back to
/// `artifact` when there is no recognisable prefix, so an unknown kind is never
/// mislabelled as something it is not.
fn artifact_label(summary: &str) -> &str {
    let Some((prefix, _)) = summary.split_once(':') else {
        return "artifact";
    };
    let prefix = prefix.trim();
    match prefix {
        "diff" | "memo" | "log" | "live-snapshot" => prefix,
        // A prefix with spaces is prose that happens to contain a colon, not a
        // kind — "I'll make both edits: …" must not become a badge.
        _ => "artifact",
    }
}

/// The newest diff artifact belonging to `session`, if any — the one a reviewer
/// opening a session almost always wants to look at.
///
/// Entries arrive in timeline order, so the last match is the newest. Used to
/// expand it automatically: a reviewer had to pick the channel, find the
/// session, scroll a long record, recognise an anonymous card and click "show
/// content" before the gesture could start (ledger `532826c2`), and this
/// deletes the last three of those.
fn newest_diff_artifact<'a>(entries: &'a [EntryDto], session: &str) -> Option<&'a EntryDto> {
    entries.iter().rfind(|entry| {
        entry.kind == "artifact"
            && entry.target.as_deref() == Some(session)
            && artifact_label(&entry.summary) == "diff"
    })
}

/// The session's newest diff, rendered as the pane's primary object rather than
/// as one collapsed card among many (`532826c2`: "the gesture is fine; reaching
/// the code is the problem").
///
/// It carries no collapse control, and its card is omitted from the side record
/// so the same diff is never on screen twice. Content normally arrives via the
/// load-time auto-expand; when it has not (a refresh that raced it, or a
/// collapse performed before this arrangement put the diff here) the panel
/// offers a button rather than a stuck spinner, so it can never be dead.
fn code_panel<'a>(
    id: pane_grid::Pane,
    pane: &'a Pane,
    artifact: &'a EntryDto,
    aim: Option<Aim<'a>>,
) -> Element<'a, Message> {
    let head = row![
        badge("diff", kind_color("artifact")),
        text(artifact.summary.clone()).size(12).color(MUTED),
    ]
    .spacing(8)
    .align_y(Center);
    let body: Element<Message> = match pane.artifacts.get(&artifact.id) {
        Some(ArtifactContent::Loaded {
            format,
            body,
            md,
            digest,
        }) => artifact_body(id, &artifact.id, digest, format, body, md.as_deref(), aim),
        Some(ArtifactContent::Loading) => text("loading the diff…").size(11).color(MUTED).into(),
        Some(ArtifactContent::Error(err)) => text(format!("⚠ {err}")).size(11).color(RED).into(),
        None => button(text("show the diff").size(11))
            .on_press(Message::ToggleArtifact(id, artifact.id.clone()))
            .padding(6)
            .into(),
    };
    column![head, scrollable(body).height(Fill)]
        .spacing(6)
        .into()
}

fn chip_style(color: Color, active: bool) -> button::Style {
    button::Style {
        background: Some(Background::Color(if active {
            color
        } else {
            Color { a: 0.18, ..color }
        })),
        text_color: if active {
            Color::from_rgb(0.12, 0.12, 0.18)
        } else {
            TEXT
        },
        border: Border {
            color,
            width: 1.0,
            radius: 10.0.into(),
        },
        ..button::Style::default()
    }
}

/// One history entry: a node on a left rail (git-log style) beside its card.
/// The rail line fills the row height; with zero column spacing the nodes link
/// into a continuous history.
#[allow(clippy::too_many_arguments)]
fn timeline_entry<'a>(
    id: pane_grid::Pane,
    entry: &'a EntryDto,
    highlighted: bool,
    draft: &'a str,
    error: Option<&'a str>,
    pending: bool,
    artifact: Option<&'a ArtifactContent>,
    summary_md: Option<&'a [markdown::Item]>,
    aim: Option<Aim<'a>>,
) -> Element<'a, Message> {
    row![
        rail(kind_color(&entry.kind)),
        entry_card(
            id,
            entry,
            highlighted,
            draft,
            error,
            pending,
            artifact,
            summary_md,
            aim
        )
    ]
    .spacing(10)
    .into()
}

/// The (affirm, decline) verification acts available on an entry, if it's still
/// open: a provisional assertion → ratify/park; a pending proposal →
/// approve/reject. Resolved entries return `None` (no buttons).
fn entry_acts(entry: &EntryDto) -> Option<(&'static str, &'static str)> {
    match (entry.kind.as_str(), entry.status.as_deref()) {
        ("assertion", Some("provisional")) => Some(("ratify", "park")),
        ("proposal", Some("pending")) => Some(("approve", "reject")),
        _ => None,
    }
}

/// The left rail for one entry: a coloured node dot at the card's top. (A
/// continuous connecting line needs a `Fill` height, which Iced forbids inside
/// a scrollable — dots-only still reads as a node history.)
fn rail(color: Color) -> Element<'static, Message> {
    column![Space::new().height(6), dot(color)]
        .align_x(Center)
        .width(Length::Fixed(18.0))
        .into()
}

/// A small filled circle (a history node).
fn dot(color: Color) -> Element<'static, Message> {
    container(Space::new())
        .width(Length::Fixed(11.0))
        .height(Length::Fixed(11.0))
        .style(move |_theme| container::Style {
            background: Some(Background::Color(color)),
            border: Border {
                radius: 6.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

/// Which authorship badges an entry card shows: `(show_unrecognized,
/// show_unverified)`. Pure — extracted from `entry_card` so the suppression
/// rule is testable without a renderer. `unverified` is suppressed whenever
/// `unrecognized` is set: that badge already signals distrust louder
/// (mirrors the web's reasoning, `crates/junto/src/render.rs`).
fn entry_badges(entry: &EntryDto) -> (bool, bool) {
    let show_unrecognized = entry.unrecognized;
    let show_unverified = entry.unverified && !entry.unrecognized;
    (show_unrecognized, show_unverified)
}

/// One timeline entry as a card: a colour-coded kind badge + author + status,
/// over the summary text.
#[allow(clippy::too_many_arguments)]
fn entry_card<'a>(
    id: pane_grid::Pane,
    entry: &'a EntryDto,
    highlighted: bool,
    draft: &'a str,
    error: Option<&'a str>,
    pending: bool,
    artifact: Option<&'a ArtifactContent>,
    summary_md: Option<&'a [markdown::Item]>,
    aim: Option<Aim<'a>>,
) -> Element<'a, Message> {
    let accent = kind_color(&entry.kind);
    // An artifact is badged with WHAT IT IS (`diff`, `memo`, `log`), not the
    // generic word `artifact` — otherwise every artifact in a session record
    // looks identical and the diff is unfindable (ledger `532826c2`).
    let kind_label = if entry.kind == "artifact" {
        artifact_label(&entry.summary)
    } else {
        &entry.kind
    };
    let mut head = row![
        badge(kind_label, accent),
        text(entry.author.clone()).size(11).color(MUTED)
    ]
    .spacing(8);
    if let Some(status) = &entry.status {
        head = head.push(badge(status, status_color(status)));
    }
    let (show_unrecognized, show_unverified) = entry_badges(entry);
    if show_unrecognized {
        head = head.push(badge("unrecognized", RED));
    }
    if show_unverified {
        head = head.push(badge("unverified", YELLOW));
    }
    head = head.push(Space::new().width(Fill));
    head = head.push(copy_button(entry.summary.clone()));

    // The body: a session memo renders as Markdown; everything else is plain.
    let body: Element<Message> = if let Some(items) = summary_md {
        markdown::view(items, Theme::CatppuccinMocha).map(|url| Message::OpenUrl(url.to_string()))
    } else {
        text(entry.summary.clone()).size(13).color(TEXT).into()
    };
    let mut card = column![head, body].spacing(6);

    // Inline verification acts on open assertions/proposals. The decision
    // frame's pre-baked options come first as one-click buttons (each carries
    // its drafted rationale — adopt it without typing, `docs/adr/0019`), then a
    // free-text fallback box for a custom rationale. Acting refetches the pane,
    // so the controls clear once the entry resolves.
    if let Some((affirm, decline)) = entry_acts(entry) {
        let entry_id = entry.id.clone();
        let mut acts = column![].spacing(6);

        // Pre-baked frame options coherent with this entry's two acts. Stacked
        // full-width so they stay readable in a narrow pane (no horizontal
        // overflow); the act is tagged on the right of each row.
        let mut options = column![].spacing(4);
        let mut has_options = false;
        for opt in &entry.frame {
            if opt.act != affirm && opt.act != decline {
                continue;
            }
            has_options = true;
            let affirmative = opt.act == affirm;
            let color = if affirmative { GREEN } else { RED };
            let inner = row![
                text(opt.label.clone()).size(11),
                Space::new().width(Fill),
                text(opt.act.clone()).size(10),
            ]
            .spacing(8)
            .align_y(Center);
            let mut opt_btn = button(inner)
                .width(Fill)
                .padding([4, 10])
                .style(move |_t, _s| chip_style(color, affirmative));
            if !pending {
                opt_btn = opt_btn.on_press(Message::Act(
                    id,
                    entry_id.clone(),
                    opt.act.clone(),
                    opt.rationale.clone(),
                ));
            }
            options = options.push(opt_btn);
        }
        if has_options {
            acts = acts.push(options);
        }

        // Free-text fallback: type a custom rationale, then affirm/decline.
        let has_rationale = !draft.trim().is_empty();
        let rationale = text_input("custom rationale…", draft)
            .on_input({
                let entry_id = entry_id.clone();
                move |v| Message::ActRationaleChanged(id, entry_id.clone(), v)
            })
            .size(12)
            .padding(6);
        let mut affirm_btn = button(text(affirm).size(11))
            .padding([3, 10])
            .style(|_t, _s| chip_style(GREEN, true));
        let mut decline_btn = button(text(decline).size(11))
            .padding([3, 10])
            .style(|_t, _s| chip_style(RED, false));
        if has_rationale && !pending {
            affirm_btn = affirm_btn.on_press(Message::Act(
                id,
                entry_id.clone(),
                affirm.to_string(),
                draft.to_string(),
            ));
            decline_btn = decline_btn.on_press(Message::Act(
                id,
                entry_id.clone(),
                decline.to_string(),
                draft.to_string(),
            ));
        }
        acts = acts.push(
            row![rationale, affirm_btn, decline_btn]
                .spacing(6)
                .align_y(Center),
        );
        if pending {
            acts = acts.push(
                text("recording… (writing to the ledger)")
                    .size(11)
                    .color(YELLOW),
            );
        } else if let Some(err) = error {
            acts = acts.push(text(format!("⚠ {err}")).size(11).color(RED));
        }
        card = card.push(acts);
    }

    // Artifacts: a toggle that lazy-loads the diff/memo/log content inline.
    if entry.kind == "artifact" {
        let expanded = artifact.is_some();
        let toggle_label = if expanded {
            "hide content ▾"
        } else {
            "show content ▸"
        };
        card = card.push(
            button(text(toggle_label).size(11))
                .on_press(Message::ToggleArtifact(id, entry.id.clone()))
                .padding([2, 8])
                .style(|_t, _s| chip_style(TEAL, false)),
        );
        match artifact {
            Some(ArtifactContent::Loading) => {
                card = card.push(text("loading…").size(11).color(MUTED));
            }
            Some(ArtifactContent::Error(err)) => {
                card = card.push(text(format!("⚠ {err}")).size(11).color(RED));
            }
            Some(ArtifactContent::Loaded {
                format,
                body,
                md,
                digest,
            }) => {
                card = card.push(
                    row![Space::new().width(Fill), copy_button(body.clone())].align_y(Center),
                );
                card = card.push(artifact_body(
                    id,
                    &entry.id,
                    digest,
                    format,
                    body,
                    md.as_deref(),
                    aim,
                ));
            }
            None => {}
        }
    }

    let (border_color, border_width) = if highlighted {
        (YELLOW, 2.0)
    } else {
        (BORDER, 1.0)
    };
    container(card)
        .padding(10)
        .width(Fill)
        .style(move |_theme| container::Style {
            background: Some(Background::Color(SURFACE)),
            border: Border {
                color: border_color,
                width: border_width,
                radius: 6.0.into(),
            },
            text_color: Some(TEXT),
            ..container::Style::default()
        })
        .into()
}

/// Where the annotation composer is currently aimed, threaded down to the diff
/// renderer. `Some` only while the composer is live (`Pane::annotate_tx`), which
/// is the same condition that makes pointing meaningful at all: aiming inputs
/// that are not on screen would be a click that appears to do nothing.
#[derive(Clone, Copy)]
struct Aim<'a> {
    /// The composer's current `path`, untrimmed (as typed). Empty when the aim
    /// is at record content or the live stream rather than at a file.
    path: &'a str,
    /// The record entry the composer is aimed at, when pointing at content
    /// already in the record (`Pane::annotate_record`). Mutually exclusive with
    /// a non-empty `path`.
    record: Option<&'a str>,
    /// The composer's current `lines`, parsed — `None` while it is empty or
    /// malformed.
    span: Option<Span>,
    /// The new-file line the floating comment panel hangs from — the span's
    /// END, i.e. the row most recently clicked. `None` when no panel is shown.
    /// Resolved by `popup_anchor`, which refuses a row that is not actually
    /// rendered, so the panel can never be aimed at nothing.
    popup_at: Option<u32>,
    /// The row under the cursor as `(target key, line)`, so exactly one row
    /// paints its hover. Tracked in state because these rows are `mouse_area`s,
    /// which report no hover status of their own.
    hover: Option<(&'a str, u32)>,
    /// The pane, so the row owning the panel can build it in place.
    pane: &'a Pane,
}

/// The most rows of a diff artifact that are ever rendered. `popup_anchor` uses
/// the same bound, so it never promises a panel on a row past the cut-off.
const MAX_DIFF_ROWS: usize = 500;

/// The new-file line the floating comment panel should hang from: the aimed
/// span's end, but only when an expanded diff artifact in this pane actually
/// renders that row.
///
/// The "actually renders" check is what stops the reviewer being left with no
/// composer at all: the bottom composer hides while the panel is up, so
/// promising a panel that has no anchor would remove the only way to comment.
fn popup_anchor(pane: &Pane) -> Option<u32> {
    // A record aim needs no rendering check: the aimed entry IS the artifact
    // being rendered, and the row was clicked to set the span in the first
    // place, so the line necessarily exists in what is on screen.
    if pane.annotate_record.is_some() {
        return parse_span(&pane.annotate_lines).map(|span| span.end);
    }
    let diffs = pane.artifacts.values().filter_map(|content| match content {
        ArtifactContent::Loaded {
            format, body, md, ..
        } if format == "diff" && md.is_none() => Some(body.as_str()),
        _ => None,
    });
    popup_anchor_line(
        &pane.annotate_path,
        parse_span(&pane.annotate_lines),
        diffs,
        MAX_DIFF_ROWS,
    )
}

/// Render an artifact's content inline: a diff gets per-line add/remove/hunk
/// colour; anything else is shown verbatim. Monospace; long artifacts are
/// truncated (the web view holds the full text).
///
/// With an `aim`, EVERY rendered line is a click target (ledger `9d0ea0b6`:
/// pointing that works only on the added lines of a committed diff violates
/// least surprise). A diff row occupying a line of the new file aims a
/// `CodeAnchor` at that file line (`pointing::diff_row_targets`); every other
/// line — a header, a hunk marker, a removed row, a whole memo or log — aims a
/// `RecordAnchor` at that line of this artifact's stored content, which is
/// immutable and digest-addressable. Rows inside the aimed span are lit.
///
/// A Markdown artifact renders formatted while READING and as pointable
/// monospace lines while commenting, because a line is what an anchor can name:
/// rendered Markdown has no stable line to point at, and refusing to point at
/// memos at all is the defect this replaces.
fn artifact_body<'a>(
    id: pane_grid::Pane,
    entry: &'a str,
    digest: &'a str,
    format: &str,
    body: &'a str,
    md: Option<&'a [markdown::Item]>,
    aim: Option<Aim<'a>>,
) -> Element<'a, Message> {
    if let Some(items) = md.filter(|_| aim.is_none()) {
        return container(
            markdown::view(items, Theme::CatppuccinMocha)
                .map(|url| Message::OpenUrl(url.to_string())),
        )
        .padding(8)
        .width(Fill)
        .style(|_theme| container::Style {
            background: Some(Background::Color(Color {
                a: 0.6,
                ..Color::from_rgb(0.067, 0.067, 0.106)
            })),
            border: Border {
                color: BORDER,
                width: 1.0,
                radius: 4.0.into(),
            },
            ..container::Style::default()
        })
        .into();
    }
    let lines: Vec<&str> = body.lines().collect();
    let is_diff = format == "diff";
    // One target per rendered row, in row order — indexed by the same `i` the
    // loop below draws with, so a click can never land on a different line than
    // the one under the cursor. Skipped entirely when there is nothing to aim.
    let targets = match aim {
        Some(_) if is_diff => diff_row_targets(body),
        _ => Vec::new(),
    };
    let mut col = column![].spacing(1);
    for (i, line) in lines.iter().enumerate().take(MAX_DIFF_ROWS) {
        let color = if is_diff { diff_line_color(line) } else { TEXT };
        let Some(aim) = aim else {
            col = col.push(
                text((*line).to_string())
                    .font(iced::Font::MONOSPACE)
                    .size(12)
                    .color(color),
            );
            continue;
        };
        // `line_no` is this row's own 1-indexed position in the stored content,
        // which is what a `RecordAnchor` names; `file_line` is the position in
        // the NEW file, which is what a `CodeAnchor` names. They are different
        // numbers and must never be swapped.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "capped at MAX_DIFF_ROWS, far below u32::MAX"
        )]
        let line_no = (i + 1) as u32;
        // A diff row that occupies a real new-file line makes the stronger
        // claim; everything else points at this artifact's stored text.
        let (target, row_line) = match targets.get(i).copied().flatten() {
            Some((path, file_line)) => (AnchorTarget::Code(path.to_string()), file_line),
            None => (
                AnchorTarget::Record(entry.to_string(), digest.to_string()),
                line_no,
            ),
        };
        // Aimed HERE means the composer is pointed at this row's own target, so
        // `lit` and the panel can never appear on a row belonging to a different
        // file or artifact.
        let aimed_here = match &target {
            AnchorTarget::Code(path) => aim.record.is_none() && aim.path.trim() == path,
            AnchorTarget::Record(record, _) => aim.record == Some(record.as_str()),
        };
        let lit = aimed_here
            && aim
                .span
                .is_some_and(|s| s.start <= row_line && row_line <= s.end);
        let hovered = aim.hover == Some((target.key(), row_line));
        let row = anchor_row(id, line, color, target, row_line, lit, hovered);
        // The comment surface hangs off the row it is about, rather than sitting
        // 800px away at the bottom of the pane (ledger `02ff24be`). Exactly one
        // row in the pane carries it: the aimed span's end.
        if aimed_here && aim.popup_at == Some(row_line) {
            col = col.push(Popover::new(row, Some(annotate_popup(id, aim.pane))));
        } else {
            col = col.push(row);
        }
    }
    if lines.len() > MAX_DIFF_ROWS {
        col = col.push(
            text(format!(
                "… ({} more lines — open in the web view for the full content)",
                lines.len() - MAX_DIFF_ROWS
            ))
            .size(11)
            .color(MUTED),
        );
    }
    // A release outside any row would otherwise leave `drag_from` set, and the
    // next hover — with no button down — would silently extend the selection.
    // Leaving the artifact ends the gesture, which bounds that to the one case
    // a `mouse_area` cannot observe.
    let body: Element<Message> = if aim.is_some() {
        mouse_area(col)
            .on_release(Message::AnchorRelease(id))
            .on_exit(Message::AnchorRelease(id))
            .into()
    } else {
        col.into()
    };
    container(body)
        .padding(8)
        .width(Fill)
        .style(|_theme| container::Style {
            background: Some(Background::Color(Color {
                a: 0.6,
                ..Color::from_rgb(0.067, 0.067, 0.106) // --bg #11111b
            })),
            border: Border {
                color: BORDER,
                width: 1.0,
                radius: 4.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// Per-line colour for a unified diff (matches the web's `render_diff`).
fn diff_line_color(line: &str) -> Color {
    if line.starts_with("@@") {
        MAUVE
    } else if line.starts_with("+++")
        || line.starts_with("---")
        || line.starts_with("diff ")
        || line.starts_with("index ")
        || line.starts_with("old mode")
        || line.starts_with("new mode")
    {
        MUTED
    } else if line.starts_with('+') {
        GREEN
    } else if line.starts_with('-') {
        RED
    } else {
        TEXT
    }
}

/// One anchorable row: a full-width target that stays invisible until hovered,
/// so a 500-row diff reads as a diff and a memo reads as prose rather than as
/// hundreds of buttons.
///
/// A `mouse_area`, deliberately not a `button`. A button reports no drag state
/// at all, which is why the first version of this gesture was "click a line,
/// then click a lower line" — directional, undiscoverable, and Dan's complaint
/// (2026-08-23). `on_press`/`on_enter`/`on_release` give press-drag-release
/// selection instead, at the cost of painting hover by hand (`hovered`).
///
/// `lit` paints rows already inside the aimed span, which is the only feedback
/// that a range was picked. Colour distinguishes the claim: mauve for a
/// `CodeAnchor` at a file line, teal for a `RecordAnchor` at stored content, so
/// a reviewer can tell which one a drag is making without reading the composer.
fn anchor_row<'a>(
    id: pane_grid::Pane,
    body: &str,
    color: Color,
    target: AnchorTarget,
    line: u32,
    lit: bool,
    hovered: bool,
) -> Element<'a, Message> {
    let paint = match target {
        AnchorTarget::Code(_) => MAUVE,
        AnchorTarget::Record(..) => TEAL,
    };
    let row = container(
        text(body.to_string())
            .font(iced::Font::MONOSPACE)
            .size(12)
            .color(color),
    )
    .width(Fill)
    .padding([0, 4])
    .style(move |_theme| container::Style {
        background: (lit || hovered).then_some(Background::Color(Color {
            a: if lit { 0.28 } else { 0.14 },
            ..paint
        })),
        text_color: Some(color),
        border: Border {
            radius: 3.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    });
    mouse_area(row)
        .on_press(Message::AnchorPress(id, target.clone(), line))
        .on_enter(Message::AnchorOver(id, target, line))
        .on_release(Message::AnchorRelease(id))
        .into()
}

/// A small "copy" button that writes `text` to the clipboard (Iced static text
/// isn't mouse-selectable, so copy buttons are how you grab content).
fn copy_button(text_to_copy: String) -> Element<'static, Message> {
    button(text("copy").size(10))
        .on_press(Message::Copy(text_to_copy))
        .padding([1, 6])
        .style(|_t, _s| chip_style(MUTED, false))
        .into()
}

/// Watcher presence for the session header: an initials avatar per watcher
/// (capped, with a `+N` overflow) plus the count, so "someone else is looking
/// at this right now" is visible at a glance instead of being a line of emails
/// buried in the feed. Fed by `Message::Watchers`, which the live websocket's
/// `Ephemeral` presence frames already deliver.
fn watchers_chip(watchers: &[String]) -> Element<'static, Message> {
    const MAX_AVATARS: usize = 4;
    let mut chip = row![].spacing(3).align_y(Center);
    for email in watchers.iter().take(MAX_AVATARS) {
        chip = chip.push(avatar(email));
    }
    if watchers.len() > MAX_AVATARS {
        chip = chip.push(
            text(format!("+{}", watchers.len() - MAX_AVATARS))
                .size(10)
                .color(MUTED),
        );
    }
    chip.push(
        text(format!("{} watching", watchers.len()))
            .size(11)
            .color(TEAL),
    )
    .into()
}

/// One watcher's initials in a filled pill, tinted deterministically from their
/// email so two watchers are told apart at a glance. The initials are a cue,
/// not an identity — hovering shows the full email, which is why moving
/// presence into the header loses nothing.
fn avatar(email: &str) -> Element<'static, Message> {
    const TINTS: [Color; 5] = [BLUE, TEAL, MAUVE, GREEN, YELLOW];
    let tint = TINTS[email_tint(email) as usize % TINTS.len()];
    let pill = container(
        text(watcher_initials(email))
            .size(9)
            .color(Color::from_rgb(0.12, 0.12, 0.18)),
    )
    .padding([1, 4])
    .style(move |_theme| container::Style {
        background: Some(Background::Color(tint)),
        border: Border {
            radius: 7.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    });
    tooltip(
        pill,
        container(text(email.to_string()).size(11).color(TEXT))
            .padding([2, 6])
            .style(|_theme| container::Style {
                background: Some(Background::Color(SURFACE)),
                border: Border {
                    color: BORDER,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..container::Style::default()
            }),
        tooltip::Position::Bottom,
    )
    .into()
}

/// FNV-1a over an email, so a watcher keeps the same avatar tint across frames
/// and across machines (a colour that shuffled every repaint would be noise).
fn email_tint(email: &str) -> u32 {
    email.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    })
}

fn badge(label: &str, color: Color) -> Element<'static, Message> {
    container(
        text(label.to_string())
            .size(11)
            .color(Color::from_rgb(0.12, 0.12, 0.18)),
    )
    .padding([2, 7])
    .style(move |_theme| container::Style {
        background: Some(Background::Color(color)),
        border: Border {
            radius: 10.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    })
    .into()
}

fn kind_color(kind: &str) -> Color {
    match kind {
        "assertion" => BLUE,
        "proposal" => YELLOW,
        "session" => TEAL,
        "act" => GREEN,
        "lineage" => MAUVE,
        _ => MUTED,
    }
}

fn status_color(status: &str) -> Color {
    match status {
        "ratified" | "approved" | "done" => GREEN,
        "parked" | "rejected" | "error" | "superseded" => RED,
        "provisional" | "pending" | "working" | "blocked" | "awaitingapproval" => YELLOW,
        _ => MUTED,
    }
}

impl Pane {
    /// Aim the composer at `target` over `lines`, keeping the three anchor kinds
    /// mutually exclusive.
    ///
    /// Every pointing gesture goes through here so the exclusivity is stated
    /// once: an aim left half-set — a stale `annotate_path` beside a fresh
    /// `annotate_record`, say — would make `AnnotateSubmit` sign the wrong kind
    /// of claim, and that is a corrupt record rather than a cosmetic bug.
    fn aim_at(&mut self, target: &AnchorTarget, lines: String) {
        self.annotate_lines = lines;
        self.annotate_op = None;
        match target {
            AnchorTarget::Code(path) => {
                self.annotate_path = path.clone();
                self.annotate_record = None;
            }
            AnchorTarget::Record(entry, digest) => {
                // Record content is not a file: the empty path is what tells
                // `AnnotateSubmit` this is not a `CodeAnchor`.
                self.annotate_path.clear();
                self.annotate_record = Some((entry.clone(), digest.clone()));
            }
        }
    }

    /// The key of whatever the composer is currently aimed at, or `None` when it
    /// is aimed at the live stream (which has no rows to drag across).
    fn aimed_key(&self) -> Option<&str> {
        if let Some((entry, _)) = &self.annotate_record {
            return Some(entry.as_str());
        }
        Some(self.annotate_path.trim()).filter(|path| !path.is_empty())
    }

    fn loading(channel: &str) -> Self {
        Pane {
            channel: channel.to_string(),
            content: Content::Loading,
            watched: None,
            streaming: false,
            feed: Vec::new(),
            remote: None,
            watch_email: String::new(),
            watchers: Vec::new(),
            annotate_tx: None,
            annotate_email: None,
            conversation_len: 0,
            annotate_op: None,
            annotate_record: None,
            drag_from: None,
            hover: None,
            worktree_commit: None,
            annotate_path: String::new(),
            annotate_lines: String::new(),
            annotate_body: String::new(),
            annotate_urgent: false,
            launch_intent: String::new(),
            steer_text: String::new(),
            launch_agent: None,
            launch_outcome: false,
            launch_workspace: String::new(),
            launching: false,
            launch_error: None,
            highlight_entry: None,
            act_drafts: HashMap::new(),
            act_errors: HashMap::new(),
            act_pending: HashSet::new(),
            scroll_id: iced::widget::Id::unique(),
            artifacts: HashMap::new(),
            launch_expanded: false,
            show_full_history: false,
            entry_md: HashMap::new(),
            watch_newest: false,
            stream_nonce: 0,
            lifecycle: None,
            lifecycle_text: String::new(),
            lifecycle_target: String::new(),
            lifecycle_pending: false,
            lifecycle_error: None,
            brief_md: None,
            brief_text: None,
            keys: None,
            keys_error: None,
            auto_expanded: false,
            members_open: false,
            identity_form: None,
            identity_pending: false,
            identity_error: None,
            invite_minted: None,
            redeem_preview: None,
            redeem_outcomes: Vec::new(),
            identity_member: String::new(),
            identity_channels: Vec::new(),
            identity_paste: String::new(),
            identity_kind: String::new(),
            identity_rationale: String::new(),
            identity_notice: None,
        }
    }

    /// This pane's effective REST/websocket base URL: its remote override
    /// if set, else the local `HOST`. The one place that distinction is
    /// made — every fetch/post call and the live subscription read this
    /// instead of touching `remote/HOST` directly.
    fn base(&self) -> &str {
        self.remote.as_deref().unwrap_or(HOST)
    }

    /// This channel's sessions from the pane's already-fetched `view.json`
    /// data — empty while `content` is still `Loading` or `Error`, never a
    /// panic. The blade's Sessions view reads through this rather than
    /// matching on `Content` itself.
    fn session_list(&self) -> &[SessionDto] {
        match &self.content {
            Content::Loaded(dto) => &dto.sessions,
            Content::Loading | Content::Error(_) => &[],
        }
    }

    /// This channel's artifact entries (`entry.kind == "artifact"`) from the
    /// pane's already-fetched `view.json` data — empty while `content` is
    /// still `Loading` or `Error`. Named `artifact_entries`, not `artifacts`,
    /// because that name already belongs to the expanded-inline-content
    /// cache (`Pane::artifacts`) and means something different.
    fn artifact_entries(&self) -> impl Iterator<Item = &EntryDto> {
        let entries: &[EntryDto] = match &self.content {
            Content::Loaded(dto) => &dto.entries,
            Content::Loading | Content::Error(_) => &[],
        };
        entries.iter().filter(|entry| entry.kind == "artifact")
    }
}

// ---- the branch graph: a horizontal time-axis lineage strip, matching the
// web's `lineage_strip` (newest on the right, log-scaled by age; each channel a
// track from its first to last activity; diverge/converge as connectors). ----

const ROWH: f32 = 24.0;
const TOP: f32 = 12.0;
const LABEL_W: f32 = 150.0;
/// Minimum drawn track length, so a short side-quest's diverge (at its start)
/// and converge (at its end) keep a visible horizontal gap.
const MIN_TRACK: f32 = 56.0;

#[derive(Clone)]
struct Track {
    name: String,
    row: usize,
    first_ms: i64,
    last_ms: i64,
    root: bool,
    /// This channel is currently open as a pane (highlighted in the graph).
    open: bool,
    /// Labelled points along the track: (timestamp, explanatory text).
    milestones: Vec<(i64, String)>,
}

#[derive(Clone)]
struct LineageCanvas {
    tracks: Vec<Track>,
    /// (parent_row, child_row, divergence_ms, is_diverge)
    links: Vec<(usize, usize, i64, bool)>,
    now_ms: i64,
    span_ms: i64,
    height: f32,
}

impl LineageCanvas {
    fn layout(graph: &LineageGraphDto, open: &HashSet<String>) -> Self {
        let mut is_child: HashSet<&str> = HashSet::new();
        for edge in &graph.edges {
            if edge.relation == "diverge" {
                is_child.insert(edge.to.as_str());
            }
        }

        // Stack tracks oldest-first (root spines near the top).
        let now_ms = graph
            .nodes
            .iter()
            .filter_map(|n| n.last_ms)
            .max()
            .unwrap_or(0);
        let min_ms = graph
            .nodes
            .iter()
            .filter_map(|n| n.first_ms)
            .min()
            .unwrap_or(0);

        let mut ordered: Vec<&GNode> = graph.nodes.iter().collect();
        ordered.sort_by_key(|n| n.first_ms.unwrap_or(min_ms));
        // The oldest root is the mainline spine — keep it at row 0 (the sticky
        // pinned ambient). Show the remaining tracks newest-first, so the most
        // recent channels appear at the top of the timeline.
        if ordered.len() > 1 {
            ordered[1..].reverse();
        }

        let mut row_of: HashMap<&str, usize> = HashMap::new();
        let mut tracks = Vec::new();
        for (row, n) in ordered.iter().enumerate() {
            row_of.insert(n.id.as_str(), row);
            tracks.push(Track {
                name: n.name.clone(),
                row,
                first_ms: n.first_ms.unwrap_or(min_ms),
                last_ms: n.last_ms.unwrap_or(now_ms),
                root: !is_child.contains(n.id.as_str()),
                open: open.contains(&n.name),
                milestones: n
                    .milestones
                    .iter()
                    .map(|m| (m.ms, m.label.clone()))
                    .collect(),
            });
        }
        let first_of: HashMap<&str, i64> = graph
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n.first_ms.unwrap_or(min_ms)))
            .collect();
        let last_of: HashMap<&str, i64> = graph
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n.last_ms.unwrap_or(now_ms)))
            .collect();

        let links = graph
            .edges
            .iter()
            .filter_map(|e| {
                let parent = *row_of.get(e.from.as_str())?;
                let child = *row_of.get(e.to.as_str())?;
                let diverge = e.relation == "diverge";
                // Diverge happens at the child's start; convergence happens at the
                // source's end (when the side-quest merged back).
                let at = if diverge {
                    *first_of.get(e.to.as_str())?
                } else {
                    *last_of.get(e.from.as_str())?
                };
                Some((parent, child, at, diverge))
            })
            .collect();

        let height = TOP * 2.0 + tracks.len() as f32 * ROWH;
        LineageCanvas {
            tracks,
            links,
            now_ms,
            span_ms: (now_ms - min_ms).max(1),
            height,
        }
    }

    /// Log-scaled age → x, newest on the right (matches the web's `strip_age_x`).
    fn x_of(&self, ms: i64, left: f32, right: f32) -> f32 {
        let age = (self.now_ms - ms).max(0) as f64;
        let frac = ((age + 1.0).ln() / (self.span_ms as f64 + 1.0).ln()).clamp(0.0, 1.0) as f32;
        right - frac * (right - left)
    }

    fn y_of(row: usize) -> f32 {
        TOP + row as f32 * ROWH + ROWH / 2.0
    }
}

impl canvas::Program<Message> for LineageCanvas {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let left = LABEL_W;
        let right = (bounds.width - 24.0).max(left + 60.0);
        let hover = cursor.position_in(bounds);
        let mut tooltip: Option<(Point, String)> = None;

        // Per-track x-range with a minimum length so diverge/converge keep a gap.
        let mut ranges = vec![(0.0_f32, 0.0_f32); self.tracks.len()];
        for t in &self.tracks {
            let x0 = self.x_of(t.first_ms, left, right);
            let x1 = self.x_of(t.last_ms, left, right).max(x0 + MIN_TRACK);
            ranges[t.row] = (x0, x1);
        }

        // Diverge/converge connectors: straight vertical links, distinguished by
        // style so they read even when close — diverge solid (mauve) anchored at
        // the child's start, converge dashed (green) anchored at the source's end.
        for (parent_row, child_row, _at_ms, diverge) in &self.links {
            let x = if *diverge {
                ranges[*child_row].0
            } else {
                ranges[*parent_row].1
            };
            let y0 = Self::y_of(*parent_row);
            let y1 = Self::y_of(*child_row);
            let color = if *diverge { MAUVE } else { GREEN };
            let base = Stroke::default().with_color(color).with_width(1.6);
            let stroke = if *diverge {
                base
            } else {
                canvas::Stroke {
                    line_dash: canvas::LineDash {
                        segments: &[4.0, 3.0],
                        offset: 0,
                    },
                    ..base
                }
            };
            frame.stroke(&Path::line(Point::new(x, y0), Point::new(x, y1)), stroke);
        }

        // Tracks: a horizontal line from first to last activity + an end cap,
        // plus a labelled dot per milestone (label shown on hover).
        for track in &self.tracks {
            let y = Self::y_of(track.row);
            let (x0, x1) = ranges[track.row];
            let base = if track.root { TEAL } else { MAUVE };
            let color = if track.open {
                base
            } else {
                Color { a: 0.45, ..base }
            };
            frame.stroke(
                &Path::line(Point::new(x0, y), Point::new(x1, y)),
                Stroke::default()
                    .with_color(color)
                    .with_width(if track.open { 3.0 } else { 2.0 }),
            );
            frame.fill(
                &Path::circle(Point::new(x1, y), if track.open { 5.0 } else { 4.0 }),
                color,
            );

            // Milestone points along the track (clamped onto the drawn span).
            for (ms, label) in &track.milestones {
                let mx = self.x_of(*ms, left, right).clamp(x0, x1);
                let dot = Point::new(mx, y);
                frame.fill(
                    &Path::circle(dot, 2.5),
                    Color {
                        a: if track.open { 0.95 } else { 0.55 },
                        ..TEXT
                    },
                );
                if let Some(h) = hover
                    && (h.x - mx).abs() < 5.0
                    && (h.y - y).abs() < 5.0
                {
                    tooltip = Some((dot, label.clone()));
                }
            }

            frame.fill_text(canvas::Text {
                content: truncate(&track.name, 20),
                position: Point::new(8.0, y - 8.0),
                color: if track.open {
                    TEXT
                } else {
                    Color { a: 0.7, ..TEXT }
                },
                size: 13.0.into(),
                ..canvas::Text::default()
            });
        }

        draw_tooltip(&mut frame, tooltip, bounds);
        vec![frame.into_geometry()]
    }
}

/// Draw a milestone hover tooltip (a labelled box near the hovered point).
fn draw_tooltip(frame: &mut Frame, tooltip: Option<(Point, String)>, bounds: Rectangle) {
    let Some((dot, label)) = tooltip else { return };
    let w = (label.chars().count() as f32 * 6.3 + 14.0).min(bounds.width - 8.0);
    let tx = (dot.x + 8.0).min(bounds.width - w - 4.0).max(4.0);
    let ty = (dot.y - 24.0).max(2.0);
    frame.fill(
        &Path::rectangle(Point::new(tx, ty), Size::new(w, 19.0)),
        Color { a: 0.97, ..SURFACE },
    );
    frame.fill_text(canvas::Text {
        content: label,
        position: Point::new(tx + 7.0, ty + 3.0),
        color: TEXT,
        size: 11.0.into(),
        ..canvas::Text::default()
    });
}

/// Fetch a channel's structured view from `base` (the pane's effective host,
/// `Pane::base`) into `pane`.
fn fetch(pane: pane_grid::Pane, base: String, channel: &str) -> Task<Message> {
    let url = format!("{base}/channels/{channel}/view.json");
    Task::perform(
        async move {
            let response = reqwest::get(&url).await.map_err(|e| e.to_string())?;
            response
                .json::<ChannelDto>()
                .await
                .map_err(|e| e.to_string())
        },
        move |result| Message::Fetched(pane, result),
    )
}

/// Fetch a channel's key roster (`keys.json`) — the members-and-devices
/// disclosure's data source (device-key-enrollment plan, Task 13).
fn fetch_keys(pane: pane_grid::Pane, base: String, channel: &str) -> Task<Message> {
    let url = format!("{base}/channels/{channel}/keys.json");
    Task::perform(
        async move {
            let response = reqwest::get(&url).await.map_err(|e| e.to_string())?;
            response.json::<KeysDto>().await.map_err(|e| e.to_string())
        },
        move |result| Message::KeysFetched(pane, result),
    )
}

/// Fetch the whole lineage DAG for the always-visible top branch graph.
fn fetch_lineage_graph() -> Task<Message> {
    let url = format!("{HOST}/lineage.json");
    Task::perform(
        async move {
            match reqwest::get(&url).await {
                Ok(response) => response.json::<LineageGraphDto>().await.ok(),
                Err(_) => None,
            }
        },
        Message::LineageGraphLoaded,
    )
}

/// Fetch the cross-channel focus board ("needs you" items).
fn fetch_focus() -> Task<Message> {
    let url = format!("{HOST}/focus.json");
    Task::perform(
        async move {
            match reqwest::get(&url).await {
                Ok(response) => response.json::<Vec<FocusItem>>().await.unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        },
        Message::FocusLoaded,
    )
}

/// Fetch one artifact's raw content + format for inline rendering.
fn fetch_artifact(
    pane: pane_grid::Pane,
    base: String,
    channel: String,
    artifact: String,
) -> Task<Message> {
    let url = format!("{base}/channels/{channel}/artifacts/{artifact}/content.json");
    let id = artifact.clone();
    Task::perform(
        async move {
            match reqwest::get(&url).await {
                Ok(resp) if resp.status().is_success() => {
                    resp.json::<ArtifactDto>().await.map_err(|e| e.to_string())
                }
                Ok(resp) => Err(format!("content unavailable ({})", resp.status())),
                Err(err) => Err(format!("request failed: {err}")),
            }
        },
        move |result| Message::ArtifactLoaded(pane, id.clone(), result),
    )
}

/// Fetch machine settings for the settings view.
fn fetch_settings() -> Task<Message> {
    let url = format!("{HOST}/settings.json");
    Task::perform(
        async move {
            match reqwest::get(&url).await {
                Ok(resp) if resp.status().is_success() => resp.json::<SettingsDto>().await.ok(),
                _ => None,
            }
        },
        Message::SettingsLoaded,
    )
}

/// POST a repo setup (register a home substrate — the GUI `junto init`).
fn post_setup_repo(path: String, channel: String) -> Task<Message> {
    let url = format!("{HOST}/repos");
    Task::perform(
        async move {
            let mut form = vec![("path", path)];
            if !channel.is_empty() {
                form.push(("channel", channel));
            }
            simple_post_result(&url, &form, "setup").await
        },
        Message::RepoSetupDone,
    )
}

/// POST an agent create/edit (`/agents`). An empty `slug` creates; a set one
/// edits. MCP servers / skills / plugins go as repeated keys (the host zips
/// `mcp_name`+`mcp_url` and collects `skill` / `plugin_path`).
#[allow(clippy::too_many_arguments)]
fn post_save_agent(
    slug: Option<String>,
    name: String,
    harness: String,
    role: String,
    model: String,
    mcp: Vec<(String, String)>,
    skills: Vec<String>,
    plugins: Vec<String>,
) -> Task<Message> {
    let url = format!("{HOST}/agents");
    Task::perform(
        async move {
            let mut form = vec![
                ("name", name),
                ("harness", harness),
                ("role", role),
                ("model", model),
            ];
            if let Some(slug) = slug {
                form.push(("slug", slug));
            }
            // Repeated keys — reqwest's form serializer keeps duplicates.
            for (n, u) in mcp {
                if !n.trim().is_empty() && !u.trim().is_empty() {
                    form.push(("mcp_name", n));
                    form.push(("mcp_url", u));
                }
            }
            for s in skills {
                if !s.trim().is_empty() {
                    form.push(("skill", s));
                }
            }
            for p in plugins {
                if !p.trim().is_empty() {
                    form.push(("plugin_path", p));
                }
            }
            simple_post_result(&url, &form, "save").await
        },
        Message::AgentSaved,
    )
}

/// POST an agent delete (`/agents/{slug}/delete`).
fn post_delete_agent(slug: String) -> Task<Message> {
    let url = format!("{HOST}/agents/{slug}/delete");
    Task::perform(
        async move { simple_post_result(&url, &[], "delete").await },
        Message::AgentDeleted,
    )
}

/// POST an enroll (`/devices/enroll`) — mints this device's own signing
/// and transport key pair on the host from a founder's pasted invite,
/// modelled on `post_save_agent`/`simple_post_result` but, per that
/// endpoint's JSON response, routed through `post_json_result` instead.
/// `name` is the display name to enroll under; `None` lets the host fall
/// back to this machine's git identity, then the invite email's local
/// part.
fn post_device_enroll(base: String, invite: String, name: Option<String>) -> Task<Message> {
    let url = format!("{base}/devices/enroll");
    Task::perform(
        async move {
            let mut form = vec![("invite", invite)];
            if let Some(name) = name {
                form.push(("name", name));
            }
            post_json_result::<EnrolledDto>(url, form, "join").await
        },
        Message::JoinDone,
    )
}

/// Shared POST → `Result<(), String>` helper: success when the (redirect-
/// followed) status is 2xx, else the status + body as an error.
async fn simple_post_result(url: &str, form: &[(&str, String)], what: &str) -> Result<(), String> {
    match reqwest::Client::new().post(url).form(form).send().await {
        Ok(resp) if resp.status().is_success() => Ok(()),
        Ok(resp) => {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let body = body.trim();
            Err(if body.is_empty() {
                format!("{what} failed ({code})")
            } else {
                format!("{code}: {body}")
            })
        }
        Err(err) => Err(format!("request failed: {err}")),
    }
}

/// Turn a failed response's status, content-type, and body into a short
/// human string a panel can show directly — never the raw body. A JSON
/// content-type body is parsed for a `message` field; anything else — an
/// unrecognized JSON shape (with no `message` to extract — the caller
/// wants that body's *own* fields, e.g. `POST /members`'s 409
/// `{"outcomes":[…]}`, which belongs through `post_json_result_accepting`
/// instead, never through here), an empty/unparseable body, or in
/// particular the identity endpoints' plain-text refusals — which the
/// host's router-wide `prettify_errors` (`crates/junto/src/web.rs`)
/// rewrites into a styled HTML error page — all collapse to the same
/// fixed, status-naming sentence, so a caller here never renders a raw
/// body (HTML or otherwise) in an error box.
fn describe_failed_response(status: u16, content_type: Option<&str>, body: &str) -> String {
    let is_json = content_type.is_some_and(|ct| ct.starts_with("application/json"));
    if is_json
        && let Some(message) = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|value| value.get("message")?.as_str().map(str::to_string))
    {
        return message;
    }
    format!("request failed ({status}); see the host log for details")
}

/// The `post_json`/`post_json_result_accepting` parse decision, factored
/// out of the network call so it's unit-testable: on a 2xx status, a body
/// that fails to parse as `T` genuinely is a bug worth surfacing as a
/// parse error; on an accepted non-2xx status (`post_json_result_accepting`'s
/// `extra_status`), a body that fails to parse as `T` is expected — that
/// status is only sometimes structured (e.g. `POST /members`'s 409 is
/// `{"outcomes":[…]}"` for a rejected-but-recorded outcome, but the
/// router's HTML error page for a stale invite) — so it degrades through
/// `describe_failed_response` instead of reporting a raw parse error.
fn parse_or_describe<T: DeserializeOwned>(
    status: u16,
    content_type: Option<&str>,
    body: &str,
    what: &str,
) -> Result<T, String> {
    match serde_json::from_str::<T>(body) {
        Ok(value) => Ok(value),
        Err(err) if (200..300).contains(&status) => {
            Err(format!("{what}: couldn't parse the response: {err}"))
        }
        Err(_) => Err(describe_failed_response(status, content_type, body)),
    }
}

/// Shared POST → `Result<T, String>` core behind `post_json_result` and
/// `post_json_result_accepting`: reads the response once, then hands the
/// status/content-type/body to `parse_or_describe` when `accept` returns
/// true for the status, else straight to `describe_failed_response`. The
/// two callers differ only in which statuses carry a `T` to parse — never
/// in how a genuine failure is reported.
async fn post_json<T: DeserializeOwned>(
    url: &str,
    form: &[(&str, String)],
    what: &str,
    accept: impl Fn(u16) -> bool,
) -> Result<T, String> {
    let resp = match reqwest::Client::new().post(url).form(form).send().await {
        Ok(resp) => resp,
        Err(err) => return Err(format!("{what}: request failed: {err}")),
    };
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = resp.text().await.unwrap_or_default();
    if !accept(status) {
        return Err(describe_failed_response(
            status,
            content_type.as_deref(),
            &body,
        ));
    }
    parse_or_describe(status, content_type.as_deref(), &body, what)
}

/// Shared POST → `Result<T, String>` helper: parses a JSON body into `T`
/// on 2xx; on failure, `describe_failed_response` turns the status,
/// content-type, and body into a short human string — never the raw body.
/// Beside `simple_post_result`, which returns `()` instead of a parsed
/// body. `post_device_enroll` (Task 12) uses this; an endpoint that
/// returns a structured body on a specific non-2xx status too (e.g.
/// `POST /members`'s 409 outcomes) wants `post_json_result_accepting`
/// instead.
async fn post_json_result<T: DeserializeOwned>(
    url: String,
    form: Vec<(&'static str, String)>,
    what: &'static str,
) -> Result<T, String> {
    post_json(&url, &form, what, |status| (200..300).contains(&status)).await
}

/// Like `post_json_result`, but also parses `T` from `extra_status` (e.g.
/// `409`) instead of treating it as a failure. Some endpoints
/// (`POST /members`) return their per-channel outcomes WITH a non-2xx
/// status by contract; collapsing that into a bare error string would
/// force the caller to string-scrape the JSON back out of
/// `describe_failed_response`'s output, which never dumps a raw body
/// anyway. `post_redeem` (below) is this function's first real caller —
/// `POST /members`'s 409, the channel-pane members disclosure (Task 13) —
/// matching `crates/junto/src/keys.rs::has_transport_key`'s own precedent
/// for kernel API landed ahead of its wiring.
async fn post_json_result_accepting<T: DeserializeOwned>(
    url: String,
    form: Vec<(&'static str, String)>,
    what: &'static str,
    extra_status: u16,
) -> Result<T, String> {
    post_json(&url, &form, what, move |status| {
        (200..300).contains(&status) || status == extra_status
    })
    .await
}

/// POST a founder-issued enrollment invite (`/invites`) covering one or
/// more channels (device-key-enrollment plan, Task 13).
fn post_invite(
    pane: pane_grid::Pane,
    base: String,
    member: String,
    channels: Vec<String>,
) -> Task<Message> {
    let url = format!("{base}/invites");
    Task::perform(
        async move {
            let mut form = vec![("member", member)];
            form.extend(channels.into_iter().map(|c| ("channel", c)));
            post_json_result::<InviteMintedDto>(url, form, "invite")
                .await
                .map(IdentityResult::Minted)
        },
        move |result| Message::IdentityDone(pane, result),
    )
}

/// POST an enroll code's read-only preview (`/devices/preview`) — what
/// redeeming it would grant, before anything is appended. The founder
/// must see this before the kind picker, since the channel set never
/// travels inside the code itself.
fn post_preview_enroll(pane: pane_grid::Pane, base: String, enroll: String) -> Task<Message> {
    let url = format!("{base}/devices/preview");
    Task::perform(
        async move {
            post_json_result::<EnrollPreviewDto>(url, vec![("enroll", enroll)], "preview")
                .await
                .map(IdentityResult::Previewed)
        },
        move |result| Message::IdentityDone(pane, result),
    )
}

/// POST an enrollment redemption (`POST /members`) across every channel
/// its invite still covers. `409` carries a structured per-channel
/// outcome, not a bare failure — routed through
/// `post_json_result_accepting` so that body's truth reaches the panel
/// either way.
fn post_redeem(pane: pane_grid::Pane, base: String, enroll: String, kind: String) -> Task<Message> {
    let url = format!("{base}/members");
    Task::perform(
        async move {
            post_json_result_accepting::<RedeemedDto>(
                url,
                vec![("enroll", enroll), ("kind", kind)],
                "redeem",
                409,
            )
            .await
            .map(IdentityResult::Redeemed)
        },
        move |result| Message::IdentityDone(pane, result),
    )
}

/// POST a retire or revoke act — the two "park a key grant" endpoints,
/// both requiring a rationale (device-key-enrollment plan, Task 10).
/// `target` selects the route: a grant retires by the entry that
/// authorized it; a revoke parks every active grant an email holds.
fn post_park(
    pane: pane_grid::Pane,
    base: String,
    channel: String,
    target: IdentityForm,
    rationale: String,
) -> Task<Message> {
    #[derive(Deserialize)]
    struct ParkedDto {
        parked: usize,
    }
    let url = match &target {
        IdentityForm::Retire { grant } => format!("{base}/channels/{channel}/keys/{grant}/retire"),
        IdentityForm::Revoke { email } => {
            format!("{base}/channels/{channel}/members/{email}/revoke")
        }
        IdentityForm::Invite | IdentityForm::Redeem => {
            unreachable!("post_park is only ever called with Retire/Revoke")
        }
    };
    Task::perform(
        async move {
            post_json_result::<ParkedDto>(url, vec![("rationale", rationale)], "park")
                .await
                .map(|dto| IdentityResult::Parked(dto.parked))
        },
        move |result| Message::IdentityDone(pane, result),
    )
}

/// Fetch a channel's curated brief (recall bridge) as Markdown text.
fn fetch_brief(pane: pane_grid::Pane, base: String, channel: String) -> Task<Message> {
    let url = format!("{base}/channels/{channel}/brief");
    Task::perform(
        async move {
            match reqwest::get(&url).await {
                Ok(resp) if resp.status().is_success() => resp.text().await.ok(),
                _ => None,
            }
        },
        move |md| Message::BriefLoaded(pane, md),
    )
}

/// Fetch the registered home substrates for the new-channel picker.
fn fetch_substrates() -> Task<Message> {
    let url = format!("{HOST}/substrates.json");
    Task::perform(
        async move {
            match reqwest::get(&url).await {
                Ok(response) => response.json::<Vec<String>>().await.unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        },
        Message::SubstratesLoaded,
    )
}

/// Fetch the recent workspace repos for the launch default/suggestions.
fn fetch_workspaces() -> Task<Message> {
    let url = format!("{HOST}/workspaces.json");
    Task::perform(
        async move {
            match reqwest::get(&url).await {
                Ok(response) => response.json::<Vec<String>>().await.unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        },
        Message::WorkspacesLoaded,
    )
}

/// Fetch the configured Agents for the per-pane launch picker.
fn fetch_agents() -> Task<Message> {
    let url = format!("{HOST}/agents.json");
    Task::perform(
        async move {
            match reqwest::get(&url).await {
                Ok(response) => response.json::<Vec<AgentDto>>().await.unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        },
        Message::AgentsLoaded,
    )
}

/// Fetch the list of channel names for the type-ahead picker.
fn fetch_channels() -> Task<Message> {
    #[derive(Deserialize)]
    struct Item {
        name: String,
    }
    Task::perform(
        async move {
            let url = format!("{HOST}/channels.json");
            match reqwest::get(&url).await {
                Ok(response) => response
                    .json::<Vec<Item>>()
                    .await
                    .map(|items| items.into_iter().map(|i| i.name).collect())
                    .unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        },
        Message::ChannelsLoaded,
    )
}

/// A long-lived SSE subscription streaming a session's live feed from the host
/// (`/channels/{channel}/sessions/{session}/stream`) into `Message::Live`.
fn session_stream(channel: String, session: String) -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::{SinkExt, StreamExt};
    iced::stream::channel::<Message>(64, move |mut output: mpsc::Sender<Message>| async move {
        let url = format!("{HOST}/channels/{channel}/sessions/{session}/stream");
        let Ok(response) = reqwest::get(&url).await else {
            let _ = output.send(Message::LiveEnded(session)).await;
            return;
        };
        let mut bytes = response.bytes_stream();
        let mut buf = String::new();
        while let Some(Ok(chunk)) = bytes.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            // SSE frames are separated by a blank line.
            while let Some(idx) = buf.find("\n\n") {
                let frame: String = buf.drain(..idx + 2).collect();
                if frame.contains("event: end") {
                    let _ = output.send(Message::LiveEnded(session.clone())).await;
                    return;
                }
                if let Some(data) = frame.lines().find_map(|l| l.strip_prefix("data:"))
                    && let Ok(event) = serde_json::from_str::<LiveEvent>(data.trim())
                {
                    // The local SSE stream is not a live document, so it
                    // exposes no container index and its lines are not
                    // pointable — the composer is websocket-only anyway.
                    let _ = output
                        .send(Message::Live(session.clone(), None, event))
                        .await;
                }
            }
        }
        let _ = output.send(Message::LiveEnded(session)).await;
    })
}

/// A session's live-websocket URL, derived from a pane's REST base URL
/// (`Pane::base`) by swapping the scheme: `http` → `ws`, `https` → `wss`.
///
/// The channel and session are percent-encoded: a channel name is free text
/// and may contain spaces (`febe66f2` keeps names human), and
/// `tokio_tungstenite` refuses the resulting URL outright with "invalid uri
/// character" — so an ordinary name silently broke live watching, while the
/// REST calls beside it kept working because `reqwest` encodes for itself.
fn ws_url(base: &str, channel: &str, session: &str) -> String {
    let base = base.trim_end_matches('/');
    let (scheme, rest) = base.split_once("://").unwrap_or(("http", base));
    let ws_scheme = if scheme == "https" { "wss" } else { "ws" };
    let channel = encode_segment(channel);
    let session = encode_segment(session);
    format!("{ws_scheme}://{rest}/channels/{channel}/sessions/{session}/live")
}

/// Percent-encode one URL path segment, keeping only the RFC 3986 unreserved
/// set. Used by [`ws_url`], which builds its URL by string formatting and so
/// has no library doing this for it.
fn encode_segment(segment: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// A synthetic error line for the live feed — same shape `Message::Steered`'s
/// error arm already pushes for a failed REST steer.
fn error_event(text: String) -> LiveEvent {
    LiveEvent {
        kind: "error".into(),
        text,
        seq: 0,
        html: false,
        markdown: None,
    }
}

/// The junto machine-local key store's home dir: `$JUNTO_HOME` if set, else
/// `~/.junto` — the same resolution `crates/junto/src/host.rs::junto_home`
/// uses.
fn junto_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("JUNTO_HOME") {
        return Some(PathBuf::from(home));
    }
    std::env::home_dir().map(|home| home.join(".junto"))
}

/// Where the shell's layout state lives — `<junto-home>/ui.toml`, alongside
/// the host's `keys.toml`. Falls back to a relative path when the home cannot
/// be resolved; `shell::load` treats an unreadable path as "use defaults".
fn shell_state_path() -> PathBuf {
    junto_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ui.toml")
}

/// One record in `<junto-home>/keys.toml` — mirrors `crates/junto/src/keys.rs`'s
/// own `KeyRecord`. A separate copy, not a shared type: junto-iced is a
/// standalone workspace that never links against the `junto` binary crate.
#[derive(Debug, Deserialize)]
struct KeyRecord {
    email: String,
    /// 64 hex chars — the Ed25519 secret seed.
    secret: String,
}

/// The serialized shape of `<junto-home>/keys.toml`.
#[derive(Debug, Default, Deserialize)]
struct KeysFile {
    #[serde(default)]
    keys: Vec<KeyRecord>,
}

/// The signing key for `email`, read from the host's machine-local key store
/// (`<junto-home>/keys.toml`, `crates/junto/src/keys.rs`) — `None` if the
/// home can't be resolved, the file is missing or unparseable, or no record
/// matches `email`. Never mints one: minting is the host's job (its own
/// first-use path in `crates/junto/src/keys.rs::signing_key`); a watcher
/// with no local identity on file simply can't authenticate yet.
fn load_signing_key(email: &str) -> Option<SigningKey> {
    let path = junto_home()?.join("keys.toml");
    let text = std::fs::read_to_string(path).ok()?;
    let file: KeysFile = toml::from_str(&text).ok()?;
    let record = file.keys.into_iter().find(|record| record.email == email)?;
    SigningKey::from_secret_hex(&record.secret).ok()
}

/// The 16-hex fingerprint the host derives from a public key
/// (`crates/junto/src/identity.rs::fingerprint`): strip the `ed25519:`
/// prefix, take the first 16 hex chars. Takes the key's raw string form
/// (`SigningKey::public_key().as_str()`/`PublicKey::as_str()`) rather than
/// a typed key, since the GUI derives this locally and must agree with the
/// host's format byte-for-byte, or the two screens show different ids for
/// one key.
fn device_fingerprint(key: &str) -> String {
    key.strip_prefix("ed25519:")
        .unwrap_or(key)
        .chars()
        .take(16)
        .collect()
}

/// Serialize and send one `WireFrame` over the live websocket's write half.
async fn send_frame<W>(
    write: &mut W,
    frame: &WireFrame,
) -> Result<(), tokio_tungstenite::tungstenite::Error>
where
    W: iced::futures::Sink<
            tokio_tungstenite::tungstenite::Message,
            Error = tokio_tungstenite::tungstenite::Error,
        > + Unpin,
{
    use iced::futures::SinkExt;
    write
        .send(tokio_tungstenite::tungstenite::Message::text(
            serde_json::to_string(frame).expect("WireFrame always serializes"),
        ))
        .await
}

/// What the FIRST frame on a freshly opened live socket means.
enum FirstFrame {
    /// The handshake is on: sign this nonce.
    Challenge(String),
    /// The session has no live document, so there is nothing to watch. The host
    /// answers that with `End` BEFORE challenging and calls it "graceful, not a
    /// failure" (`crates/junto/src/live_ws.rs`) — watching a landed session is
    /// the ordinary case and must close quietly.
    QuietEnd,
    /// A real failure, with something worth showing the reviewer.
    Failed(String),
}

/// Classify the first frame of a live-socket handshake.
///
/// Split out because getting this wrong is user-visible, and was: treating the
/// pre-challenge `End` as "expected a challenge" put a "handshake failed" line
/// in the feed every time anyone watched a session that was not currently
/// running a turn — which became every local watch once the websocket stopped
/// being remote-only.
fn classify_first_frame(
    incoming: Option<
        Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>,
    >,
) -> FirstFrame {
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    match incoming {
        Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<WireFrame>(text.as_str()) {
            Ok(WireFrame::Challenge { nonce }) => FirstFrame::Challenge(nonce),
            Ok(WireFrame::End) => FirstFrame::QuietEnd,
            // The host also refuses before challenging when the session is not
            // watchable at all; that reason IS worth showing, unlike `End`.
            Ok(WireFrame::Rejected { reason }) => FirstFrame::Failed(reason),
            _ => FirstFrame::Failed("handshake failed: expected a challenge".into()),
        },
        // A clean close carrying no frame means the same as `End`: nothing to
        // watch. Only a transport error or a non-text frame is a failure.
        Some(Ok(WsMessage::Close(_))) | None => FirstFrame::QuietEnd,
        Some(Ok(_)) => FirstFrame::Failed("handshake failed: expected a challenge".into()),
        Some(Err(err)) => FirstFrame::Failed(format!("handshake failed: {err}")),
    }
}

/// A long-lived subscription streaming a session's live feed from a REMOTE
/// host's authenticated live websocket (`/channels/{channel}/sessions/{session}/live`)
/// into `Message::Live`/`Message::Watchers` — the websocket counterpart of
/// `session_stream`'s local SSE, used instead of it when a pane has a
/// `remote` host configured (`App::subscription`).
///
/// Handshakes with `load_signing_key(&email)`'s key, then maintains a local
/// `LiveDoc`/`Presence`: every inbound `Update` is imported and its new or
/// changed (replace-in-place, `LiveDoc` module docs) conversation entries
/// are emitted, alongside `Message::ConversationLen` (the container's own
/// length, for the annotation composer's `StreamAnchor`) and, for any new
/// `{"kind":"diff","commit":…}` worktree entry, `Message::WorktreeDiff`
/// (the only source the composer's `CodeAnchor` may take a commit from).
/// Every inbound `Ephemeral` is applied and re-published as
/// `Message::Watchers`. Sends a presence heartbeat every 10s, and forwards
/// anything received on the connected `annotate_tx` straight to the socket
/// — the annotation composer's outbound path.
fn live_ws_stream(
    base: String,
    channel: String,
    session: String,
    email: String,
) -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    iced::stream::channel::<Message>(64, move |mut output: mpsc::Sender<Message>| async move {
        let Some(signing_key) = load_signing_key(&email) else {
            let _ = output
                .send(Message::Live(
                    session.clone(),
                    None,
                    error_event(format!("no signing key on file for '{email}'")),
                ))
                .await;
            let _ = output.send(Message::LiveEnded(session)).await;
            return;
        };
        let url = ws_url(&base, &channel, &session);
        let socket = match tokio_tungstenite::connect_async(&url).await {
            Ok((socket, _response)) => socket,
            Err(err) => {
                let _ = output
                    .send(Message::Live(
                        session.clone(),
                        None,
                        error_event(format!("connect failed: {err}")),
                    ))
                    .await;
                let _ = output.send(Message::LiveEnded(session)).await;
                return;
            }
        };
        let (mut write, mut read) = socket.split();

        // Handshake: Challenge → Auth → AuthOk. What the FIRST frame means is
        // classified by `classify_first_frame`, so the distinction that matters
        // — a graceful `End` versus a real failure — is unit-tested.
        let nonce = match classify_first_frame(read.next().await) {
            FirstFrame::Challenge(nonce) => nonce,
            FirstFrame::QuietEnd => {
                let _ = output.send(Message::LiveEnded(session)).await;
                return;
            }
            FirstFrame::Failed(reason) => {
                let _ = output
                    .send(Message::Live(session.clone(), None, error_event(reason)))
                    .await;
                let _ = output.send(Message::LiveEnded(session)).await;
                return;
            }
        };
        let signature = signing_key.sign_bytes(nonce.as_bytes());
        let auth = WireFrame::Auth {
            email: email.clone(),
            signature: signature.into(),
        };
        if let Err(err) = send_frame(&mut write, &auth).await {
            let _ = output
                .send(Message::Live(
                    session.clone(),
                    None,
                    error_event(format!("failed to send auth: {err}")),
                ))
                .await;
            let _ = output.send(Message::LiveEnded(session)).await;
            return;
        }
        match read.next().await {
            Some(Ok(WsMessage::Text(text))) => {
                match serde_json::from_str::<WireFrame>(text.as_str()) {
                    Ok(WireFrame::AuthOk) => {}
                    Ok(WireFrame::Rejected { reason }) => {
                        let _ = output
                            .send(Message::Live(session.clone(), None, error_event(reason)))
                            .await;
                        let _ = output.send(Message::LiveEnded(session)).await;
                        return;
                    }
                    _ => {
                        let _ = output.send(Message::LiveEnded(session)).await;
                        return;
                    }
                }
            }
            _ => {
                let _ = output.send(Message::LiveEnded(session)).await;
                return;
            }
        }

        // Wired for Task 10's annotation composer: anything sent here is
        // forwarded straight to the socket, below.
        let (annotate_tx, mut annotate_rx) = mpsc::channel::<WireFrame>(16);
        if output
            .send(Message::LiveConnected(
                session.clone(),
                annotate_tx,
                email.clone(),
            ))
            .await
            .is_err()
        {
            return;
        }

        let doc = LiveDoc::new();
        let presence = Presence::new();
        // How many conversation entries have already been emitted, plus the
        // last of those entries' own value — so a replace-in-place update to
        // that still-growing last entry (the host's `LiveDoc` coalescing) is
        // re-emitted instead of missed, while entries before it (frozen once
        // superseded) are never re-checked.
        let mut emitted = 0usize;
        let mut worktree_emitted = 0usize;
        let mut last_seen: Option<serde_json::Value> = None;
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(10));
        heartbeat.tick().await; // the first tick fires immediately

        loop {
            tokio::select! {
                incoming = read.next() => {
                    match incoming {
                        Some(Ok(WsMessage::Text(text))) => {
                            let Ok(frame) = serde_json::from_str::<WireFrame>(text.as_str()) else {
                                continue;
                            };
                            match frame {
                                WireFrame::Update { .. } => {
                                    let Some(bytes) = frame.update_bytes() else { continue };
                                    if doc.import_update(&bytes).is_err() {
                                        continue;
                                    }
                                    let len = doc.conversation_len();
                                    // The already-emitted last entry may have grown in
                                    // place (the host's replace-in-place coalescing,
                                    // `LiveDoc` module docs) — re-emit it if it
                                    // changed, so the app's own seq-keyed coalescing
                                    // lands the final text, not a stale mid-growth one.
                                    if emitted > 0
                                        && let Some(current) = doc.conversation_event(emitted - 1)
                                        && Some(&current) != last_seen.as_ref()
                                    {
                                        if let Ok(event) =
                                            serde_json::from_value::<LiveEvent>(current.clone())
                                        {
                                            let _ = output
                                                .send(Message::Live(
                                                    session.clone(),
                                                    // Its own container index —
                                                    // the block a StreamAnchor
                                                    // on this row would name.
                                                    Some(emitted - 1),
                                                    event,
                                                ))
                                                .await;
                                        }
                                        last_seen = Some(current);
                                    }
                                    for i in emitted..len {
                                        let Some(value) = doc.conversation_event(i) else {
                                            continue;
                                        };
                                        if let Ok(event) =
                                            serde_json::from_value::<LiveEvent>(value.clone())
                                        {
                                            let _ = output
                                                .send(Message::Live(
                                                    session.clone(),
                                                    Some(i),
                                                    event,
                                                ))
                                                .await;
                                        }
                                        last_seen = Some(value);
                                    }
                                    emitted = len;
                                    let _ = output
                                        .send(Message::ConversationLen(session.clone(), len))
                                        .await;
                                    let wt_len = doc.worktree_len();
                                    for i in worktree_emitted..wt_len {
                                        let Some(value) = doc.worktree_event(i) else {
                                            continue;
                                        };
                                        if value.get("kind").and_then(|k| k.as_str())
                                            == Some("diff")
                                            && let Some(commit) =
                                                value.get("commit").and_then(|c| c.as_str())
                                        {
                                            let _ = output
                                                .send(Message::WorktreeDiff(
                                                    session.clone(),
                                                    commit.to_string(),
                                                ))
                                                .await;
                                        }
                                    }
                                    worktree_emitted = wt_len;
                                }
                                WireFrame::Ephemeral { .. } => {
                                    let Some(bytes) = frame.ephemeral_bytes() else { continue };
                                    if presence.apply(&bytes).is_ok() {
                                        let _ = output
                                            .send(Message::Watchers(session.clone(), presence.watchers()))
                                            .await;
                                    }
                                }
                                WireFrame::Rejected { reason } => {
                                    let _ = output
                                        .send(Message::Live(
                                            session.clone(),
                                            None,
                                            error_event(reason),
                                        ))
                                        .await;
                                }
                                WireFrame::End => {
                                    let _ = output.send(Message::LiveEnded(session)).await;
                                    return;
                                }
                                WireFrame::Challenge { .. }
                                | WireFrame::Auth { .. }
                                | WireFrame::AuthOk => {}
                            }
                        }
                        Some(Ok(WsMessage::Close(_))) | None => {
                            let _ = output.send(Message::LiveEnded(session)).await;
                            return;
                        }
                        Some(Ok(_)) => {} // ping/pong/binary: nothing this protocol needs.
                        Some(Err(_)) => {
                            let _ = output.send(Message::LiveEnded(session)).await;
                            return;
                        }
                    }
                }
                _ = heartbeat.tick() => {
                    presence.set_watching(&email);
                    let frame = WireFrame::ephemeral(&presence.encode_all());
                    if send_frame(&mut write, &frame).await.is_err() {
                        let _ = output.send(Message::LiveEnded(session)).await;
                        return;
                    }
                }
                Some(frame) = annotate_rx.next() => {
                    if send_frame(&mut write, &frame).await.is_err() {
                        let _ = output.send(Message::LiveEnded(session)).await;
                        return;
                    }
                }
            }
        }
    })
}

/// POST a launch (a new session) for `channel` — intent plus the agent slug,
/// mode (`single`/`outcome`), and optional workspace the picker selected.
fn post_launch(
    pane: pane_grid::Pane,
    base: String,
    channel: String,
    intent: String,
    agent: Option<String>,
    mode: &'static str,
    workspace: String,
) -> Task<Message> {
    let url = format!("{base}/channels/{channel}/sessions");
    Task::perform(
        async move {
            let mut form = vec![
                ("intent", intent),
                ("mode", mode.to_string()),
                ("workspace", workspace),
            ];
            if let Some(agent) = agent {
                form.push(("agent", agent));
            }
            match reqwest::Client::new().post(&url).form(&form).send().await {
                Ok(resp) if resp.status().is_success() => Ok(()),
                Ok(resp) => {
                    let code = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let body = body.trim();
                    Err(if body.is_empty() {
                        format!("launch failed ({code})")
                    } else {
                        format!("{code}: {body}")
                    })
                }
                Err(err) => Err(format!("request failed: {err}")),
            }
        },
        move |result| Message::Launched(pane, result),
    )
}

/// POST a verification act on an entry — ratify/park (assertions) or
/// approve/reject (proposals), with the required rationale.
fn post_verify(
    pane: pane_grid::Pane,
    base: String,
    channel: String,
    entry: String,
    act: String,
    rationale: String,
) -> Task<Message> {
    let url = format!("{base}/channels/{channel}/entries/{entry}/{act}");
    let entry_id = entry.clone();
    Task::perform(
        async move {
            match reqwest::Client::new()
                .post(&url)
                .form(&[("rationale", rationale)])
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => Ok(()),
                Ok(resp) => {
                    let code = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let body = body.trim();
                    Err(if body.is_empty() {
                        format!("act failed ({code})")
                    } else {
                        format!("{code}: {body}")
                    })
                }
                Err(err) => Err(format!("request failed: {err}")),
            }
        },
        move |result| Message::Acted(pane, entry_id.clone(), result),
    )
}

/// POST a session act — `steer` (with a message) or `interrupt` (no body).
fn post_act(
    pane: pane_grid::Pane,
    base: String,
    channel: String,
    session: String,
    act: &'static str,
    message: Option<String>,
) -> Task<Message> {
    let url = format!("{base}/channels/{channel}/sessions/{session}/{act}");
    Task::perform(
        async move {
            let request = reqwest::Client::new().post(&url);
            let request = match message {
                Some(message) => request.form(&[("message", message)]),
                None => request,
            };
            let _ = request.send().await;
        },
        move |()| Message::Posted(pane),
    )
}

/// POST a channel lifecycle act (close / reopen / diverge / converge) and
/// report the result. Diverge returns the child's name to open.
fn post_lifecycle(
    pane: pane_grid::Pane,
    base: String,
    channel: String,
    kind: LifecycleKind,
    text: String,
    target: String,
) -> Task<Message> {
    let (path, form, outcome) = match kind {
        LifecycleKind::Close => ("close", vec![("rationale", text)], LifecycleResult::Done),
        LifecycleKind::Reopen => ("reopen", vec![("rationale", text)], LifecycleResult::Done),
        LifecycleKind::Diverge => (
            "diverge",
            vec![("child_name", text.clone())],
            LifecycleResult::OpenChild(text), // open the new side-quest by name
        ),
        LifecycleKind::Converge => (
            "converge",
            vec![("target", target), ("rationale", text)],
            LifecycleResult::Done,
        ),
        LifecycleKind::Rename => (
            "rename",
            vec![("name", target.clone()), ("rationale", text)],
            LifecycleResult::Renamed(target),
        ),
    };
    let url = format!("{base}/channels/{channel}/{path}");
    Task::perform(
        async move {
            match reqwest::Client::new().post(&url).form(&form).send().await {
                Ok(resp) if resp.status().is_success() => Ok(outcome),
                Ok(resp) => {
                    let code = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let body = body.trim();
                    Err(if body.is_empty() {
                        format!("{path} failed ({code})")
                    } else {
                        format!("{code}: {body}")
                    })
                }
                Err(err) => Err(format!("request failed: {err}")),
            }
        },
        move |result| Message::LifecycleDone(pane, result),
    )
}

/// POST a new channel (open) and report the result (the name to open on success).
fn post_create_channel(name: String, repo: Option<String>) -> Task<Message> {
    let url = format!("{HOST}/channels");
    Task::perform(
        async move {
            let mut form = vec![("name", name.clone())];
            if let Some(repo) = repo {
                form.push(("repo", repo));
            }
            match reqwest::Client::new().post(&url).form(&form).send().await {
                Ok(resp) if resp.status().is_success() => Ok(name),
                Ok(resp) => {
                    let code = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let body = body.trim();
                    Err(if body.is_empty() {
                        format!("create failed ({code})")
                    } else {
                        format!("{code}: {body}")
                    })
                }
                Err(err) => Err(format!("request failed: {err}")),
            }
        },
        Message::ChannelCreated,
    )
}

/// POST a steer message and report the result, so the pane can re-stream the
/// resumed turn (a landed session) or keep streaming (a live one).
fn post_steer(
    pane: pane_grid::Pane,
    base: String,
    channel: String,
    session: String,
    message: String,
) -> Task<Message> {
    let url = format!("{base}/channels/{channel}/sessions/{session}/steer");
    Task::perform(
        async move {
            match reqwest::Client::new()
                .post(&url)
                .form(&[("message", message)])
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => Ok(()),
                Ok(resp) => {
                    let code = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let body = body.trim();
                    Err(if body.is_empty() {
                        format!("steer failed ({code})")
                    } else {
                        format!("{code}: {body}")
                    })
                }
                Err(err) => Err(format!("request failed: {err}")),
            }
        },
        move |result| Message::Steered(pane, result),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Headless UI test, which is the reason for the 0.14 upgrade. Verifying
    /// that clicking a diff row emits the right anchor used to mean driving the
    /// real window through Win32 — stolen focus, DPI-scaled coordinates, and
    /// dropped keystrokes. `iced_test` selects a widget BY ITS TEXT and clicks
    /// it in memory, so the same claim is now an ordinary assertion.
    #[test]
    fn clicking_a_rendered_diff_row_emits_that_rows_anchor() {
        let diff = "\
diff --git a/lib.rs b/lib.rs
+++ b/lib.rs
@@ -1,3 +1,4 @@
 fn one() {}
-fn two() {}
+fn two() { println!(\"two\"); }
 fn three() {}
";
        // A pane is only needed so the aimed row can build its panel; nothing
        // is aimed here, so the rows are plain click targets.
        let (panes, id) = pane_grid::State::new(Pane::loading("c"));
        let pane = panes.get(id).expect("the pane just created");
        let aim = Aim {
            path: "",
            record: None,
            span: None,
            popup_at: None,
            hover: None,
            pane,
        };

        let mut ui = iced_test::simulator(artifact_body(
            id,
            "a1",
            "sha256:x",
            "diff",
            diff,
            None,
            Some(aim),
        ));

        // The added line is row 5 of the diff and line 2 of the NEW file: the
        // removed row above it consumes no new-file line.
        ui.click("+fn two() { println!(\"two\"); }")
            .expect("the added row is a click target");
        let messages: Vec<Message> = ui.into_messages().collect();

        // A click is now a press AND a release (`mouse_area`), so the assertion
        // names the press rather than demanding the whole list be one message.
        assert!(
            messages.iter().any(|m| matches!(
                m,
                Message::AnchorPress(_, AnchorTarget::Code(path), 2) if path == "lib.rs"
            )),
            "expected a code anchor press at lib.rs:2, got {messages:?}"
        );
    }

    /// Builds a pane watching session `s1`, whose record holds one diff
    /// artifact with its content already fetched.
    fn reviewing_pane() -> (pane_grid::State<Pane>, pane_grid::Pane, &'static str) {
        const DIFF: &str = "\
diff --git a/lib.rs b/lib.rs
+++ b/lib.rs
@@ -1,3 +1,4 @@
 fn one() {}
+fn two() { println!(\"two\"); }
";
        let entry = |id: &str, kind: &str, summary: &str| EntryDto {
            id: id.into(),
            author: "omp@oh-my-pi.dev".into(),
            kind: kind.into(),
            summary: summary.into(),
            status: None,
            unrecognized: false,
            unverified: false,
            target: Some("s1".into()),
            frame: Vec::new(),
        };
        let (mut panes, id) = pane_grid::State::new(Pane::loading("c"));
        let pane = panes.get_mut(id).expect("the pane just created");
        pane.watched = Some("s1".into());
        pane.content = Content::Loaded(ChannelDto {
            id: "c".into(),
            name: Some("c".into()),
            closed: false,
            party: Vec::new(),
            workspace: None,
            sessions: Vec::new(),
            entries: vec![
                entry("s1", "session", "did a thing"),
                entry("m1", "memo", "memo: some prose"),
                entry("a1", "artifact", "diff: lib.rs"),
            ],
        });
        pane.artifacts.insert(
            "a1".into(),
            ArtifactContent::Loaded {
                format: "diff".into(),
                body: DIFF.into(),
                md: None,
                digest: ContentDigest::sha256_of(DIFF.as_bytes())
                    .as_str()
                    .to_string(),
            },
        );
        (panes, id, DIFF)
    }

    #[test]
    fn watching_a_session_puts_its_diff_on_screen_with_no_clicks() {
        // The review-first arrangement's whole claim (ledger `532826c2`): a
        // reviewer who opens a session is looking AT the code, having clicked
        // nothing. Simulating `pane_body` rather than `artifact_body` is the
        // point — it is the layout, not the diff renderer, that is on trial.
        let (panes, id, _) = reviewing_pane();
        let pane = panes.get(id).expect("the pane just built");
        let mut ui = iced_test::simulator(pane_body(id, pane, &[], &[]));

        ui.find("+fn two() { println!(\"two\"); }")
            .expect("the diff's added line must be on screen before any click");
    }

    #[test]
    fn the_primary_diff_is_not_also_a_card_in_the_side_record() {
        // Showing it twice is how the record got long enough to hide things in,
        // so the artifact's own card must be gone - while the memo beside it,
        // which has no primary panel, stays.
        let (panes, id, _) = reviewing_pane();
        let pane = panes.get(id).expect("the pane just built");
        let mut ui = iced_test::simulator(pane_body(id, pane, &[], &[]));

        ui.find("memo: some prose")
            .expect("a non-diff entry still belongs in the side record");
        assert!(
            ui.find("diff: lib.rs").is_ok(),
            "the primary panel labels itself with the artifact's summary"
        );
        // An artifact card carries a toggle; the primary panel never does, so
        // with the primary excluded no toggle exists in the pane at all. The
        // label reads "hide" here because this artifact's content is loaded.
        assert!(
            ui.find("hide content ▾").is_err(),
            "the primary diff must not also appear as a card in the record"
        );
    }

    #[test]
    fn clicking_a_removed_diff_row_anchors_the_record_not_the_file() {
        // CONTRACT DELIBERATELY INVERTED (Dan, 2026-08-23: "EVERYTHING should be
        // pointable"). This test previously asserted a removed row emitted
        // NOTHING, which was correct while `CodeAnchor` was the only anchor a
        // diff row could make: a deleted line has no line in the new file, so
        // there was nothing truthful to point at. `Anchor::Record` gives it
        // something — the line still exists in the stored artifact's TEXT, at
        // row 3 — so the row is now a target for that claim instead, and the
        // old silence is the bug rather than the guarantee.
        let diff = "+++ b/lib.rs\n@@ -1,2 +1,1 @@\n-fn gone() {}\n fn stays() {}\n";
        let (panes, id) = pane_grid::State::new(Pane::loading("c"));
        let pane = panes.get(id).expect("the pane just created");
        let aim = Aim {
            path: "",
            record: None,
            span: None,
            popup_at: None,
            hover: None,
            pane,
        };

        let mut ui = iced_test::simulator(artifact_body(
            id,
            "a1",
            "sha256:x",
            "diff",
            diff,
            None,
            Some(aim),
        ));
        ui.click("-fn gone() {}")
            .expect("a removed row is a record target");
        let messages: Vec<Message> = ui.into_messages().collect();

        assert!(
            messages.iter().any(|m| matches!(
                m,
                Message::AnchorPress(_, AnchorTarget::Record(entry, digest), 3)
                    if entry == "a1" && digest == "sha256:x"
            )),
            "expected a record anchor at line 3 of a1, got {messages:?}"
        );
        // The surviving context line keeps its stronger claim: it has a real
        // new-file line, so it still anchors the FILE, not the artifact text.
        let mut ui = iced_test::simulator(artifact_body(
            id,
            "a1",
            "sha256:x",
            "diff",
            diff,
            None,
            Some(aim),
        ));
        ui.click(" fn stays() {}")
            .expect("a context row is a target");
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            messages.iter().any(|m| matches!(
                m,
                Message::AnchorPress(_, AnchorTarget::Code(path), 1) if path == "lib.rs"
            )),
            "expected a code anchor at lib.rs:1, got {messages:?}"
        );
    }

    #[test]
    fn ws_url_swaps_http_and_https_schemes() {
        assert_eq!(
            ws_url("http://h:1727", "c", "s"),
            "ws://h:1727/channels/c/sessions/s/live"
        );
        assert_eq!(
            ws_url("https://h:1727", "c", "s"),
            "wss://h:1727/channels/c/sessions/s/live"
        );
        // A stray trailing slash on the base is tolerated.
        assert_eq!(
            ws_url("http://h:1727/", "c", "s"),
            "ws://h:1727/channels/c/sessions/s/live"
        );
    }

    #[test]
    fn ws_url_percent_encodes_a_channel_name_with_spaces() {
        // Found by dogfooding, not by reading: a channel literally named
        // "pointing dogfood 20260823" made `tokio_tungstenite` refuse the URL
        // ("invalid uri character"), so live watching failed for an ordinary
        // human-readable name while every REST call in the same pane worked.
        assert_eq!(
            ws_url("http://127.0.0.1:1727", "pointing dogfood 20260823", "s"),
            "ws://127.0.0.1:1727/channels/pointing%20dogfood%2020260823/sessions/s/live"
        );
        // Unreserved characters are left alone, so ids and slugs are unchanged.
        assert_eq!(
            ws_url("http://h", "a-b_c.d~e", "2dd175c6-0d6b"),
            "ws://h/channels/a-b_c.d~e/sessions/2dd175c6-0d6b/live"
        );
        // A name that could otherwise inject extra path segments is encoded.
        assert_eq!(
            ws_url("http://h", "a/../b", "s"),
            "ws://h/channels/a%2F..%2Fb/sessions/s/live"
        );
    }

    /// Same shape as `crates/junto/src/keys.rs`'s own tests: a tempdir
    /// `keys.toml` with a known secret, asserting the derived public key
    /// matches. `JUNTO_HOME` is process-global; this crate has no other test
    /// that touches it, so no cross-test lock is needed (unlike
    /// `crates/junto/src/host.rs::tests::HomeGuard`, which several tests
    /// share).
    #[test]
    fn load_signing_key_reads_the_matching_record_by_email() {
        let home = std::env::temp_dir().join(format!(
            "junto-iced-test-keys-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).expect("create temp home");
        let key = SigningKey::from_secret_bytes([7u8; 32]);
        std::fs::write(
            home.join("keys.toml"),
            format!(
                "[[keys]]\nemail = \"dan@example.com\"\nsecret = \"{}\"\n",
                key.to_secret_hex()
            ),
        )
        .expect("write keys.toml");

        let previous = std::env::var_os("JUNTO_HOME");
        // SAFETY: test-only, single-threaded use of this env var in this crate.
        unsafe { std::env::set_var("JUNTO_HOME", &home) };
        let loaded = load_signing_key("dan@example.com");
        let missing = load_signing_key("nobody@example.com");
        match previous {
            Some(value) => unsafe { std::env::set_var("JUNTO_HOME", value) },
            None => unsafe { std::env::remove_var("JUNTO_HOME") },
        }
        std::fs::remove_dir_all(&home).ok();

        assert_eq!(
            loaded
                .expect("key on file for dan@example.com")
                .public_key(),
            key.public_key(),
            "the derived public key matches the stored secret"
        );
        assert!(missing.is_none(), "no record on file for an unknown email");
    }

    #[test]
    fn device_fingerprint_matches_the_hosts_sixteen_chars() {
        // The GUI derives a fingerprint locally; it must agree with the host's
        // identity::fingerprint, or the two screens show different ids for one key.
        let key = "ed25519:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(device_fingerprint(key), "0123456789abcdef");
    }

    #[test]
    fn describe_failed_response_prefers_a_json_message_and_never_dumps_a_raw_body() {
        assert_eq!(
            describe_failed_response(400, Some("application/json"), r#"{"message":"nope"}"#),
            "nope"
        );
        // A JSON body with no recognized "message" field is never dumped
        // into the panel raw — same fixed sentence as every other failure
        // shape. A structured non-2xx body a caller actually wants (e.g.
        // `POST /members`'s 409 `{"outcomes":[…]}`) belongs through
        // `post_json_result_accepting`, never through this string.
        assert_eq!(
            describe_failed_response(409, Some("application/json"), r#"{"outcomes":[1,2]}"#),
            "request failed (409); see the host log for details"
        );
        // An empty or unparseable JSON-content-type body still yields a
        // non-empty, status-naming string — never a blank ⚠ box.
        let empty = describe_failed_response(500, Some("application/json"), "");
        assert!(!empty.is_empty(), "never a blank error string");
        assert!(empty.contains("500"), "names the status: {empty}");
        // The identity endpoints' plain-text refusals arrive here as the
        // host's `prettify_errors` HTML error page — never rendered raw.
        let html = describe_failed_response(
            400,
            Some("text/html; charset=utf-8"),
            "<html><body><h1>That needs a small fix</h1><p>this invite has expired</p></body></html>",
        );
        assert!(
            !html.contains('<'),
            "no raw markup leaks into the panel: {html}"
        );
        assert!(
            !html.to_lowercase().contains("html"),
            "no mention of the wrapper format: {html}"
        );
    }

    #[test]
    fn parse_or_describe_degrades_an_unparseable_accepted_non_2xx_body() {
        // An accepted non-2xx status (e.g. `POST /members`'s 409) is only
        // SOMETIMES structured — a stale invite's refusal arrives as the
        // router's HTML error page, not `{"outcomes":[…]}`. That must
        // degrade through `describe_failed_response`'s human sentence,
        // never a raw serde parse-error string or markup.
        let result: Result<serde_json::Value, String> = parse_or_describe(
            409,
            Some("text/html; charset=utf-8"),
            "<html><body>this invite has expired</body></html>",
            "members",
        );
        let err = result.expect_err("an HTML body never parses as JSON");
        assert!(
            !err.contains("couldn't parse"),
            "not a developer-facing parse-error string: {err}"
        );
        assert!(
            !err.contains('<'),
            "no raw markup leaks into the panel: {err}"
        );
        assert_eq!(err, "request failed (409); see the host log for details");
    }

    #[test]
    fn parse_or_describe_reports_a_parse_error_for_a_malformed_2xx_body() {
        // A 2xx that fails to parse as `T` genuinely is a bug — still
        // worth surfacing as a parse error, unlike the accepted-non-2xx case.
        let result: Result<serde_json::Value, String> =
            parse_or_describe(200, Some("application/json"), "not json", "join");
        let err = result.expect_err("malformed JSON never parses");
        assert!(
            err.contains("couldn't parse"),
            "surfaced as a parse error: {err}"
        );
    }

    #[test]
    fn parse_span_accepts_single_and_range_and_rejects_malformed() {
        assert_eq!(parse_span("12"), Span::new(12, 12).ok());
        assert_eq!(parse_span("12-14"), Span::new(12, 14).ok());
        // Whitespace around either form is tolerated.
        assert_eq!(parse_span(" 12 - 14 "), Span::new(12, 14).ok());
        assert_eq!(parse_span("0"), None, "line numbers are 1-indexed");
        assert_eq!(parse_span("9-3"), None, "an inverted range is rejected");
        assert_eq!(parse_span("x"), None, "non-numeric input is rejected");
        assert_eq!(parse_span(""), None, "empty input is rejected");
        assert_eq!(parse_span("12-"), None, "a dangling range is rejected");
        assert_eq!(parse_span("-12"), None, "a missing start is rejected");
    }

    /// All fields defaulted or empty — a minimal `EntryDto` for tests that
    /// only care about the authorship-badge fields.
    fn sample_entry() -> EntryDto {
        EntryDto {
            id: String::new(),
            author: String::new(),
            kind: String::new(),
            summary: String::new(),
            status: None,
            unrecognized: false,
            unverified: false,
            target: None,
            frame: Vec::new(),
        }
    }

    #[test]
    fn entry_badges_suppress_unverified_on_an_unrecognized_card() {
        let both = EntryDto {
            unrecognized: true,
            unverified: true,
            ..sample_entry()
        };
        assert_eq!(entry_badges(&both), (true, false));
        let only_unverified = EntryDto {
            unrecognized: false,
            unverified: true,
            ..sample_entry()
        };
        assert_eq!(entry_badges(&only_unverified), (false, true));
        let clean = EntryDto {
            unrecognized: false,
            unverified: false,
            ..sample_entry()
        };
        assert_eq!(entry_badges(&clean), (false, false));
    }

    #[test]
    fn device_line_reads_active_and_retired_differently() {
        let active = KeyGrantDto {
            fingerprint: "abc".into(),
            transport_fingerprint: Some("def".into()),
            granted_by: "e1".into(),
            retired_at: None,
        };
        let retired = KeyGrantDto {
            fingerprint: "abc".into(),
            transport_fingerprint: None,
            granted_by: "e1".into(),
            retired_at: Some(1_781_000_000_000),
        };
        assert!(device_line(&active).contains("active"));
        assert!(device_line(&retired).contains("retired"));
        assert!(device_line(&retired).contains("2026"));
    }

    #[test]
    fn a_member_with_every_grant_retired_reads_as_no_active_devices() {
        let m = KeyMemberDto {
            display_name: "Dan".into(),
            email: "d@x.com".into(),
            kind: "human".into(),
            devices: vec![KeyGrantDto {
                fingerprint: "abc".into(),
                granted_by: "e1".into(),
                retired_at: Some(1),
                ..Default::default()
            }],
            revoked: true,
        };
        let summary = member_summary(&m);
        assert!(summary.contains("no active devices"), "{summary}");
        assert!(
            !summary.to_lowercase().contains("removed"),
            "never imply removal: {summary}"
        );
    }

    /// A roster shaped like `keys.json`, with the two kinds side by side.
    fn roster() -> KeysDto {
        let member = |display_name: &str, email: &str, kind: &str| KeyMemberDto {
            display_name: display_name.into(),
            email: email.into(),
            kind: kind.into(),
            devices: Vec::new(),
            revoked: false,
        };
        KeysDto {
            founder_email: "omp@oh-my-pi.dev".into(),
            viewer_email: None,
            viewer_is_founder: false,
            members: vec![
                member("Oh My Pi", "omp@oh-my-pi.dev", "agent"),
                member(
                    "Dan Cieslak",
                    "dcieslak19973@users.noreply.github.com",
                    "human",
                ),
            ],
        }
    }

    #[test]
    fn author_for_never_staples_the_machine_identity_to_another_email() {
        // The exact misattribution dogfooding produced: an annotation signed
        // by the agent but recorded as
        // {"display_name":"Dan Cieslak","email":"omp@oh-my-pi.dev","kind":"Human"}
        // because the name came from this machine's git identity and the kind
        // was hardcoded. The roster is the only authority for both.
        let author = author_for(Some(&roster()), "omp@oh-my-pi.dev");
        assert_eq!(author.display_name, "Oh My Pi");
        assert_eq!(author.email, "omp@oh-my-pi.dev");
        assert_eq!(
            author.kind,
            junto_kernel::MemberKind::Agent,
            "an agent must author as an agent, whoever owns the machine"
        );
    }

    #[test]
    fn author_for_reads_a_human_as_human() {
        let author = author_for(Some(&roster()), "dcieslak19973@users.noreply.github.com");
        assert_eq!(author.display_name, "Dan Cieslak");
        assert_eq!(author.kind, junto_kernel::MemberKind::Human);
    }

    #[test]
    fn author_for_falls_back_to_the_email_and_never_claims_to_be_a_human() {
        // No roster yet (keys.json still in flight), or an email the channel
        // does not list: assert as little as possible rather than inventing a
        // human identity.
        for keys in [None, Some(&roster())] {
            let author = author_for(keys, "stranger@example.com");
            assert_eq!(author.display_name, "stranger@example.com");
            assert_eq!(author.email, "stranger@example.com");
            assert_eq!(author.kind, junto_kernel::MemberKind::Agent);
        }
    }

    #[test]
    fn countdown_reads_down_to_expiry_then_says_expired() {
        assert_eq!(countdown(60_000, 0), "expires in 1:00");
        assert_eq!(countdown(1_000, 0), "expires in 0:01");
        assert_eq!(countdown(0, 0), "expired");
        assert_eq!(countdown(-5_000, 0), "expired");
    }

    #[test]
    fn invite_countdown_live_requires_both_an_open_form_and_an_unexpired_code() {
        assert!(
            !invite_countdown_live(false, Some(60_000), 0),
            "a closed form never ticks, even with an unexpired code retained in state"
        );
        assert!(
            !invite_countdown_live(true, None, 0),
            "an open form with no minted code never ticks"
        );
        assert!(
            !invite_countdown_live(true, Some(0), 0),
            "an open form with an expired code never ticks"
        );
        assert!(
            invite_countdown_live(true, Some(60_000), 0),
            "an open form with an unexpired code ticks"
        );
    }

    /// Build the text frame the host would actually send.
    fn ws_text(
        frame: &WireFrame,
    ) -> Option<
        Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>,
    > {
        Some(Ok(tokio_tungstenite::tungstenite::Message::text(
            serde_json::to_string(frame).expect("WireFrame always serializes"),
        )))
    }

    #[test]
    fn a_pre_challenge_end_closes_quietly_rather_than_failing() {
        // THE BUG, reported from the running app: the host answers a session
        // with no live document by sending `End` before any challenge — its own
        // code calls that "graceful, not a failure" — and the client reported
        // "handshake failed: expected a challenge". Confirmed against the real
        // host: the first and only frame for a landed session is {"t":"end"}.
        // It became visible on every watch once the websocket stopped being
        // remote-only.
        assert!(matches!(
            classify_first_frame(ws_text(&WireFrame::End)),
            FirstFrame::QuietEnd
        ));
    }

    #[test]
    fn a_challenge_starts_the_handshake() {
        let framed = ws_text(&WireFrame::Challenge {
            nonce: "abc123".into(),
        });
        match classify_first_frame(framed) {
            FirstFrame::Challenge(nonce) => assert_eq!(nonce, "abc123"),
            _ => panic!("expected a challenge"),
        }
    }

    #[test]
    fn a_refusal_surfaces_its_reason_but_end_never_does() {
        // A `Rejected` reason is actionable ("not a member", "no such
        // session"); `End` is not, which is the whole distinction.
        match classify_first_frame(ws_text(&WireFrame::Rejected {
            reason: "not a member of this channel".into(),
        })) {
            FirstFrame::Failed(reason) => assert_eq!(reason, "not a member of this channel"),
            _ => panic!("expected a failure"),
        }
    }

    #[test]
    fn a_silent_close_is_nothing_to_watch_not_a_failure() {
        assert!(matches!(classify_first_frame(None), FirstFrame::QuietEnd));
        assert!(matches!(
            classify_first_frame(Some(Ok(tokio_tungstenite::tungstenite::Message::Close(
                None
            )))),
            FirstFrame::QuietEnd
        ));
    }

    #[test]
    fn an_unexpected_frame_is_still_a_handshake_failure() {
        // `AuthOk` before a challenge is genuinely wrong, and must not be
        // swallowed quietly along with `End`.
        match classify_first_frame(ws_text(&WireFrame::AuthOk)) {
            FirstFrame::Failed(reason) => assert!(reason.contains("expected a challenge")),
            _ => panic!("expected a failure"),
        }
    }

    #[test]
    fn artifact_label_names_what_the_artifact_is() {
        // The card used to read `artifact` for all of these, which is what made
        // a diff unfindable in a session record.
        assert_eq!(
            artifact_label("diff: uncommitted changes in D:\\tmp\\demo after turn 1"),
            "diff"
        );
        assert_eq!(artifact_label("memo: I'll make both edits."), "memo");
        assert_eq!(artifact_label("log: turn output"), "log");
        assert_eq!(
            artifact_label("live-snapshot: live session plane snapshot (2323 bytes)"),
            "live-snapshot"
        );
    }

    #[test]
    fn artifact_label_refuses_to_badge_prose_that_happens_to_contain_a_colon() {
        // A memo's own text often has a colon; treating the leading words as a
        // kind would put arbitrary prose in the badge.
        assert_eq!(
            artifact_label("Both edits are done: lib.rs and notes.md"),
            "artifact"
        );
        assert_eq!(artifact_label("no colon at all"), "artifact");
        assert_eq!(artifact_label(""), "artifact");
    }

    /// An artifact entry as `view.json` delivers it.
    fn artifact_entry(id: &str, target: &str, summary: &str) -> EntryDto {
        EntryDto {
            id: id.into(),
            kind: "artifact".into(),
            target: Some(target.into()),
            summary: summary.into(),
            ..sample_entry()
        }
    }

    #[test]
    fn the_newest_diff_of_the_watched_session_is_the_one_to_expand() {
        // Entries arrive in timeline order, so the LAST matching diff is the
        // newest — that is the one a reviewer opening a session wants.
        let entries = vec![
            artifact_entry("a", "s1", "diff: after turn 1"),
            artifact_entry("b", "s1", "memo: some prose"),
            artifact_entry("c", "s1", "diff: after turn 2"),
            artifact_entry("d", "s2", "diff: another session's diff"),
        ];
        assert_eq!(
            newest_diff_artifact(&entries, "s1").map(|e| e.id.as_str()),
            Some("c")
        );
        assert_eq!(
            newest_diff_artifact(&entries, "s2").map(|e| e.id.as_str()),
            Some("d"),
            "another session's diff must not be picked for s1, nor s1's for s2"
        );
    }

    #[test]
    fn a_session_with_no_diff_expands_nothing() {
        // Auto-expanding must be a no-op rather than picking a memo, otherwise
        // opening a session pops open unrelated prose.
        let entries = vec![
            artifact_entry("a", "s1", "memo: some prose"),
            artifact_entry("b", "s1", "live-snapshot: 2323 bytes"),
        ];
        assert!(newest_diff_artifact(&entries, "s1").is_none());
        assert!(newest_diff_artifact(&[], "s1").is_none());
    }

    #[test]
    fn a_memo_renders_formatted_while_reading_and_pointable_while_commenting() {
        // The restriction the deleted `pointability_note` used to explain is
        // gone rather than better worded. A memo is prose, so it renders as
        // Markdown when nobody is commenting; a line is the only thing an anchor
        // can name, so the same memo becomes clickable monospace lines the
        // moment the composer is aimed at anything.
        let memo = "# Findings\n\nThe handshake was mine.\n";
        let md: Vec<markdown::Item> = markdown::parse(memo).collect();
        let (panes, id) = pane_grid::State::new(Pane::loading("c"));
        let pane = panes.get(id).expect("the pane just created");

        let mut reading = iced_test::simulator(artifact_body(
            id,
            "m1",
            "sha256:x",
            "markdown",
            memo,
            Some(&md),
            None,
        ));
        // Rendered Markdown drops the literal "# " of the heading; the raw line
        // is therefore absent exactly while it is unpointable.
        assert!(
            reading.click("# Findings").is_err(),
            "a memo being read is prose, not click targets"
        );

        let aim = Aim {
            path: "",
            record: None,
            span: None,
            popup_at: None,
            hover: None,
            pane,
        };
        let mut commenting = iced_test::simulator(artifact_body(
            id,
            "m1",
            "sha256:x",
            "markdown",
            memo,
            Some(&md),
            Some(aim),
        ));
        commenting
            .click("The handshake was mine.")
            .expect("a memo's lines are pointable while commenting");
        let messages: Vec<Message> = commenting.into_messages().collect();
        assert!(
            messages.iter().any(|m| matches!(
                m,
                Message::AnchorPress(_, AnchorTarget::Record(entry, _), 3) if entry == "m1"
            )),
            "expected a record anchor at line 3 of the memo, got {messages:?}"
        );
    }

    #[test]
    fn aiming_at_one_anchor_kind_clears_the_others() {
        // The corruption risk this guards: an aim left half-set — a stale
        // `annotate_path` beside a fresh `annotate_record` — makes
        // `AnnotateSubmit` sign the WRONG KIND of claim, which is a bad entry in
        // an append-only record rather than a cosmetic bug.
        let mut pane = Pane::loading("c");
        pane.annotate_op = Some(4);

        pane.aim_at(&AnchorTarget::Code("lib.rs".into()), drag_lines(2, 4));
        assert_eq!(pane.annotate_path, "lib.rs");
        assert_eq!(pane.annotate_lines, "2-4");
        assert!(
            pane.annotate_record.is_none(),
            "a code aim clears the record"
        );
        assert!(pane.annotate_op.is_none(), "a code aim clears the stream");

        pane.aim_at(
            &AnchorTarget::Record("a1".into(), "sha256:x".into()),
            drag_lines(7, 7),
        );
        assert_eq!(pane.annotate_record, Some(("a1".into(), "sha256:x".into())));
        assert!(
            pane.annotate_path.is_empty(),
            "a record aim must clear the path, or submit builds a CodeAnchor"
        );
    }

    #[test]
    fn a_drag_is_scoped_to_the_target_it_started_on() {
        // `aimed_key` is what stops a drag crossing from one file (or artifact)
        // into another: a span whose two ends came from different content would
        // be a signed claim about code that never existed.
        let mut pane = Pane::loading("c");
        assert_eq!(pane.aimed_key(), None, "the live stream has no rows");

        pane.aim_at(&AnchorTarget::Code(" lib.rs ".into()), drag_lines(1, 1));
        assert_eq!(
            pane.aimed_key(),
            Some("lib.rs"),
            "the key is trimmed, since the path is also a free text input"
        );

        pane.aim_at(
            &AnchorTarget::Record("a1".into(), "sha256:x".into()),
            drag_lines(1, 1),
        );
        assert_eq!(pane.aimed_key(), Some("a1"));
        assert_ne!(
            pane.aimed_key(),
            Some(AnchorTarget::Code("lib.rs".into()).key()),
            "a drag started in a diff cannot extend into an artifact's text"
        );
    }

    /// Settles, by real headless layout rather than by reading the source,
    /// whether iced 0.14's "Prioritized Shrink over Fill" compression
    /// (CHANGELOG #3045) collapses the center `PaneGrid` to zero height.
    /// `iced_test::simulator` performs REAL layout of the actual root
    /// `App::view()`, so it can answer this directly.
    ///
    /// EMPIRICAL VERDICT: it does not collapse. The proxies are two tagged
    /// containers: `"center-grid-column"`, wrapping `column![grid]`
    /// directly (the innermost point compression could bite), and
    /// `"center-pane-grid"`, one level further out — both `Target::bounds()`
    /// report real height in the default 768px-tall simulator window
    /// (~733px), not zero and not a tab-bar sliver. Asserting on both means
    /// this guard survives a later edit that gives the outer container an
    /// explicit `.height(...)` independent of its content (which would let
    /// the outer alone report a healthy height even if the grid itself had
    /// collapsed).
    ///
    /// `"loading…"` text (`pane_body`'s `Content::Loading` arm) is NOT a
    /// valid proxy for this, despite living deep inside the grid: a plain
    /// `text` widget always reports its own intrinsic glyph size regardless
    /// of how much space its ancestors were given, so an assertion on it
    /// passes or fails identically whether the collapse is real or not —
    /// verified directly: its bounds (height 20.8) were bit-for-bit
    /// identical whether or not the surrounding chain carried an explicit
    /// `.height(Fill)` chain, which proves that proxy measures the text,
    /// never the grid.
    ///
    /// WHY it does not collapse: NOT because a main-axis `Fill` is somehow
    /// immune to compression — it is not. `flex::resolve` genuinely does
    /// propagate main-axis compression to children (`flex.rs:85-88`) and
    /// gates the pass that distributes remaining space to main-axis fills on
    /// `!main_compress` (`flex.rs:186`); a `Fill` child's main-axis size
    /// really would collapse to its intrinsic size under a genuinely
    /// `Shrink` parent (`Limits::resolve`, `limits.rs:167-186`). The reason
    /// is that none of this chain's `column!`/`row!`/`container(...)` calls
    /// stay `Shrink` long enough to arm compression in the first place.
    /// `Column`/`Row::push` (their `column!`/`row!` macros' constructor)
    /// runs `self.height = self.height.enclose(child_size.height)` for
    /// every child (`column.rs:148-149`, `row.rs:139-140`), and
    /// `Length::enclose` promotes a still-`Shrink` parent straight to
    /// whatever `Fill`/`FillPortion` a child asks for
    /// (`iced_core-0.14.0/src/length.rs:61-66`) — so `column![grid]`
    /// (`grid` itself is `.width(Fill).height(Fill)`) is never actually
    /// `Shrink` by the time it reaches layout; it silently becomes `Fill`
    /// the moment `grid` is pushed. `Container::new` does the same thing one
    /// level up: it takes its OWN width/height from its content's
    /// `size_hint().fluid()` (`container.rs:87-97`), so `container(center)`
    /// inherits `Fill` from `center`'s already-promoted height without ever
    /// declaring it explicitly. The shell row promotes the same way from the
    /// blades' explicit `.height(Fill)` containers. `Limits::width`/`height`
    /// only ever arms compression for a length that is STILL `Shrink` when
    /// it reaches them (`limits.rs:55-88`) — and by construction, nothing on
    /// this path ever is.
    #[test]
    fn the_center_pane_grid_gets_real_height_not_a_zero_height_sliver() {
        let (mut app, _) = App::new();
        // Hermetic regardless of this machine's own `<junto-home>/ui.toml`:
        // both blades expanded, default widths, is exactly the layout the
        // reviewer's derivation describes.
        app.shell = shell::ShellState::default();

        let mut ui = iced_test::simulator(app.view());
        let inner = ui
            .find(iced::widget::Id::new("center-grid-column"))
            .expect("the column wrapping the grid must be laid out")
            .bounds();
        let outer = ui
            .find(iced::widget::Id::new("center-pane-grid"))
            .expect("the center container must be laid out")
            .bounds();

        assert!(
            inner.height > 100.0 && outer.height > 100.0,
            "expected the center pane grid to receive real height in a \
             768px-tall window, got inner (wraps column![grid] directly) \
             {inner:?} and outer (one level further out) {outer:?} — the \
             predicted zero-height/sliver collapse",
        );
    }

    /// Settles whether `left_blade`'s `FillPortion`-split nav/body columns
    /// actually observe the persisted `NavSplit` ratio, or whether — per the
    /// review finding — the enclosing `column![...]` being `Shrink` in both
    /// axes leaves `FillPortion` inert. Same method as the Fix-1 test:
    /// `iced_test` real layout, read back through tagged container ids
    /// rather than trusted from source.
    ///
    /// EMPIRICAL VERDICT: it is not inert. Measured ratio is exactly 0.55 —
    /// `NavSplit::DEFAULT` — with the enclosing column left untouched. It is
    /// NOT actually `Shrink` in both axes by the time it reaches layout,
    /// though: by the same `Length::enclose` promotion the Fix-1 test's doc
    /// comment traces (`column.rs:148-149`, `length.rs:61-66`), pushing a
    /// `Length::FillPortion(55)` child promotes this column's declared
    /// `Shrink` height straight to `FillPortion(55)`, and pushing the
    /// `Fill`-width `switcher`/nav row promotes its width to `Fill` too. A
    /// `FillPortion` main-axis child (height, for this `Column`) draws from
    /// the ordinary `available` budget once compression is never armed —
    /// which it is not here, for the same reason as Fix 1.
    #[test]
    fn the_left_blades_nav_and_body_observe_the_persisted_split() {
        let (mut app, _) = App::new();
        app.shell = shell::ShellState::default();
        assert_eq!(
            shell::NavSplit::DEFAULT,
            0.55,
            "this test's ratio assertion assumes the documented default"
        );

        let mut ui = iced_test::simulator(app.view());
        let nav = ui
            .find(iced::widget::Id::new("left-blade-nav"))
            .expect("the left blade's nav container must be laid out")
            .bounds();
        let body = ui
            .find(iced::widget::Id::new("left-blade-body"))
            .expect("the left blade's body container must be laid out")
            .bounds();

        assert!(
            nav.height > 20.0 && body.height > 20.0,
            "expected both the nav and body halves to receive real, non-\
             degenerate height, got nav {nav:?} and body {body:?}"
        );
        // `NavSplit::DEFAULT` (0.55) means the nav half should be
        // moderately taller than the body half, not equal (which is what an
        // inert `FillPortion` — both children falling back to their
        // intrinsic content size — would produce instead by coincidence).
        let ratio = nav.height / (nav.height + body.height);
        assert!(
            (ratio - shell::NavSplit::DEFAULT).abs() < 0.05,
            "expected the nav/body height ratio to track NavSplit::DEFAULT \
             (0.55), got {ratio} from nav {nav:?} and body {body:?} — a \
             FillPortion that inert would not track it at all",
        );
    }
}
