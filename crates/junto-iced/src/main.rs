//! SPIKE — a native junto surface in Iced (`docs/native-ui-toolkit-assessment.md`).
//!
//! A tmux-style **vertical-split pane workspace**: each pane is a junto channel,
//! rendered from the host's structured JSON read-API (`/channels/{name}/view.json`)
//! into native widgets — a **lineage strip** (the split/side-quest history), the
//! party, and the **entry timeline** as colour-coded cards. Type a channel name
//! and `+ pane` to split a column; drag dividers to resize. The point is to feel
//! whether native (Iced) beats the webview as the desktop power-surface.

use std::collections::{HashMap, HashSet};

use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::widget::pane_grid;
use iced::widget::{
    button, checkbox, column, combo_box, container, markdown, pick_list, row, scrollable, text,
    text_input, Space,
};
use iced::{
    Background, Border, Center, Color, Element, Fill, Length, Point, Rectangle, Renderer, Size,
    Task, Theme, mouse,
};
use serde::Deserialize;

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
    iced::application("junto — native spike", App::update, App::view)
        .subscription(App::subscription)
        .theme(|_| Theme::CatppuccinMocha)
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
        .run_with(App::new)
}

struct App {
    /// pane_grid::State is used purely as a keyed store of panes; rendering is
    /// the custom shared-width Columns layout, not a PaneGrid.
    panes: pane_grid::State<Pane>,
    focus: Option<pane_grid::Pane>,
    /// Left-to-right pane order (pane_grid's own iteration isn't ordered).
    order: Vec<pane_grid::Pane>,
    /// Available channel names for the type-ahead picker.
    channels: combo_box::State<String>,
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
    scroll_id: scrollable::Id,
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
}

impl LifecycleKind {
    fn label(self) -> &'static str {
        match self {
            LifecycleKind::Close => "close channel",
            LifecycleKind::Reopen => "reopen channel",
            LifecycleKind::Diverge => "diverge (side-quest)",
            LifecycleKind::Converge => "converge into…",
        }
    }
}

/// The fetch state of an artifact's inline content (`/artifacts/{id}/content.json`).
enum ArtifactContent {
    Loading,
    Loaded {
        format: String,
        body: String,
        /// Parsed Markdown, for memo-format artifacts (parsed once on load).
        md: Option<Vec<markdown::Item>>,
    },
    Error(String),
}

/// The host's artifact content payload.
#[derive(Debug, Clone, Deserialize)]
struct ArtifactDto {
    format: String,
    content: String,
}

#[derive(Debug, Clone)]
enum Message {
    ChannelsLoaded(Vec<String>),
    ChannelPicked(String),
    /// A focus-board chip: open/focus the channel and jump to the entry.
    FocusChipPicked(String, String),
    /// Dismiss the pinned attention card in a pane.
    ClearHighlight(pane_grid::Pane),
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
    // Live session pane.
    Watch(pane_grid::Pane, String),
    /// Close the session view, returning the pane to its timeline.
    CloseSession(pane_grid::Pane),
    Live(String, LiveEvent),
    LiveEnded(String),
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
    /// A lifecycle act finished: Ok(Some(name)) opens that channel; Ok(None)
    /// just refreshes; Err carries the message.
    LifecycleDone(pane_grid::Pane, Result<Option<String>, String>),
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
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let (panes, first) = pane_grid::State::new(Pane::loading("junto-dev"));
        let app = App {
            panes,
            focus: Some(first),
            order: vec![first],
            channels: combo_box::State::new(Vec::new()),
            lineage: None,
            focus_items: Vec::new(),
            agents: Vec::new(),
            recent_workspaces: Vec::new(),
            new_channel: String::new(),
            new_channel_error: None,
            substrates: Vec::new(),
            new_channel_repo: None,
        };
        (
            app,
            Task::batch([
                fetch(first, "junto-dev"),
                fetch_channels(),
                fetch_lineage_graph(),
                fetch_focus(),
                fetch_agents(),
                fetch_workspaces(),
                fetch_substrates(),
            ]),
        )
    }

    /// Focus the existing pane for `name`, or split a new one. Returns the pane
    /// (when resolved) and the fetch task for a freshly-opened pane.
    fn open_or_focus(&mut self, name: &str) -> (Option<pane_grid::Pane>, Task<Message>) {
        if let Some(existing) = self
            .order
            .iter()
            .copied()
            .find(|p| self.panes.get(*p).is_some_and(|s| s.channel == name))
        {
            self.focus = Some(existing);
            return (Some(existing), Task::none());
        }
        let Some(target) = self.focus.or_else(|| self.order.last().copied()) else {
            return (None, Task::none());
        };
        if let Some((new_pane, _)) =
            self.panes
                .split(pane_grid::Axis::Vertical, target, Pane::loading(name))
        {
            self.order.push(new_pane);
            self.focus = Some(new_pane);
            return (Some(new_pane), fetch(new_pane, name));
        }
        (None, Task::none())
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ChannelsLoaded(names) => {
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
                let channel = state.channel.clone();
                state.act_errors.remove(&entry_id); // clear any stale error
                state.act_pending.insert(entry_id.clone()); // show "recording…"
                post_verify(pane, channel, entry_id, act, rationale)
            }
            Message::Acted(pane, entry_id, result) => match result {
                Ok(()) => {
                    let channel = self.panes.get_mut(pane).map(|state| {
                        state.act_pending.remove(&entry_id);
                        state.act_errors.remove(&entry_id);
                        state.act_drafts.remove(&entry_id);
                        state.channel.clone()
                    });
                    channel.map_or_else(Task::none, |c| fetch(pane, &c))
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
                            }
                        }
                        state.content = Content::Loaded(dto);
                        let channel = state.channel.clone();
                        // Jump to the newest entry (bottom) and refresh the brief.
                        Task::batch([
                            scrollable::snap_to(
                                state.scroll_id.clone(),
                                scrollable::RelativeOffset::END,
                            ),
                            fetch_brief(pane, channel),
                        ])
                    }
                    Err(err) => {
                        state.content = Content::Error(err);
                        Task::none()
                    }
                }
            }
            Message::Refresh(pane) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    let channel = state.channel.clone();
                    state.content = Content::Loading;
                    return fetch(pane, &channel);
                }
                Task::none()
            }
            Message::Close(pane) => {
                self.order.retain(|p| *p != pane);
                if let Some((_, sibling)) = self.panes.close(pane) {
                    self.focus = Some(sibling);
                }
                Task::none()
            }
            Message::Watch(pane, session) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.watched = Some(session);
                    state.streaming = true; // try to stream; ends fast if not live
                    state.stream_nonce += 1;
                    state.feed.clear();
                }
                Task::none()
            }
            Message::CloseSession(pane) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.watched = None;
                    state.streaming = false;
                    state.feed.clear();
                }
                Task::none()
            }
            Message::Live(session, event) => {
                let mut scroll = None;
                for (_, state) in self.panes.iter_mut() {
                    if state.watched.as_deref() == Some(session.as_str()) {
                        let item = FeedItem {
                            md: feed_markdown(&event),
                            event,
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
                scroll.map_or_else(Task::none, |id| {
                    scrollable::snap_to(id, scrollable::RelativeOffset::END)
                })
            }
            Message::LiveEnded(session) => {
                let mut to_refresh = None;
                for (pane, state) in self.panes.iter_mut() {
                    if state.watched.as_deref() == Some(session.as_str()) {
                        // Stop streaming but stay on the session, so its landed
                        // record + steer (resume) box remain. Refetch to pick up
                        // the persisted memo/artifacts.
                        state.streaming = false;
                        to_refresh = Some(*pane);
                        break;
                    }
                }
                match to_refresh {
                    Some(pane) => {
                        let channel = self.panes.get(pane).map(|p| p.channel.clone());
                        channel.map_or_else(Task::none, |c| fetch(pane, &c))
                    }
                    None => Task::none(),
                }
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
                let channel = state.channel.clone();
                let agent = state.launch_agent.as_ref().map(|a| a.slug.clone());
                let mode = if state.launch_outcome { "outcome" } else { "single" };
                let workspace = state.launch_workspace.trim().to_string();
                // Keep the intent until the launch succeeds, so a failed launch
                // doesn't lose what was typed.
                state.launching = true;
                state.launch_error = None;
                post_launch(pane, channel, intent, agent, mode, workspace)
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
                        let channel = state.channel.clone();
                        fetch(pane, &channel)
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
                let channel = state.channel.clone();
                fetch_artifact(pane, channel, artifact_id)
            }
            Message::ArtifactLoaded(pane, artifact_id, result) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    let content = match result {
                        Ok(dto) => {
                            let md = (dto.format == "markdown")
                                .then(|| markdown::parse(&dto.content).collect::<Vec<_>>());
                            ArtifactContent::Loaded {
                                format: dto.format,
                                body: dto.content,
                                md,
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
                let (Some(session), text) = (state.watched.clone(), state.steer_text.trim().to_string())
                else {
                    return Task::none();
                };
                if text.is_empty() {
                    return Task::none();
                }
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
                });
                let scroll = state.scroll_id.clone();
                Task::batch([
                    post_steer(pane, channel, session, text),
                    scrollable::snap_to(scroll, scrollable::RelativeOffset::END),
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
                let channel = state.channel.clone();
                post_act(pane, channel, session, "interrupt", None)
            }
            Message::Posted(pane) => {
                let channel = self.panes.get(pane).map(|p| p.channel.clone());
                channel.map_or_else(Task::none, |c| fetch(pane, &c))
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
                    LifecycleKind::Converge => !text.is_empty() && !target.is_empty(),
                };
                if !valid {
                    return Task::none();
                }
                state.lifecycle_pending = true;
                state.lifecycle_error = None;
                post_lifecycle(pane, state.channel.clone(), kind, text, target)
            }
            Message::LifecycleDone(pane, result) => {
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                state.lifecycle_pending = false;
                match result {
                    Ok(open_name) => {
                        state.lifecycle = None;
                        state.lifecycle_error = None;
                        let channel = state.channel.clone();
                        let mut tasks =
                            vec![fetch(pane, &channel), fetch_lineage_graph(), fetch_channels()];
                        // Diverge yields a child to open in its own pane.
                        if let Some(name) = open_name {
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
                    state.brief_md = md
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| markdown::parse(&humanize_brief(&s)).collect());
                }
                Task::none()
            }
            Message::OpenUrl(url) => {
                let _ = open::that(url);
                Task::none()
            }
        }
    }

    fn subscription(&self) -> iced::Subscription<Message> {
        // One SSE subscription per pane that is watching a session.
        let streams: Vec<_> = self
            .panes
            .iter()
            .filter_map(|(_, state)| {
                state
                    .watched
                    .as_ref()
                    .filter(|_| state.streaming)
                    .map(|session| {
                        // Id includes the nonce so a new turn restarts the stream.
                        iced::Subscription::run_with_id(
                            (session.clone(), state.stream_nonce),
                            session_stream(state.channel.clone(), session.clone()),
                        )
                    })
            })
            .collect();
        iced::Subscription::batch(streams)
    }

    fn view(&self) -> Element<'_, Message> {
        let adder_row = row![
            text("open ▸").size(13).color(MUTED),
            combo_box(
                &self.channels,
                "type to search channels…",
                None,
                Message::ChannelPicked,
            )
            .width(260),
            text("· new ▸").size(13).color(MUTED),
            text_input("new channel name…", &self.new_channel)
                .on_input(Message::NewChannelChanged)
                .on_submit(Message::CreateChannel)
                .width(200)
                .padding(6),
        ]
        .spacing(8)
        .align_y(Center);
        // When several substrates are registered, the host needs to know which.
        let adder_row = if self.substrates.len() > 1 {
            adder_row.push(
                pick_list(
                    self.substrates.clone(),
                    self.new_channel_repo.clone(),
                    Message::NewChannelRepoChanged,
                )
                .text_size(12)
                .padding(6),
            )
        } else {
            adder_row
        };
        let adder_row = adder_row.push(button("create").on_press(Message::CreateChannel).padding(6));
        let adder: Element<Message> = match &self.new_channel_error {
            Some(err) => column![adder_row, text(format!("⚠ {err}")).size(11).color(RED)]
                .spacing(4)
                .into(),
            None => adder_row.into(),
        };

        // The always-visible branch graph: the *whole* lineage DAG (every
        // channel, every diverge/converge), with currently-open channels
        // highlighted. Visible regardless of how many panes are open.
        let open: HashSet<String> = self
            .order
            .iter()
            .filter_map(|id| self.panes.get(*id).map(|p| p.channel.clone()))
            .collect();
        let ribbon: Element<Message> = match &self.lineage {
            Some(graph) => {
                // Sticky ambient (row 0) pinned above a taller, vertically-
                // scrollable graph of the whole lineage.
                let canvas = LineageCanvas::layout(graph, &open);
                let content_h = canvas.height.max(60.0);
                // Shorter history scroll; a roomier pinned-ambient strip. Both
                // shrink the band overall so the channel columns get more room.
                let scroll_h = content_h.min(150.0);
                let pinned = Canvas::new(canvas.pinned())
                    .width(Fill)
                    .height(Length::Fixed(48.0));
                let scroll = scrollable(
                    Canvas::new(canvas).width(Fill).height(Length::Fixed(content_h)),
                )
                .height(Fill);
                container(column![pinned, scroll])
                    .width(Fill)
                    .height(Length::Fixed(scroll_h + 56.0))
                    .padding(6)
                    .style(|_theme| container::Style {
                        background: Some(Background::Color(Color { a: 0.4, ..SURFACE })),
                        border: Border {
                            radius: 6.0.into(),
                            ..Border::default()
                        },
                        ..container::Style::default()
                    })
                    .into()
            }
            None => container(text("loading lineage…").size(12).color(MUTED))
                .width(Fill)
                .height(Length::Fixed(40.0))
                .padding(8)
                .into(),
        };

        // The focus board: a visible top banner of cross-channel "needs you"
        // items. Click one to open its channel pane (acting inline is next).
        let focus_inner: Element<Message> = if self.focus_items.is_empty() {
            text("focus · all clear").size(13).color(GREEN).into()
        } else {
            let mut items = row![
                text(format!("needs you ({}) ▸", self.focus_items.len()))
                    .size(13)
                    .color(YELLOW)
            ]
            .spacing(8)
            .align_y(Center);
            for item in &self.focus_items {
                let (tag, color) = match item.kind.as_str() {
                    "gate" => ("gate", YELLOW),
                    "awaiting-execution" => ("exec", MAUVE),
                    _ => ("ratify", BLUE),
                };
                let chan = item.channel_name.clone().unwrap_or_default();
                let label = format!("{tag} · {chan} · {}", truncate(&item.summary, 44));
                let mut chip = button(text(label).size(11)).padding([3, 9]).style(
                    move |_t, _s| chip_style(color, false),
                );
                if let Some(name) = &item.channel_name {
                    chip = chip.on_press(Message::FocusChipPicked(
                        name.clone(),
                        item.entry_id.clone(),
                    ));
                }
                items = items.push(chip);
            }
            // Fixed height taller than the chips so the chips align to the top
            // and the horizontal scrollbar gets its own band below them
            // (otherwise it overlays the chips).
            scrollable(items.padding([6, 0]))
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::default(),
                ))
                .width(Fill)
                .height(Length::Fixed(44.0))
                .into()
        };
        let focus_board = container(focus_inner)
            .width(Fill)
            .center_y(Length::Fixed(56.0))
            .padding([0, 12])
            .style(|_theme| container::Style {
                background: Some(Background::Color(SURFACE)),
                border: Border {
                    color: BORDER,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..container::Style::default()
            });

        // Shared-width columns, reflowing as channels open/close.
        let mut body = row![].spacing(6);
        for id in &self.order {
            if let Some(pane) = self.panes.get(*id) {
                body = body.push(column_pane(*id, pane, &self.agents));
            }
        }

        column![focus_board, adder, ribbon, body.height(Fill)]
            .spacing(10)
            .padding(10)
            .into()
    }
}

/// A pane's title bar (channel name + refresh/close).
fn title_row(id: pane_grid::Pane, pane: &Pane) -> Element<'_, Message> {
    row![
        text(pane.channel.clone()).size(15),
        Space::with_width(Fill),
        button("↻").on_press(Message::Refresh(id)).padding(4),
        button("×").on_press(Message::Close(id)).padding(4),
    ]
    .spacing(6)
    .into()
}

/// One channel pane as a bordered column (custom Columns layout).
fn column_pane<'a>(
    id: pane_grid::Pane,
    pane: &'a Pane,
    agents: &'a [AgentDto],
) -> Element<'a, Message> {
    container(column![title_row(id, pane), pane_body(id, pane, agents)].spacing(8))
        .width(Length::FillPortion(1))
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
fn brief_panel(items: &[markdown::Item]) -> Element<'_, Message> {
    let body = markdown::view(
        items,
        markdown::Settings::default(),
        markdown::Style::from_palette(Theme::CatppuccinMocha.palette()),
    )
    .map(|url| Message::OpenUrl(url.to_string()));
    container(column![text("brief").size(11).color(TEAL), body].spacing(6))
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
fn lifecycle_form(
    id: pane_grid::Pane,
    pane: &Pane,
    kind: LifecycleKind,
) -> Element<'_, Message> {
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
            col = col
                .push(
                    text_input("target channel name…", &pane.lifecycle_target)
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

fn pane_body<'a>(
    id: pane_grid::Pane,
    pane: &'a Pane,
    agents: &'a [AgentDto],
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

    // The channel's own header: the party, a closed badge, and the lifecycle
    // acts (lineage lives in the top window-wide branch graph).
    let mut header = column![].spacing(6);
    let mut party_row = row![].spacing(8).align_y(Center);
    if !dto.party.is_empty() {
        party_row = party_row.push(
            text(format!("party: {}", dto.party.join(", ")))
                .size(12)
                .color(MUTED),
        );
    }
    if dto.closed {
        party_row = party_row.push(badge("closed", RED));
    }
    header = header.push(party_row);
    // Lifecycle act buttons; a closed channel only offers reopen.
    let acts: &[LifecycleKind] = if dto.closed {
        &[LifecycleKind::Reopen]
    } else {
        &[
            LifecycleKind::Diverge,
            LifecycleKind::Converge,
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
        header = header.push(lifecycle_form(id, pane, kind));
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
    let mut launch_btn = button(text(if pane.launching { "launching…" } else { "launch" }))
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
    let mode_checkbox = checkbox("code-PR push-gate (verify loop)", pane.launch_outcome)
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
        header = header.push(Space::with_width(Fill));
        header = header.push(
            button(text("× close").size(11))
                .on_press(Message::CloseSession(id))
                .padding([2, 8])
                .style(|_t, _s| chip_style(MUTED, false)),
        );

        // The session's persisted record: its SessionStarted entry plus every
        // entry targeting it (memos, artifacts), in timeline order.
        let mut record = column![].spacing(8);
        for entry in &dto.entries {
            if entry.id == session_id || entry.target.as_deref() == Some(session_id.as_str()) {
                record = record.push(timeline_entry(
                    id,
                    entry,
                    false,
                    "",
                    None,
                    false,
                    artifact_for(entry),
                    summary_md_for(entry),
                ));
            }
        }
        // The live exchange (your steers + the agent's streaming output). Kept
        // visible after the turn lands until you leave the session.
        if pane.streaming || !pane.feed.is_empty() {
            record = record.push(text("— live turn —").size(11).color(MUTED));
            let mut feed = column![].spacing(6);
            for item in &pane.feed {
                feed = feed.push(feed_line(item));
            }
            if pane.streaming {
                feed = feed.push(text("● working…").size(11).color(YELLOW));
            }
            record = record.push(feed);
        }

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
        column![
            header,
            scrollable(record).id(pane.scroll_id.clone()).height(Fill),
            steer
        ]
        .spacing(8)
        .into()
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
                    Space::with_width(Fill),
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
                        summary_md_for(entry)
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
            timeline = timeline.push(brief_panel(items));
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
            ));
        }
        let scroll = scrollable(timeline)
            .id(pane.scroll_id.clone())
            .height(Fill);
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

/// One live-feed line — kind-coloured, with any HTML the host rendered stripped
/// back to plain text (native can't paint HTML).
fn feed_line(item: &FeedItem) -> Element<'_, Message> {
    // Your own steer messages, echoed as a chat-style "you ›" line.
    if item.event.kind == "you" {
        return container(text(format!("you › {}", item.event.text)).size(13).color(BLUE))
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
        return markdown::view(
            md,
            markdown::Settings::default(),
            markdown::Style::from_palette(Theme::CatppuccinMocha.palette()),
        )
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
    if let Some(digits) = core.strip_prefix('@') {
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    // A content digest, e.g. `sha256:abcd…`.
    if core.contains(':') && core.split(':').next().is_some_and(|a| a == "sha256") {
        return true;
    }
    is_uuid(core) || is_long_hex(core)
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

/// A bare hex run long enough to be an id rather than a word (≥12 chars).
fn is_long_hex(s: &str) -> bool {
    s.len() >= 12 && s.chars().all(|c| c.is_ascii_hexdigit())
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
fn timeline_entry<'a>(
    id: pane_grid::Pane,
    entry: &'a EntryDto,
    highlighted: bool,
    draft: &'a str,
    error: Option<&'a str>,
    pending: bool,
    artifact: Option<&'a ArtifactContent>,
    summary_md: Option<&'a [markdown::Item]>,
) -> Element<'a, Message> {
    row![
        rail(kind_color(&entry.kind)),
        entry_card(id, entry, highlighted, draft, error, pending, artifact, summary_md)
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
    column![Space::with_height(6), dot(color)]
        .align_x(Center)
        .width(Length::Fixed(18.0))
        .into()
}

/// A small filled circle (a history node).
fn dot(color: Color) -> Element<'static, Message> {
    container(Space::new(0.0, 0.0))
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

/// One timeline entry as a card: a colour-coded kind badge + author + status,
/// over the summary text.
fn entry_card<'a>(
    id: pane_grid::Pane,
    entry: &'a EntryDto,
    highlighted: bool,
    draft: &'a str,
    error: Option<&'a str>,
    pending: bool,
    artifact: Option<&'a ArtifactContent>,
    summary_md: Option<&'a [markdown::Item]>,
) -> Element<'a, Message> {
    let accent = kind_color(&entry.kind);
    let mut head = row![badge(&entry.kind, accent), text(entry.author.clone()).size(11).color(MUTED)]
        .spacing(8);
    if let Some(status) = &entry.status {
        head = head.push(badge(status, status_color(status)));
    }
    if entry.unrecognized {
        head = head.push(badge("unrecognized", MUTED));
    }

    // The body: a session memo renders as Markdown; everything else is plain.
    let body: Element<Message> = if let Some(items) = summary_md {
        markdown::view(
            items,
            markdown::Settings::default(),
            markdown::Style::from_palette(Theme::CatppuccinMocha.palette()),
        )
        .map(|url| Message::OpenUrl(url.to_string()))
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
                Space::with_width(Fill),
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
            acts = acts.push(text("recording… (writing to the ledger)").size(11).color(YELLOW));
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
            Some(ArtifactContent::Loaded { format, body, md }) => {
                card = card.push(artifact_body(format, body, md.as_deref()));
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

/// A small filled pill.
/// Render an artifact's content inline: a diff gets per-line add/remove/hunk
/// colour; anything else is shown verbatim. Monospace; long artifacts are
/// truncated (the web view holds the full text).
fn artifact_body<'a>(
    format: &str,
    body: &'a str,
    md: Option<&'a [markdown::Item]>,
) -> Element<'a, Message> {
    // A memo renders as formatted Markdown.
    if let Some(items) = md {
        return container(
            markdown::view(
                items,
                markdown::Settings::default(),
                markdown::Style::from_palette(Theme::CatppuccinMocha.palette()),
            )
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
    const MAX_LINES: usize = 500;
    let lines: Vec<&str> = body.lines().collect();
    let is_diff = format == "diff";
    let mut col = column![].spacing(1);
    for line in lines.iter().take(MAX_LINES) {
        let color = if is_diff { diff_line_color(line) } else { TEXT };
        col = col.push(
            text((*line).to_string())
                .font(iced::Font::MONOSPACE)
                .size(12)
                .color(color),
        );
    }
    if lines.len() > MAX_LINES {
        col = col.push(
            text(format!(
                "… ({} more lines — open in the web view for the full content)",
                lines.len() - MAX_LINES
            ))
            .size(11)
            .color(MUTED),
        );
    }
    container(col)
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

fn badge(label: &str, color: Color) -> Element<'static, Message> {
    container(text(label.to_string()).size(11).color(Color::from_rgb(0.12, 0.12, 0.18)))
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
    fn loading(channel: &str) -> Self {
        Pane {
            channel: channel.to_string(),
            content: Content::Loading,
            watched: None,
            streaming: false,
            feed: Vec::new(),
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
            scroll_id: scrollable::Id::unique(),
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
        }
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
    /// When true, render only the ambient (row 0) track at a fixed y — the
    /// sticky mainline pinned above the scrollable graph.
    pinned: bool,
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
            pinned: false,
        }
    }

    /// A clone that renders only the ambient (row 0) track — the sticky mainline.
    fn pinned(&self) -> Self {
        let mut c = self.clone();
        c.pinned = true;
        c
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

        // Pinned mode: draw only the ambient (row 0) track at a fixed y — the
        // sticky mainline that stays above the scrollable graph.
        if self.pinned {
            if let Some(track) = self.tracks.iter().find(|t| t.row == 0) {
                let y = bounds.height / 2.0;
                let x0 = self.x_of(track.first_ms, left, right);
                let x1 = self.x_of(track.last_ms, left, right).max(x0 + MIN_TRACK);
                frame.stroke(
                    &Path::line(Point::new(x0, y), Point::new(x1, y)),
                    Stroke::default().with_color(TEAL).with_width(3.0),
                );
                frame.fill(&Path::circle(Point::new(x1, y), 5.0), TEAL);
                for (ms, label) in &track.milestones {
                    let mx = self.x_of(*ms, left, right).clamp(x0, x1);
                    frame.fill(&Path::circle(Point::new(mx, y), 2.5), TEXT);
                    if let Some(h) = hover
                        && (h.x - mx).abs() < 5.0
                        && (h.y - y).abs() < 5.0
                    {
                        tooltip = Some((Point::new(mx, y), label.clone()));
                    }
                }
                frame.fill_text(canvas::Text {
                    content: format!("⚓ {}", truncate(&track.name, 18)),
                    position: Point::new(8.0, y - 8.0),
                    color: TEAL,
                    size: 13.0.into(),
                    ..canvas::Text::default()
                });
                draw_tooltip(&mut frame, tooltip, bounds);
            }
            return vec![frame.into_geometry()];
        }

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
            frame.fill(&Path::circle(Point::new(x1, y), if track.open { 5.0 } else { 4.0 }), color);

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

/// Fetch a channel's structured view from the host into `pane`.
fn fetch(pane: pane_grid::Pane, channel: &str) -> Task<Message> {
    let url = format!("{HOST}/channels/{channel}/view.json");
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
fn fetch_artifact(pane: pane_grid::Pane, channel: String, artifact: String) -> Task<Message> {
    let url = format!("{HOST}/channels/{channel}/artifacts/{artifact}/content.json");
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

/// Fetch a channel's curated brief (recall bridge) as Markdown text.
fn fetch_brief(pane: pane_grid::Pane, channel: String) -> Task<Message> {
    let url = format!("{HOST}/channels/{channel}/brief");
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
    iced::stream::channel(64, move |mut output| async move {
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
                if let Some(data) = frame.lines().find_map(|l| l.strip_prefix("data:")) {
                    if let Ok(event) = serde_json::from_str::<LiveEvent>(data.trim()) {
                        let _ = output.send(Message::Live(session.clone(), event)).await;
                    }
                }
            }
        }
        let _ = output.send(Message::LiveEnded(session)).await;
    })
}

/// POST a launch (a new session) for `channel` — intent plus the agent slug,
/// mode (`single`/`outcome`), and optional workspace the picker selected.
fn post_launch(
    pane: pane_grid::Pane,
    channel: String,
    intent: String,
    agent: Option<String>,
    mode: &'static str,
    workspace: String,
) -> Task<Message> {
    let url = format!("{HOST}/channels/{channel}/sessions");
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
    channel: String,
    entry: String,
    act: String,
    rationale: String,
) -> Task<Message> {
    let url = format!("{HOST}/channels/{channel}/entries/{entry}/{act}");
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
    channel: String,
    session: String,
    act: &'static str,
    message: Option<String>,
) -> Task<Message> {
    let url = format!("{HOST}/channels/{channel}/sessions/{session}/{act}");
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
    channel: String,
    kind: LifecycleKind,
    text: String,
    target: String,
) -> Task<Message> {
    let (path, form, open_name) = match kind {
        LifecycleKind::Close => ("close", vec![("rationale", text)], None),
        LifecycleKind::Reopen => ("reopen", vec![("rationale", text)], None),
        LifecycleKind::Diverge => (
            "diverge",
            vec![("child_name", text.clone())],
            Some(text), // open the new side-quest by name
        ),
        LifecycleKind::Converge => (
            "converge",
            vec![("target", target), ("rationale", text)],
            None,
        ),
    };
    let url = format!("{HOST}/channels/{channel}/{path}");
    Task::perform(
        async move {
            match reqwest::Client::new().post(&url).form(&form).send().await {
                Ok(resp) if resp.status().is_success() => Ok(open_name),
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
            match reqwest::Client::new()
                .post(&url)
                .form(&form)
                .send()
                .await
            {
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
    channel: String,
    session: String,
    message: String,
) -> Task<Message> {
    let url = format!("{HOST}/channels/{channel}/sessions/{session}/steer");
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
