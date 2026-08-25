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
    Space, button, checkbox, column, container, markdown, mouse_area, pick_list, row, scrollable,
    text, text_input, tooltip,
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

// The type/spacing system: three text sizes with real gaps and four spacing
// steps on a 4px grid, so hierarchy comes from size + weight + colour-muting
// instead of nine near-identical sizes that all read as the same weight.
/// Meta text: timestamps, authors, counts, badges, chip labels, captions.
/// Usually paired with `MUTED`.
const TEXT_META: f32 = 11.0;
/// Body text: the default — entry text, buttons, inputs, list rows.
const TEXT_BODY: f32 = 13.0;
/// Title text: pane titles and blade section headers. Paired with
/// [`semibold`].
const TEXT_TITLE: f32 = 16.0;
/// Tight spacing: within a row or chip.
const SP_TIGHT: f32 = 4.0;
/// Standard spacing: between siblings.
const SP: f32 = 8.0;
/// Loose spacing: blade and container padding.
const SP_LOOSE: f32 = 12.0;
/// Section spacing: between major regions.
const SP_SECTION: f32 = 16.0;
/// Width of a blade's drag handle. Reserved even when the blade is collapsed
/// so collapsing changes only the blade's own width, never the row's total.
const DIVIDER_W: f32 = 5.0;

/// Width of the tab a collapsed blade leaves on the window edge — just enough
/// to be a click target for the chevron that reopens it.
///
/// Design spec §5 called for collapsing to a ~24px stub rather than to nothing,
/// so a collapsed left blade could still show its attention badge. That reason
/// is gone: attention now lives in the footer's `bell N need you` chip, which
/// is visible whatever either blade is doing, so nothing is hidden by
/// collapsing to an edge tab instead.
const EDGE_TAB_W: f32 = 12.0;

/// Every icon-only button is this square, so a row of them reads as a grid
/// rather than as boxes that each shrank to their own glyph plus padding.
const ICON_BTN: f32 = 24.0;

/// The additional icon family loaded in `main` (`assets/lucide.ttf`, Lucide
/// 0.469.0) — Segoe UI's own punctuation (`‹ › ▸ ▾ ↻ × → ↓ ⚠`) was standing
/// in for icons at text metrics, with mismatched stroke weights and no
/// shared grid; this gives the shell chrome a real icon set instead.
const ICON_FONT: iced::Font = iced::Font::with_name("lucide");

/// One icon glyph at the shell's single icon size (`TEXT_BODY`) — every
/// icon in the chrome sits on the same grid, since inconsistent icon sizing
/// was half of what made the old punctuation-as-icons look hand-drawn.
fn icon(codepoint: char) -> iced::widget::Text<'static> {
    text(codepoint.to_string()).font(ICON_FONT).size(TEXT_BODY)
}

/// Bare icon-button geometry: fixed square, centred glyph, zero padding. No
/// style or tip — `icon_button` layers `ghost_style` and a tooltip on top;
/// the few call sites that need a different look (the config-row "remove"
/// buttons, the annotate popup's "clear aim", the pane title bar's
/// conditionally-disabled close) style themselves directly instead, since
/// `icon_button`'s own return type no longer exposes `.style()`.
fn icon_button_raw<'a>(
    codepoint: char,
    message: Option<Message>,
) -> iced::widget::Button<'a, Message> {
    let glyph: Element<'a, Message> = Element::new(icon(codepoint));
    button(container(glyph).center(Length::Fill))
        .on_press_maybe(message)
        .width(Length::Fixed(ICON_BTN))
        .height(Length::Fixed(ICON_BTN))
        .padding(0)
}

/// Wrap `content` with a tip styled like the file's other elevated surfaces
/// (`SURFACE`/`BORDER`). A tip that renders off the edge of the window is
/// worse than none, so callers near the top of the chrome pass
/// `Position::Bottom` and callers near the bottom pass `Position::Top` — and,
/// the same rule turned sideways, a control flush to the window's left or
/// right edge (`blade_edge_tab`) passes `Position::Right`/`Position::Left`.
fn with_tip<'a>(
    content: impl Into<Element<'a, Message>>,
    tip: &'a str,
    position: tooltip::Position,
) -> Element<'a, Message> {
    tooltip(content, text(tip).size(TEXT_META), position)
        .padding(SP_TIGHT)
        .style(|_theme| container::Style {
            background: Some(Background::Color(SURFACE)),
            border: Border {
                color: BORDER,
                width: 1.0,
                radius: 4.0.into(),
            },
            text_color: Some(TEXT),
            ..container::Style::default()
        })
        .into()
}

/// An icon-only button: fixed square, glyph centred, `ghost_style`, and a
/// tip — so a bare glyph can never ship without a label a user can read
/// (unlike the split/search/plus controls an earlier pass stripped, ledger
/// this task).
fn icon_button<'a>(
    codepoint: char,
    tip: &'a str,
    position: tooltip::Position,
    message: Message,
) -> Element<'a, Message> {
    with_tip(
        icon_button_raw(codepoint, Some(message)).style(|_theme, status| ghost_style(status)),
        tip,
        position,
    )
}

/// A chrome affordance that sits ON the background rather than looking like a
/// button: no fill, no border, the icon muted, and a tint only under the
/// cursor. Orca and xum both render panel toggles and per-pane controls this
/// way — a filled button reads as the most important thing on the strip,
/// which a collapse chevron never is.
fn ghost_style(status: button::Status) -> button::Style {
    let tint = |a: f32| Some(Background::Color(Color { a, ..SURFACE }));
    let (background, text_color) = match status {
        button::Status::Active => (None, MUTED),
        button::Status::Hovered => (tint(0.4), TEXT),
        button::Status::Pressed => (tint(0.6), TEXT),
        button::Status::Disabled => (None, Color { a: 0.4, ..MUTED }),
    };
    button::Style {
        background,
        text_color,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 4.0.into(),
        },
        ..button::Style::default()
    }
}

/// Lucide codepoints for the shell-chrome icons `icon` renders above, taken
/// from the font's own CSS (Lucide 0.469.0, `assets/LICENSE-lucide`).
const ICON_PANEL_LEFT: char = '\u{e12d}';
const ICON_X: char = '\u{e1b1}';
const ICON_ROTATE_CW: char = '\u{e14c}';
const ICON_COLUMNS_2: char = '\u{e09c}';
const ICON_ROWS_2: char = '\u{e43d}';
const ICON_CHEVRON_RIGHT: char = '\u{e073}';
const ICON_CHEVRON_LEFT: char = '\u{e072}';
const ICON_CHEVRON_DOWN: char = '\u{e071}';
const ICON_CIRCLE_ALERT: char = '\u{e07b}';
const ICON_SEARCH: char = '\u{e154}';
const ICON_PLUS: char = '\u{e140}';
const ICON_BELL: char = '\u{e05d}';
const ICON_BOT: char = '\u{e1ba}';
const ICON_FILE_DIFF: char = '\u{e319}';
const ICON_GIT_BRANCH: char = '\u{e0e5}';
const ICON_REFRESH_CW: char = '\u{e148}';

/// The default font at Semibold weight — the one hierarchy tool this pass
/// introduces. Reserved for `TEXT_TITLE`-sized text and blade section
/// labels; everything else stays Normal so the weight keeps meaning.
fn semibold() -> iced::Font {
    iced::Font {
        weight: iced::font::Weight::Semibold,
        ..iced::Font::DEFAULT
    }
}

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
        // The vendored Lucide icon font (ISC-licensed, assets/LICENSE-lucide)
        // — an ADDITIONAL family alongside Segoe UI, not a replacement.
        .font(include_bytes!("../assets/lucide.ttf").as_slice())
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
    /// An in-flight blade-width drag. Transient: deliberately not part of
    /// `ShellState`, since a half-finished gesture is not layout worth
    /// saving.
    blade_drag: Option<BladeDrag>,
    /// The channel whose placement is pending, if any — set by pressing
    /// an unopened channel's chip (`PendingTarget::Channel`), an unopened
    /// attention chip (`Entry`, the entry to jump to), or an unwatched
    /// session chip (`Session`, the session to watch), cleared by
    /// choosing a placement, pressing the chip again, an outside click,
    /// or the target going stale (`clear_stale_session_pending`).
    /// Transient like `blade_drag`: a half-made placement choice is not
    /// layout worth persisting, so this lives on `App`, not `ShellState`.
    pending: Option<PendingOpen>,
    /// The same names as a plain list — the source both the channel chips
    /// and the filter over them (`channel_nav`) render from.
    channel_names: Vec<String>,
    /// The left blade's channel-list filter — a case-insensitive substring
    /// match over `channel_names` (`channel_nav`, `channel_matches`).
    /// Transient like `pending`/`blade_drag`: a half-typed filter is not
    /// layout worth persisting, so this lives on `App`, not `ShellState`.
    channel_filter: String,
    /// The whole lineage DAG, rendered as a vertical list (`lineage_view`).
    lineage: Option<LineageGraphDto>,
    /// Whether the right blade's lineage section is collapsed. Transient
    /// like `channel_filter`/`creating`: a chrome toggle, not layout worth
    /// persisting, so this lives on `App`, not `ShellState`.
    lineage_collapsed: bool,
    /// Node ids whose per-row relations/milestones disclosure is expanded
    /// (`lineage_view`'s per-row chevron). Transient like
    /// `lineage_collapsed`.
    lineage_expanded: HashSet<String>,
    /// Cross-channel "needs you" items — the focus board.
    focus_items: Vec<FocusItem>,
    /// Configured Agents the launch picker offers (`/agents.json`).
    agents: Vec<AgentDto>,
    /// Distinct workspace repos, most-recent first — the inferred launch default.
    recent_workspaces: Vec<String>,
    /// Whether the left blade's create-channel form is open — toggled by
    /// the header's `plus` (`Message::ToggleCreating`). Transient like
    /// `channel_filter`: not part of `ShellState`.
    creating: bool,
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

// ---- pure lineage-graph logic: hierarchical row order, lane assignment,
// per-row rail geometry, structural relations, and a node's role —
// decidable from `LineageGraphDto` alone, with no widget or `App` state,
// and tested as such (`lineage_tests`) before `lineage_view` (the
// vertical-list rewrite of the old horizontal `LineageCanvas`) ever
// touches them. ----

/// Comparator for every level of `lineage_hierarchy`'s order — roots
/// against each other, and each node's children against their siblings:
/// newest `last_ms` first. A node's `last_ms` is `None` when its channel
/// has no recorded entries yet — treated as the oldest possible activity
/// (`i64::MIN`), on the reasoning that "no evidence of recent activity"
/// belongs at the bottom of a newest-first list, not jumping to the top.
/// Ties (equal `last_ms`, including two `None`s) break on `name` so the
/// list never silently reorders between two frames over identical data.
fn lineage_activity_order(a: &&GNode, b: &&GNode) -> std::cmp::Ordering {
    let a_key = a.last_ms.unwrap_or(i64::MIN);
    let b_key = b.last_ms.unwrap_or(i64::MIN);
    b_key.cmp(&a_key).then_with(|| a.name.cmp(&b.name))
}

/// One node's structural relations within the lineage DAG, as borrowed
/// node ids — never a formatted string; `lineage_view` owns turning these
/// into words.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct LineageRelations<'a> {
    /// The `from` of the one `diverge` edge pointing at this node, if any.
    parent: Option<&'a str>,
    /// The `to` of every `diverge` edge FROM this node — other channels
    /// that branched off it.
    children: Vec<&'a str>,
    /// The `to` of this node's own outgoing `converge` edge, if its thread
    /// merged back into another channel.
    converged_into: Option<&'a str>,
}

/// Scans `graph`'s edges for everything touching `id`. `relation` is only
/// ever `"diverge"` or `"converge"` in the live data; anything else (there
/// is none today) is silently ignored rather than treated as a parse
/// error, since this walks already-deserialized JSON, not the wire itself.
fn node_relations<'a>(graph: &'a LineageGraphDto, id: &str) -> LineageRelations<'a> {
    let mut relations = LineageRelations::default();
    for edge in &graph.edges {
        match edge.relation.as_str() {
            "diverge" if edge.to == id => relations.parent = Some(edge.from.as_str()),
            "diverge" if edge.from == id => relations.children.push(edge.to.as_str()),
            "converge" if edge.from == id => relations.converged_into = Some(edge.to.as_str()),
            _ => {}
        }
    }
    relations
}

/// A node's structural role in the lineage DAG — what `lineage_view` used
/// to pick the rail glyph from, and what `lineage_rail_rows` now bundles
/// into each row's description — computed from a node's own
/// `LineageRelations` rather than re-scanning the graph, so it's testable
/// in isolation.
///
/// A node can satisfy more than one of these at once — the real data's own
/// hub channel is both a root (no parent) AND a fork (six diverges out).
/// Precedence, checked in this order: `Fork` first, since other channels
/// branching off a node is the most structurally significant fact about
/// it and must never be hidden behind a rarer one; then `Converged`; then
/// `Root`; `Ordinary` is the fallback once none apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LineageRole {
    /// No incoming diverge — nothing forked this channel off another.
    /// Also `LineageRailRow`'s own `#[default]`: the closest thing to
    /// "no relations known" if a row's rail ever had to fall back.
    #[default]
    Root,
    /// At least one outgoing diverge — other channels forked off THIS one.
    Fork,
    /// An outgoing converge — this channel's thread merged into another.
    Converged,
    /// A parented, non-forking, non-converging interior node.
    Ordinary,
}

fn node_role(relations: &LineageRelations) -> LineageRole {
    if !relations.children.is_empty() {
        LineageRole::Fork
    } else if relations.converged_into.is_some() {
        LineageRole::Converged
    } else if relations.parent.is_none() {
        LineageRole::Root
    } else {
        LineageRole::Ordinary
    }
}

/// A node's owner for the hierarchical row order (`lineage_hierarchy`) and
/// lane assignment (`lineage_lanes`): its diverge parent if it has one,
/// else its converge target — so a node with no diverge edge at all (the
/// live data's `pointing-dogfood-20260823`, which only converges) still
/// sits with the channel it reconnects into instead of stranded among
/// unrelated roots. `None` only for a genuine root: no diverge parent AND
/// no converge target.
fn lineage_owner<'a>(graph: &'a LineageGraphDto, id: &str) -> Option<&'a str> {
    let relations = node_relations(graph, id);
    relations.parent.or(relations.converged_into)
}

/// The mutable accumulators `lineage_visit`'s DFS threads through the
/// hierarchy walk, bundled into one `&mut` argument (rather than four)
/// the same way `LineageRowState` bundles a row's presentation state —
/// keeps the recursive call well under clippy's argument limit without
/// an `#[allow]`.
#[derive(Default)]
struct LineageWalk<'a> {
    visited: HashSet<&'a str>,
    order: Vec<&'a GNode>,
    /// Each visited node's own subtree span: the index, into `order`, of
    /// the last row inside that node's own subtree — `lineage_rail_rows`
    /// uses this to know when an ancestor's branch has no more rows left.
    ends: HashMap<&'a str, usize>,
    /// Each visited node's REALIZED structural parent — absent for a
    /// root, and (this is what makes cycle-breaking sound for the rail,
    /// not just for termination) absent for whichever side of a broken
    /// cycle got visited first, since that side is never actually
    /// nested under the other in the emitted order.
    parents: HashMap<&'a str, &'a str>,
}

/// Depth-first-emits `node` and its children (from the static `children`
/// adjacency, already sorted newest-first) into `walk`, recording the
/// parent it was actually reached through — `None` for a root or a
/// cycle-breaking re-entry point.
///
/// The data is a DAG in practice, but a converge-derived owner could in
/// principle close a loop (two nodes converging into each other with
/// neither having a diverge parent, so each "owns" the other per
/// `lineage_owner`). `walk.visited`'s guard breaks that loop rather than
/// recursing forever: a node already visited returns immediately, before
/// `parents` is touched, so the cycle's second edge is simply never
/// realized.
fn lineage_visit<'a>(
    node: &'a GNode,
    parent: Option<&'a str>,
    children: &HashMap<&'a str, Vec<&'a GNode>>,
    walk: &mut LineageWalk<'a>,
) {
    if !walk.visited.insert(node.id.as_str()) {
        return;
    }
    if let Some(parent) = parent {
        walk.parents.insert(node.id.as_str(), parent);
    }
    walk.order.push(node);
    if let Some(kids) = children.get(node.id.as_str()) {
        for child in kids {
            lineage_visit(child, Some(node.id.as_str()), children, walk);
        }
    }
    walk.ends.insert(node.id.as_str(), walk.order.len() - 1);
}

/// The hierarchical row order, plus the structure `lineage_lanes` and
/// `lineage_rail_rows` need afterward. Each subtree is contiguous — a
/// root (no `lineage_owner`), newest-first, immediately followed by its
/// descendants depth-first, each level's siblings also newest-first — so
/// a branch and everything that diverged from or converges into it sit
/// near each other instead of scattered by raw activity time.
///
/// A cycle (`lineage_visit`'s own doc comment covers how) can leave nodes
/// unreachable from any real root; anything `walk.visited` doesn't cover
/// once every root is drained is emitted afterward as its own root,
/// newest-first, so a cycle breaks rather than a node silently vanishing.
fn lineage_hierarchy(
    graph: &LineageGraphDto,
) -> (Vec<&GNode>, HashMap<&str, usize>, HashMap<&str, &str>) {
    let by_id: HashMap<&str, &GNode> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let mut children: HashMap<&str, Vec<&GNode>> = HashMap::new();
    let mut roots: Vec<&GNode> = Vec::new();
    for node in &graph.nodes {
        match lineage_owner(graph, &node.id).filter(|owner| by_id.contains_key(*owner)) {
            Some(owner) => children.entry(owner).or_default().push(node),
            None => roots.push(node),
        }
    }
    roots.sort_by(lineage_activity_order);
    for siblings in children.values_mut() {
        siblings.sort_by(lineage_activity_order);
    }

    let mut walk = LineageWalk::default();
    for root in &roots {
        lineage_visit(root, None, &children, &mut walk);
    }
    let mut leftover: Vec<&GNode> = graph
        .nodes
        .iter()
        .filter(|node| !walk.visited.contains(node.id.as_str()))
        .collect();
    leftover.sort_by(lineage_activity_order);
    for node in leftover {
        lineage_visit(node, None, &children, &mut walk);
    }

    (walk.order, walk.ends, walk.parents)
}

/// Cap on the rail's rendered depth (`lineage_lanes`): the live graph's
/// deepest chain is 3 hops from a root, so 4 lanes (indices `0..MAX_LANE`)
/// already covers it with a spare lane, while keeping the rail
/// (`LANE_W * MAX_LANE` wide — `lineage_rail_cell`'s own doc comment
/// covers the pixel budget) from growing arbitrarily wide against a
/// pathological long diverge chain. A node deeper than the cap is drawn
/// in the last lane; `LineageRelations::parent` still names its real
/// diverge parent for the expanded detail — only the rail's own drawing
/// is clamped, never the DAG data.
const MAX_LANE: usize = 4;

/// Which lanes have a through-line in one row — a bitmask rather than a
/// `Vec`, since `MAX_LANE` bounds it to a handful of bits and keeping it a
/// plain value keeps `LineageRailRow` (and therefore `LineageRowState`)
/// `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct LaneSet(u8);

impl LaneSet {
    fn with(mut self, lane: usize) -> Self {
        self.0 |= 1 << lane;
        self
    }

    fn contains(self, lane: usize) -> bool {
        self.0 & (1 << lane) != 0
    }

    /// Every set lane, ascending — what `lineage_rail_cell` iterates to
    /// draw one through-line per lane.
    fn lanes(self) -> impl Iterator<Item = usize> {
        (0..MAX_LANE).filter(move |&lane| self.contains(lane))
    }
}

/// Rendered lane for each node — depth in the owner tree
/// (`lineage_hierarchy`'s own doc comment covers "owner"), capped at
/// `MAX_LANE - 1`. `order` must already be hierarchical
/// (`lineage_hierarchy`'s output) so a node's structural parent always
/// has a lane recorded before its children are visited; a node on the
/// cycle-broken side of a loop has no `parents` entry at that point and
/// falls back to lane 0, the same treatment a genuine root gets.
fn lineage_lanes<'a>(
    order: &[&'a GNode],
    parents: &HashMap<&'a str, &'a str>,
) -> HashMap<&'a str, usize> {
    let mut lanes: HashMap<&str, usize> = HashMap::new();
    for node in order {
        let lane = parents
            .get(node.id.as_str())
            .copied()
            .and_then(|parent| lanes.get(parent))
            .map(|&parent_lane| (parent_lane + 1).min(MAX_LANE - 1))
            .unwrap_or(0);
        lanes.insert(node.id.as_str(), lane);
    }
    lanes
}

/// What one lineage row's rail cell must draw — the pure geometry
/// `lineage_rail_cell`'s `canvas::Program` turns into paint calls.
/// Everything here is a lane index or a role; the canvas program owns
/// turning a lane into an x coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct LineageRailRow {
    /// This row's own lane, already clamped to `MAX_LANE - 1`.
    lane: usize,
    role: LineageRole,
    /// Lanes with a full-height vertical line through this row: a real
    /// ancestor (by the owner tree, never by lane-number reuse between
    /// unrelated siblings) whose own subtree still has rows after this
    /// one.
    through: LaneSet,
    /// This row peels off from its owner's lane, drawn from the cell's
    /// top edge into this row's own lane at the vertical centre. `None`
    /// only for a root — every non-root row starts a branch here,
    /// whether the owning edge was a real diverge or a converge-fallback
    /// (`lineage_owner`'s own doc comment covers the trap that makes
    /// this matter: a node can reconnect to something it never forked
    /// from).
    branch_from: Option<usize>,
    /// This row's own outgoing `converge` edge lands in this lane, drawn
    /// from this row's own lane at the vertical centre curving to the
    /// cell's bottom edge. `None` unless the DAG has a real converge
    /// edge FROM this node whose target is a lane this layout knows
    /// about.
    converge_to: Option<usize>,
}

/// Builds every row's `LineageRailRow` in one pass over `order` (already
/// hierarchical) plus each node's `ends`/`parents`/`lanes` entries from
/// `lineage_hierarchy`/`lineage_lanes`.
fn lineage_rail_rows<'a>(
    graph: &'a LineageGraphDto,
    order: &[&'a GNode],
    ends: &HashMap<&'a str, usize>,
    parents: &HashMap<&'a str, &'a str>,
    lanes: &HashMap<&'a str, usize>,
) -> HashMap<&'a str, LineageRailRow> {
    let mut rows = HashMap::with_capacity(order.len());
    for (index, node) in order.iter().enumerate() {
        let relations = node_relations(graph, &node.id);
        let role = node_role(&relations);
        let lane = lanes.get(node.id.as_str()).copied().unwrap_or(0);

        // `parents` is a forest (each entry set exactly once, strictly
        // before its own children are visited), so walking it can never
        // cycle — no separate guard needed here.
        let mut through = LaneSet::default();
        let mut ancestor = parents.get(node.id.as_str()).copied();
        while let Some(id) = ancestor {
            if ends.get(id).is_some_and(|&end| end > index) {
                through = through.with(lanes.get(id).copied().unwrap_or(0));
            }
            ancestor = parents.get(id).copied();
        }

        let branch_from = parents
            .get(node.id.as_str())
            .copied()
            .and_then(|parent| lanes.get(parent))
            .copied();
        let converge_to = relations
            .converged_into
            .and_then(|target| lanes.get(target))
            .copied();

        rows.insert(
            node.id.as_str(),
            LineageRailRow {
                lane,
                role,
                through,
                branch_from,
                converge_to,
            },
        );
    }
    rows
}

/// Everything `lineage_view` needs to lay out and draw the rail in one
/// call: the hierarchical row order, and each node's `LineageRailRow`.
struct LineageLayout<'a> {
    order: Vec<&'a GNode>,
    rails: HashMap<&'a str, LineageRailRow>,
}

fn lineage_layout(graph: &LineageGraphDto) -> LineageLayout<'_> {
    let (order, ends, parents) = lineage_hierarchy(graph);
    let lanes = lineage_lanes(&order, &parents);
    let rails = lineage_rail_rows(graph, &order, &ends, &parents, &lanes);
    LineageLayout { order, rails }
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

/// What a landed `PendingOpen` should do once its channel is on screen:
/// nothing (a channel chip), jump to a carried entry (an attention chip),
/// or start watching a carried session (a session chip). An enum, not a
/// second `Option` field beside one already on `PendingOpen` — a pending
/// only ever carries at most one target kind, and two parallel `Option`s
/// would let both or neither be set, the illegal state this replaces.
#[derive(Debug, Clone, PartialEq)]
enum PendingTarget {
    /// Just open the channel — nothing further to do.
    Channel,
    /// Jump to this entry once the channel lands (an attention chip).
    Entry(String),
    /// Watch this session once the channel lands (a session chip).
    Session(String),
}

/// A channel the user has chosen to open but not yet placed, plus what to
/// do once it lands — the one mechanism `App::pending` uses for an
/// unopened channel chip's floating menu, an unopened attention chip's
/// inline row, and an unwatched session chip's inline row alike.
#[derive(Debug, Clone, PartialEq)]
struct PendingOpen {
    channel: String,
    target: PendingTarget,
}

/// Where an unopened channel's chip should dock when pressed — the explicit
/// placement choice `Message::ChannelToggled` offers via a floating menu
/// (`placement_menu`) instead of always splitting the focused pane
/// vertically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// Split the focused pane to the right.
    Right,
    /// Split the focused pane downward.
    Below,
    /// Reuse the focused pane, replacing the channel it shows.
    Here,
}

impl Placement {
    /// The split axis for this placement, or `None` for `Here` — which
    /// reuses the focused pane in place rather than splitting it.
    fn axis(self) -> Option<pane_grid::Axis> {
        match self {
            Placement::Right => Some(pane_grid::Axis::Vertical),
            Placement::Below => Some(pane_grid::Axis::Horizontal),
            Placement::Here => None,
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    ChannelsLoaded(Vec<String>),
    /// Type into the left blade's channel filter — a case-insensitive
    /// substring match over the chip list (`channel_matches`).
    ChannelFilterChanged(String),
    /// Press the left blade header's `plus`: open or close the
    /// create-channel form beneath it.
    ToggleCreating,
    /// Press a channel chip: close its pane if the channel is open; else
    /// open (or, pressed again, close) that channel's placement menu.
    ChannelToggled(String),
    /// Pick a placement for a pending channel — from an unopened
    /// channel's floating menu, an unopened attention chip's inline row,
    /// or an unwatched session chip's inline row.
    ChannelPlaced(PendingOpen, Placement),
    /// A focus-board chip whose channel is already open: focus that pane
    /// and jump to the entry. Unlike a channel chip this never toggles a
    /// pane closed — an attention item always means "take me there".
    FocusChipPicked(String, String),
    /// A focus-board chip whose channel isn't open yet: open (or, pressed
    /// again, close) its inline placement row.
    FocusChipToggled(String, String),
    /// A session chip whose session isn't watched yet: open (or, pressed
    /// again, close) its inline placement row (channel, session id).
    SessionToggled(String, String),
    /// Dismiss the pinned attention card in a pane.
    ClearHighlight(pane_grid::Pane),
    /// Collapse or expand the left blade.
    ToggleLeftBlade,
    /// Collapse or expand the right blade.
    ToggleRightBlade,
    /// Switch the right blade's view.
    RightViewPicked(shell::RightView),
    /// Press a footer trigger chip: toggle that floating panel open/closed.
    BottomViewToggled(shell::BottomView),
    /// Press a blade-width divider handle: begin a resize drag.
    BladeDragStart(Side),
    /// The cursor moved during an in-flight blade-width drag (absolute
    /// window coordinates, from the global `CursorMoved` event).
    BladeDragMoved(Point),
    /// Release the mouse button, ending an in-flight blade-width drag.
    BladeDragEnd,
    /// Double-click a divider handle: reset that blade to its default width.
    BladeReset(Side),
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
    /// Press the lineage header's chevron: collapse/expand the section.
    ToggleLineageCollapsed,
    /// Press a lineage row's disclosure chevron: expand/collapse that
    /// node's relations + milestones detail (the node's id).
    LineageRowToggled(String),
    /// Press the lineage header's refresh icon: refetch just the lineage DAG.
    RefreshLineage,
    /// Press a lineage row's channel name: focus its pane if already open,
    /// else open (or, pressed again, close) a placement menu for it —
    /// exactly `Message::ChannelToggled`'s own logic, except an
    /// already-open channel is FOCUSED rather than closed, since a
    /// history view should always take you there
    /// (`Message::FocusChipPicked`'s own reasoning for attention chips).
    LineageChannelPicked(String),
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
            channel_names: Vec::new(),
            channel_filter: String::new(),
            lineage: None,
            lineage_collapsed: false,
            lineage_expanded: HashSet::new(),
            focus_items: Vec::new(),
            agents: Vec::new(),
            recent_workspaces: Vec::new(),
            creating: false,
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
            blade_drag: None,
            pending: None,
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
        let existing = self
            .panes
            .iter()
            .find(|(_, state)| state.channel == name)
            .map(|(id, _)| *id);
        if let Some(existing) = existing {
            self.focus_pane(existing);
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
        self.split_channel(pane_grid::Axis::Vertical, target, name)
    }

    /// Split `target` on `axis`, adding a fresh pane for `name` and fetching
    /// its view. Returns the new pane (when the split succeeded) and its
    /// fetch task — the "make a pane for this channel and fetch it" step
    /// shared by `open_or_focus` (always `Axis::Vertical`) and
    /// `Message::ChannelPlaced`'s right/below placements (whichever axis the
    /// menu picked), so it exists exactly once rather than copy-pasted.
    fn split_channel(
        &mut self,
        axis: pane_grid::Axis,
        target: pane_grid::Pane,
        name: &str,
    ) -> (Option<pane_grid::Pane>, Task<Message>) {
        if let Some((new_pane, _)) = self.panes.split(axis, target, Pane::loading(name)) {
            self.focus_pane(new_pane);
            return (Some(new_pane), fetch(new_pane, HOST.to_string(), name));
        }
        (None, Task::none())
    }

    /// Show the timeline (not a live feed) so a jump-to entry's card is
    /// visible, and pin it to the top of `pane` — the two steps
    /// `FocusChipPicked` (channel already open) and `Message::ChannelPlaced`
    /// (once an attention chip's pending channel lands) both need, factored
    /// once rather than repeated at each call site.
    fn pin_entry(&mut self, pane: pane_grid::Pane, entry: String) {
        if let Some(state) = self.panes.get_mut(pane) {
            state.watched = None;
            state.highlight_entry = Some(entry);
        }
    }

    /// Apply a placed pending's post-open step to the pane it landed in:
    /// pin an attention chip's entry (`pin_entry`), or start watching a
    /// session chip's session (`watch`) so a freshly split pane opens
    /// straight into its live view. A bare channel-chip target needs
    /// nothing further — placing it already put the right channel on
    /// screen. Shared by `Message::ChannelPlaced`'s no-focus fallback and
    /// its right/below split; its "here" placement takes a separate path
    /// for a `Session` target (see that arm), since reusing the focused
    /// pane needs no split or refetch to begin with.
    fn apply_pending_target(&mut self, pane: pane_grid::Pane, target: PendingTarget) {
        match target {
            PendingTarget::Channel => {}
            PendingTarget::Entry(entry) => self.pin_entry(pane, entry),
            PendingTarget::Session(session) => self.watch(pane, session),
        }
    }

    /// Start watching `session` in `pane`: `Message::Watch`'s own reset
    /// (new turn, cleared feed/composer state) rather than merely setting
    /// `watched` — `App::subscription` gates the live socket on BOTH
    /// `watched` AND `streaming`, so a bare field set would show the
    /// session but never actually stream it. Shared by the message
    /// handler itself and by a session pending's placement
    /// (`apply_pending_target`, and `Message::ChannelPlaced`'s "here"
    /// case directly), so a freshly split or reused pane opens straight
    /// into a real live view, not a static one.
    fn watch(&mut self, pane: pane_grid::Pane, session: String) {
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
            // A block index belongs to the document being left; the feed
            // is cleared here, so keeping it would aim at an op from a
            // different session.
            state.annotate_op = None;
            // A different session has a different newest diff.
            state.auto_expanded = false;
        }
    }

    /// Focus `pane`, clearing a session `pending` the new focus makes
    /// stale (`clear_stale_session_pending`). Every `self.focus = Some(…)`
    /// assignment in the file routes through this, so a session's inline
    /// placement row can never survive on screen for a pane the user has
    /// since looked away from.
    fn focus_pane(&mut self, pane: pane_grid::Pane) {
        self.focus = Some(pane);
        self.clear_stale_session_pending();
    }

    /// Clears `pending` when it names a session no longer valid for the
    /// focused pane: the session has left that pane's `session_list()`, or
    /// focus has moved to a pane on a different channel altogether. A
    /// session pending has no board of its own to recheck against (unlike
    /// an attention pending, cleared in `FocusLoaded` when its channel
    /// drops off the focus board) — the focused pane's own live session
    /// list IS that check, so this reruns wherever it can change: a pane
    /// refetch (`Message::Fetched`) and a focus change (`focus_pane`).
    fn clear_stale_session_pending(&mut self) {
        let Some(PendingOpen {
            channel,
            target: PendingTarget::Session(session),
        }) = &self.pending
        else {
            return;
        };
        let still_valid = self
            .focus
            .and_then(|id| self.panes.get(id))
            .is_some_and(|pane| {
                &pane.channel == channel && pane.session_list().iter().any(|s| &s.id == session)
            });
        if !still_valid {
            self.pending = None;
        }
    }

    /// The shared half of `Message::ChannelToggled`'s and
    /// `Message::LineageChannelPicked`'s logic: if `name` already has a
    /// pane, return it — the caller decides what "already open" means
    /// (close it, for a channel chip; focus it, for a lineage row).
    /// Otherwise, opens or closes a `PendingTarget::Channel` placement
    /// menu for it, exactly like an unopened channel chip, and returns
    /// `None`.
    fn open_pane_or_toggle_pending(&mut self, name: String) -> Option<pane_grid::Pane> {
        let existing = self
            .panes
            .iter()
            .find(|(_, state)| state.channel == name)
            .map(|(id, _)| *id);
        if existing.is_some() {
            return existing;
        }
        if self.pending.as_ref().is_some_and(|pending| {
            matches!(&pending.target, PendingTarget::Channel) && pending.channel == name
        }) {
            self.pending = None;
        } else {
            self.pending = Some(PendingOpen {
                channel: name,
                target: PendingTarget::Channel,
            });
        }
        None
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ChannelsLoaded(names) => {
                self.channel_names = names;
                Task::none()
            }
            Message::LineageGraphLoaded(graph) => {
                self.lineage = graph;
                Task::none()
            }
            Message::ToggleLineageCollapsed => {
                self.lineage_collapsed = !self.lineage_collapsed;
                Task::none()
            }
            Message::LineageRowToggled(id) => {
                if !self.lineage_expanded.remove(&id) {
                    self.lineage_expanded.insert(id);
                }
                Task::none()
            }
            Message::RefreshLineage => fetch_lineage_graph(),
            Message::LineageChannelPicked(name) => {
                if let Some(pane) = self.open_pane_or_toggle_pending(name) {
                    self.focus_pane(pane);
                }
                Task::none()
            }
            Message::FocusLoaded(items) => {
                self.focus_items = items;
                // An attention-originated pending (an `Entry` target)
                // whose channel has scrolled off the board would otherwise
                // render an orphaned inline placement row with nothing
                // left to explain it; a channel or session chip's pending
                // is unrelated to the board and is left alone.
                if let Some(pending) = &self.pending
                    && matches!(&pending.target, PendingTarget::Entry(_))
                    && !self
                        .focus_items
                        .iter()
                        .any(|item| item.channel_name.as_deref() == Some(pending.channel.as_str()))
                {
                    self.pending = None;
                }
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
            Message::ChannelFilterChanged(filter) => {
                self.channel_filter = filter;
                Task::none()
            }
            Message::ToggleCreating => {
                self.creating = !self.creating;
                Task::none()
            }
            Message::ChannelToggled(name) => {
                // `State::close` removes nothing and returns `None` for the
                // last remaining pane; `channel_nav` disables the chip's
                // press in that case (mirroring the title bar's own `×`
                // guard) rather than reaching this dead end.
                if let Some(pane) = self.open_pane_or_toggle_pending(name)
                    && let Some((_, sibling)) = self.panes.close(pane)
                {
                    self.focus_pane(sibling);
                }
                Task::none()
            }
            Message::ChannelPlaced(pending, placement) => {
                self.pending = None;
                let PendingOpen {
                    channel: name,
                    target,
                } = pending;
                // Every placement here is meaningless with nothing focused
                // to split or replace — degrade all three to the plain
                // open/focus path (`open_or_focus`'s own behaviour) rather
                // than presenting a choice that has nothing to act on.
                let Some(focused) = self.focus else {
                    let (pane, task) = self.open_or_focus(&name);
                    if let Some(pane) = pane {
                        self.apply_pending_target(pane, target);
                    }
                    return task;
                };
                match placement.axis() {
                    Some(axis) => {
                        let (pane, task) = self.split_channel(axis, focused, &name);
                        if let Some(pane) = pane {
                            self.apply_pending_target(pane, target);
                        }
                        task
                    }
                    None => {
                        // A session pending's channel is always already
                        // showing in the focused pane — sessions are only
                        // ever listed for it — so "here" needs no pane
                        // replace or refetch, just the same in-place watch
                        // an ordinary session chip's press performs.
                        if let PendingTarget::Session(session) = target {
                            self.watch(focused, session);
                            return Task::none();
                        }
                        // "here": replace the focused pane's channel in
                        // place, keeping its remote-host override (a
                        // property of the pane's target machine, not of
                        // what it shows) but resetting every other
                        // per-pane view/form field to a fresh
                        // `Pane::loading`, since none of it describes the
                        // new channel.
                        let Some(state) = self.panes.get_mut(focused) else {
                            return Task::none();
                        };
                        let remote = state.remote.clone();
                        let base = state.base().to_string();
                        *state = Pane::loading(&name);
                        state.remote = remote;
                        self.apply_pending_target(focused, target);
                        fetch(focused, base, &name)
                    }
                }
            }
            Message::FocusChipPicked(name, entry_id) => {
                let (pane, task) = self.open_or_focus(&name);
                if let Some(pane) = pane {
                    self.pin_entry(pane, entry_id);
                }
                task
            }
            Message::FocusChipToggled(name, entry_id) => {
                if self.pending.as_ref().is_some_and(|pending| {
                    matches!(&pending.target, PendingTarget::Entry(id) if id == &entry_id)
                }) {
                    self.pending = None;
                } else {
                    self.pending = Some(PendingOpen {
                        channel: name,
                        target: PendingTarget::Entry(entry_id),
                    });
                }
                Task::none()
            }
            Message::SessionToggled(channel, session) => {
                if self.pending.as_ref().is_some_and(|pending| {
                    pending.channel == channel
                        && matches!(&pending.target, PendingTarget::Session(id) if id == &session)
                }) {
                    self.pending = None;
                } else {
                    self.pending = Some(PendingOpen {
                        channel,
                        target: PendingTarget::Session(session),
                    });
                }
                Task::none()
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
            Message::BladeDragStart(side) => {
                let origin_width = match side {
                    Side::Left => self.shell.left_width.get(),
                    Side::Right => self.shell.right_width.get(),
                };
                self.blade_drag = Some(BladeDrag {
                    side,
                    origin_width,
                    origin_x: None,
                });
                Task::none()
            }
            Message::BladeDragMoved(position) => {
                let Some(drag) = self.blade_drag.as_mut() else {
                    return Task::none();
                };
                let Some(origin_x) = drag.origin_x else {
                    // `on_press` carries no cursor position at all, so the
                    // drag's origin is established by the FIRST move after
                    // the press instead. The resulting one-frame lag before
                    // the divider starts tracking is imperceptible.
                    drag.origin_x = Some(position.x);
                    return Task::none();
                };
                let delta = position.x - origin_x;
                let width = match drag.side {
                    // The left blade grows as the cursor moves right.
                    Side::Left => shell::BladeWidth::new(drag.origin_width + delta),
                    // The right blade's divider sits on its OWN left edge,
                    // so it grows as the cursor moves left, toward center.
                    Side::Right => shell::BladeWidth::new(drag.origin_width - delta),
                };
                match drag.side {
                    Side::Left => self.shell.left_width = width,
                    Side::Right => self.shell.right_width = width,
                }
                Task::none()
            }
            Message::BladeDragEnd => {
                // Deliberately NOT persisted on every `BladeDragMoved` above,
                // unlike every other shell mutation in this file: a drag
                // emits a message per mouse move, and writing `ui.toml` on
                // each one would hammer the disk for a single gesture.
                // Persistence happens once, here, when the gesture ends.
                self.blade_drag = None;
                self.persist_shell();
                Task::none()
            }
            Message::BladeReset(side) => {
                match side {
                    Side::Left => {
                        self.shell.left_width =
                            shell::BladeWidth::new(shell::BladeWidth::LEFT_DEFAULT);
                    }
                    Side::Right => {
                        self.shell.right_width =
                            shell::BladeWidth::new(shell::BladeWidth::RIGHT_DEFAULT);
                    }
                }
                self.persist_shell();
                Task::none()
            }
            Message::RightViewPicked(view) => {
                self.shell.right_view = view;
                self.persist_shell();
                Task::none()
            }
            Message::BottomViewToggled(view) => {
                self.shell.toggle_bottom(view);
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
                let task = match result {
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
                };
                self.clear_stale_session_pending();
                task
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
                    self.focus_pane(sibling);
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
                self.focus_pane(pane);
                Task::none()
            }
            Message::Watch(pane, session) => {
                self.watch(pane, session);
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
                let all_channels: Vec<String> = self.channel_names.clone();
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
        // A global cursor subscription, live ONLY during an in-flight blade
        // drag — gating on `blade_drag.is_some()` keeps `update` from
        // running on every mouse move for the rest of the app's life.
        // `listen_with` takes a fn pointer, not a closure, so all the drag
        // arithmetic lives in `update` instead of here.
        let blade_drag_sub = self.blade_drag.is_some().then(|| {
            iced::event::listen_with(|event, _status, _window| match event {
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::BladeDragMoved(position))
                }
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
                    iced::mouse::Button::Left,
                )) => Some(Message::BladeDragEnd),
                _ => None,
            })
        });
        iced::Subscription::batch(
            streams
                .into_iter()
                .chain([tick, keys])
                .chain(countdown_tick)
                .chain(blade_drag_sub),
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
                .spacing(SP_LOOSE)
                .padding(SP_LOOSE)
                .into();
        }

        // The channel workspace. `pane_grid::State` has always been the store;
        // this is the widget finally rendering it, which is what buys
        // arbitrary 2D nesting — split any pane on either axis, at any depth.
        let grid = pane_grid::PaneGrid::new(&self.panes, |id, pane, _maximized| {
            pane_grid::Content::new(channel_pane(self, id, pane)).title_bar(
                pane_grid::TitleBar::new(
                    text(pane.channel.as_str())
                        .size(TEXT_TITLE)
                        .font(semibold()),
                )
                .controls(Element::from(
                    row![
                        icon_button(
                            ICON_ROTATE_CW,
                            "refreshing this channel",
                            tooltip::Position::Bottom,
                            Message::Refresh(id),
                        ),
                        // `State::close` removes nothing and returns `None`
                        // when `pane` has no sibling (the single-pane case,
                        // which is also the app's startup state) — disable
                        // rather than publish a click that does nothing.
                        with_tip(
                            icon_button_raw(
                                ICON_X,
                                (self.panes.len() > 1).then_some(Message::Close(id)),
                            )
                            .style(|_theme, status| ghost_style(status)),
                            "closing this pane",
                            tooltip::Position::Bottom,
                        ),
                    ]
                    .spacing(SP),
                ))
                .always_show_controls()
                .padding(SP),
            )
        })
        .on_resize(10, Message::PaneResized)
        .on_drag(Message::PaneDragged)
        .on_click(Message::PaneClicked)
        .width(Fill)
        .height(Fill)
        .spacing(SP);

        let top_bar = container(admin_toolbar(self.admin)).padding(Padding {
            top: SP_LOOSE,
            right: SP_LOOSE,
            bottom: SP,
            left: SP_LOOSE,
        });
        let center: Element<Message> = container(column![grid].spacing(SP_LOOSE).padding(SP_LOOSE))
            .id(iced::widget::Id::new("center-grid-column"))
            .into();

        // The three-pane shell: collapsible blades either side of the channel
        // workspace. A collapsed blade shrinks to a thin `EDGE_TAB_W` tab on
        // the window's outer edge (`blade_edge_tab`) instead of a full-width
        // stub — `EDGE_TAB_W`'s doc comment covers why that no longer risks
        // hiding anything.
        let left: Element<Message> = if self.shell.left_collapsed {
            blade_edge_tab(
                ICON_CHEVRON_RIGHT,
                "open channels · ctrl+b",
                tooltip::Position::Right,
                Message::ToggleLeftBlade,
            )
            .id(iced::widget::Id::new("left-edge-tab"))
            .into()
        } else {
            container(left_blade(self))
                .id(iced::widget::Id::new("left-blade"))
                .width(Length::Fixed(self.shell.left_width.get()))
                .height(Fill)
                .into()
        };
        let right: Element<Message> = if self.shell.right_collapsed {
            blade_edge_tab(
                ICON_CHEVRON_LEFT,
                "open artifacts & lineage · ctrl+r",
                tooltip::Position::Left,
                Message::ToggleRightBlade,
            )
            .id(iced::widget::Id::new("right-edge-tab"))
            .into()
        } else {
            container(right_blade(self))
                .id(iced::widget::Id::new("right-blade"))
                .width(Length::Fixed(self.shell.right_width.get()))
                .height(Fill)
                .into()
        };

        let left_gap: Element<Message> = if self.shell.left_collapsed {
            Space::new()
                .width(Length::Fixed(DIVIDER_W))
                .height(Fill)
                .into()
        } else {
            blade_divider(Side::Left)
        };
        let right_gap: Element<Message> = if self.shell.right_collapsed {
            Space::new()
                .width(Length::Fixed(DIVIDER_W))
                .height(Fill)
                .into()
        } else {
            blade_divider(Side::Right)
        };
        // Always five children: the divider gap is reserved even collapsed,
        // so collapsing changes only the blade's own width, never the row's
        // total (`DIVIDER_W`'s doc comment).
        let shell_row = row![
            left,
            left_gap,
            container(center)
                .id(iced::widget::Id::new("center-pane-grid"))
                .width(Fill),
            right_gap,
            right,
        ]
        .spacing(0);

        column![top_bar, separator(), shell_row, footer(self)].into()
    }
}

/// A hairline across the full window width, marking where one band of chrome
/// ends and the next begins — currently the boundary between the top tab bar
/// and the workspace beneath it.
///
/// A 1px styled container rather than `iced::widget::rule`: every other
/// divider and border in this file is expressed as a container style, and one
/// vocabulary is worth more here than reaching for a second widget.
fn separator<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Fill)
        .height(Length::Fixed(1.0))
        .style(|_theme| container::Style {
            background: Some(Background::Color(BORDER)),
            ..container::Style::default()
        })
        .into()
}

/// The always-visible top tab bar: channels · settings · agents.
fn admin_toolbar(current: Option<AdminView>) -> Element<'static, Message> {
    let tab = |label: &'static str, target: Option<AdminView>| {
        let active = current == target;
        button(text(label).size(TEXT_BODY))
            .on_press(Message::OpenAdmin(target))
            .padding([SP_TIGHT, SP_LOOSE])
            .style(move |_t, _s| tab_style(active))
    };
    row![
        text("junto").size(TEXT_TITLE).font(semibold()),
        Space::new().width(SP_SECTION),
        tab("channels", None),
        tab("settings", Some(AdminView::Settings)),
        tab("agents", Some(AdminView::Agents)),
        Space::new().width(Fill),
        button(
            row![icon(ICON_ROTATE_CW), text("refresh").size(TEXT_BODY)]
                .spacing(SP_TIGHT)
                .align_y(Center),
        )
        .on_press(Message::RefreshAll)
        .padding([SP_TIGHT, SP_LOOSE])
        .style(|_t, _s| chip_style(MUTED, false)),
    ]
    .spacing(SP_TIGHT)
    .align_y(Center)
    .into()
}

/// The bottom status strip: the last child of the root column, carrying
/// metadata (focused channel, pane count, attention count, host) so the
/// panels above don't have to carry it — the user's own suggestion for
/// reducing clutter, and the thin-bottom-footer counterpart to
/// `admin_toolbar`'s top strip.
fn footer(app: &App) -> Element<'_, Message> {
    let pane = app.focus.and_then(|id| app.panes.get(id));
    let channel = pane
        .map(|p| p.channel.as_str())
        .unwrap_or("no channel focused");
    let panes = app.panes.len();
    let pane_word = if panes == 1 { "pane" } else { "panes" };
    let host = pane.map_or("local", |p| {
        let base = p.base();
        if base == HOST { "local" } else { base }
    });

    let attention = app.focus_items.len();
    let attention_open = app.shell.bottom == Some(shell::BottomView::Attention);
    let attention_chip: Element<Message> = if attention > 0 {
        let word = if attention == 1 { "needs" } else { "need" };
        button(
            row![
                icon(ICON_BELL),
                text(attention.to_string()).size(TEXT_META).font(semibold()),
                text(format!("{word} you")).size(TEXT_META),
            ]
            .spacing(SP_TIGHT)
            .align_y(Center),
        )
        .on_press(Message::BottomViewToggled(shell::BottomView::Attention))
        .padding([SP_TIGHT, SP])
        .style(move |_t, _s| chip_style(YELLOW, attention_open))
        .into()
    } else {
        row![
            icon(ICON_BELL).color(GREEN),
            text("all clear").size(TEXT_META).color(GREEN),
        ]
        .spacing(SP_TIGHT)
        .align_y(Center)
        .into()
    };
    // Floats over the workspace instead of displacing it — the "float
    // over" metaphor Orca's own status-strip popovers use — anchored to
    // THIS chip so it rises from the chip's own x-position, and dismisses
    // on an outside click (`Popover::on_dismiss`) or a second press of the
    // chip. 360px: wider than the 280px left-blade default
    // (`shell::BladeWidth::LEFT_DEFAULT`) since a focus chip's label
    // carries a tag, channel, author, and up to a 40-char summary;
    // narrower than the annotate popup's 560 default, which sizes for
    // typed comment text rather than a short chip list.
    let attention_trigger: Element<Message> = Popover::new(
        attention_chip,
        attention_open.then(|| bottom_panel(app, shell::BottomView::Attention)),
    )
    .width(360.0)
    .on_dismiss(Message::BottomViewToggled(shell::BottomView::Attention))
    .into();

    let sessions = pane.map_or(0, |p| p.session_list().len());
    let sessions_open = app.shell.bottom == Some(shell::BottomView::Sessions);
    let sessions_chip: Element<Message> = if sessions > 0 {
        button(
            row![
                icon(ICON_BOT),
                text(format!("{sessions} sessions")).size(TEXT_META)
            ]
            .spacing(SP_TIGHT)
            .align_y(Center),
        )
        .on_press(Message::BottomViewToggled(shell::BottomView::Sessions))
        .padding([SP_TIGHT, SP])
        .style(move |_t, _s| chip_style(MUTED, sessions_open))
        .into()
    } else {
        row![
            icon(ICON_BOT).color(MUTED),
            text("no sessions").size(TEXT_META).color(MUTED),
        ]
        .spacing(SP_TIGHT)
        .align_y(Center)
        .into()
    };
    let sessions_trigger: Element<Message> = Popover::new(
        sessions_chip,
        sessions_open.then(|| bottom_panel(app, shell::BottomView::Sessions)),
    )
    .width(360.0)
    .on_dismiss(Message::BottomViewToggled(shell::BottomView::Sessions))
    .into();

    let dot = || text("·").size(TEXT_META).color(MUTED);
    let strip = row![
        text(channel).size(TEXT_META).color(MUTED),
        dot(),
        text(format!("{panes} {pane_word}"))
            .size(TEXT_META)
            .color(MUTED),
        dot(),
        attention_trigger,
        dot(),
        sessions_trigger,
        dot(),
        text(host).size(TEXT_META).color(MUTED),
    ]
    .spacing(SP)
    .align_y(Center);

    container(strip)
        .width(Fill)
        .padding(Padding {
            top: SP_TIGHT,
            right: SP_LOOSE,
            bottom: SP_TIGHT,
            left: SP_LOOSE,
        })
        .style(|_theme| container::Style {
            background: Some(Background::Color(Color { a: 0.4, ..SURFACE })),
            border: Border {
                color: BORDER,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// The floating attention/sessions panel: opened by pressing one of the
/// footer's trigger chips, which anchors it via `Popover` so it rises from
/// THAT chip's x-position and floats over the workspace instead of
/// displacing it.
///
/// No height cap here on purpose. `Popover`'s overlay caps the panel to the
/// room actually available above the chip, which is nearly the whole window,
/// so a long list grows until it genuinely cannot fit before any scrollbar
/// appears. This container must therefore stay content-sized, or it would
/// fight that cap and reintroduce the fixed-height box.
fn bottom_panel(app: &App, view: shell::BottomView) -> Element<'_, Message> {
    let (title, body) = match view {
        shell::BottomView::Attention => ("attention", attention_view(app)),
        shell::BottomView::Sessions => ("sessions", sessions_view(app)),
    };

    // No close button: the chip that opened this panel toggles it shut,
    // and `Popover::on_dismiss` closes it on an outside click too.
    let header = text(title).size(TEXT_BODY).font(semibold());

    container(column![header, body].spacing(SP))
        .padding(SP)
        .style(|_theme| container::Style {
            background: Some(Background::Color(SURFACE)),
            border: Border {
                color: BORDER,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// Which side of the shell a blade sits on — used to route a divider drag
/// or reset to the right blade (`blade_divider`, `BladeDrag`,
/// `Message::BladeReset`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

/// An in-flight blade-width drag, started by pressing a divider handle.
/// Populated in two steps because `mouse_area::on_press` carries no cursor
/// position: `BladeDragStart` records the side and the width to resize
/// from, then the FIRST `BladeDragMoved` after it fills in `origin_x`.
struct BladeDrag {
    /// Which blade is being resized.
    side: Side,
    /// That blade's width when the drag began — the running delta is
    /// applied to this, not to the blade's live (already-updated) width.
    origin_width: f32,
    /// The cursor's x position when the drag began, in absolute window
    /// coordinates. `None` until the first `BladeDragMoved` sets it.
    origin_x: Option<f32>,
}

/// The collapsed form of a blade: a thin click target flush to the
/// window's outer edge, carrying only the chevron that reopens the blade
/// — centred vertically, ghost-styled, `EDGE_TAB_W` wide rather than a
/// full-width rail. It carries no attention badge, unlike the stub this
/// replaced; see `EDGE_TAB_W`'s doc comment for why that is now safe.
///
/// `EDGE_TAB_W` is narrower than `ICON_BTN`, so the reopen chevron can't
/// be a plain `icon_button` (a fixed `ICON_BTN` square) — this builds it
/// directly from `icon_button_raw` (overriding its geometry to
/// `EDGE_TAB_W`) plus `with_tip`, the same pair `icon_button` itself
/// composes. Returns the bare `Container` rather than an `Element` so a
/// caller can tag it with an `.id(...)` (`App::view`), matching
/// `blade_divider`'s and `icon_button_raw`'s own precedent of leaving the
/// final `.into()` to the caller.
fn blade_edge_tab<'a>(
    codepoint: char,
    tip: &'a str,
    position: tooltip::Position,
    message: Message,
) -> iced::widget::Container<'a, Message> {
    let chevron = icon_button_raw(codepoint, Some(message))
        .width(Length::Fixed(EDGE_TAB_W))
        .height(Length::Fixed(EDGE_TAB_W))
        .style(|_theme, status| ghost_style(status));
    container(with_tip(chevron, tip, position))
        .width(Length::Fixed(EDGE_TAB_W))
        .center_y(Fill)
}

/// A thin draggable handle between a blade and the center: press-drag to
/// resize that blade, double-click to reset it to its default width.
/// Rendered only beside an EXPANDED blade (`App::view`); a collapsed blade
/// gets a plain `Space` of the same width instead (`App::view`), so the row
/// always has five children and collapsing changes only the blade's width.
fn blade_divider<'a>(side: Side) -> Element<'a, Message> {
    let handle = container(Space::new())
        .width(Length::Fixed(DIVIDER_W))
        .height(Fill)
        .style(|_theme| container::Style {
            background: Some(Background::Color(BORDER)),
            ..container::Style::default()
        });
    mouse_area(handle)
        .on_press(Message::BladeDragStart(side))
        .on_double_click(Message::BladeReset(side))
        .interaction(mouse::Interaction::ResizingHorizontally)
        .into()
}

/// The blade's recessed field style, shared by the channel filter and the
/// new-channel name field: a muted fill and hairline border, matching the
/// file's other recessed surfaces (`admin_card`), instead of the default
/// theme's raised, high-contrast text-input chrome.
fn field_style(_theme: &Theme, _status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: Background::Color(Color { a: 0.4, ..SURFACE }),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 6.0.into(),
        },
        icon: MUTED,
        placeholder: MUTED,
        value: TEXT,
        selection: Color { a: 0.35, ..BLUE },
    }
}

/// The substrate picker's recessed style, matching `field_style` — `pick_list`
/// has its own `Catalog` rather than forwarding to `text_input`'s, so it needs
/// its own (smaller) style function to read as the same quiet field.
fn field_pick_list_style(_theme: &Theme, _status: pick_list::Status) -> pick_list::Style {
    pick_list::Style {
        text_color: TEXT,
        placeholder_color: MUTED,
        handle_color: MUTED,
        background: Background::Color(Color { a: 0.4, ..SURFACE }),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 6.0.into(),
        },
    }
}

/// The create button's recessed style: the same muted fill and hairline as
/// `field_style`'s fields (a touch brighter on hover/press, matching
/// `ghost_style`'s tint step), so it reads as part of the quiet field row
/// rather than the default theme's raised, accented button.
fn field_button_style(status: button::Status) -> button::Style {
    let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(Color {
            a: if hot { 0.6 } else { 0.4 },
            ..SURFACE
        })),
        text_color: if status == button::Status::Disabled {
            MUTED
        } else {
            TEXT
        },
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..button::Style::default()
    }
}

/// A glyph set inside a recessed field's left edge (`field_style`), in place
/// of a separate icon glyph beside the field — folding the search/create
/// affordance into the quiet control itself instead of an extra loud
/// element next to it.
fn field_icon(code_point: char) -> text_input::Icon<iced::Font> {
    text_input::Icon {
        font: ICON_FONT,
        code_point,
        size: Some(TEXT_META.into()),
        spacing: SP_TIGHT,
        side: text_input::Side::Left,
    }
}

/// The create-channel controls: a name field, substrate picker, and error
/// display for a new one. Shown beneath the left blade's header only
/// while `App::creating` is true (`channel_nav`, toggled by the header's
/// `plus`). Every control is styled recessed (`field_style`/
/// `field_pick_list_style`/`field_button_style`) so this reads as a row of
/// quiet fields, not the loudest thing in the blade.
fn adder(app: &App) -> Element<'_, Message> {
    let new_row = row![
        text_input("new channel name…", &app.new_channel)
            .icon(field_icon(ICON_PLUS))
            .on_input(Message::NewChannelChanged)
            .on_submit(Message::CreateChannel)
            .size(TEXT_META)
            .style(field_style)
            .width(Fill)
            .padding(SP),
        button(text("create").size(TEXT_META))
            .on_press(Message::CreateChannel)
            .padding(SP)
            .style(|_theme, status| field_button_style(status)),
    ]
    .spacing(SP)
    .align_y(Center);
    let mut adder_col = column![new_row].spacing(SP);
    // When several substrates are registered, the host needs to know which.
    if app.substrates.len() > 1 {
        adder_col = adder_col.push(
            pick_list(
                app.substrates.clone(),
                app.new_channel_repo.clone(),
                Message::NewChannelRepoChanged,
            )
            .text_size(TEXT_META)
            .padding(SP)
            .style(field_pick_list_style),
        );
    }
    match &app.new_channel_error {
        Some(err) => adder_col
            .push(
                row![
                    icon(ICON_CIRCLE_ALERT).color(RED),
                    text(err).size(TEXT_META).color(RED)
                ]
                .spacing(SP_TIGHT)
                .align_y(Center),
            )
            .into(),
        None => adder_col.into(),
    }
}

/// Where `placement_choices` lays out its three controls: the channel
/// chips' floating menu wants a full-width column (one choice per row,
/// filling the menu's fixed width); the attention panel's inline row wants
/// them side by side and only as wide as their labels, since there is no
/// floating menu to size around them.
#[derive(Debug, Clone, Copy)]
enum PlacementLayout {
    Menu,
    Inline,
}

/// The three ways to dock a pending channel or session — split right,
/// split below, or reuse the focused pane — in the file's ghost-row
/// vocabulary (`ghost_style`) rather than a filled control, since this is
/// a transient pick, not a persistent toggle. Shared by the channel
/// chips' floating menu (`placement_menu`) and the attention/sessions
/// panels' shared inline placement row (`inline_placement_row`), so the
/// three controls and their labels exist in exactly one place rather than
/// duplicated per call site.
fn placement_choices(pending: &PendingOpen, layout: PlacementLayout) -> Element<'static, Message> {
    let width = match layout {
        PlacementLayout::Menu => Fill,
        PlacementLayout::Inline => Length::Shrink,
    };
    let row_button = |code_point: char, label: &'static str, placement: Placement| {
        button(
            row![icon(code_point).color(MUTED), text(label).size(TEXT_META)]
                .spacing(SP_TIGHT)
                .align_y(Center),
        )
        .on_press(Message::ChannelPlaced(pending.clone(), placement))
        .padding([SP_TIGHT, SP])
        .width(width)
        .style(|_theme, status| ghost_style(status))
        .into()
    };
    let choices = [
        row_button(ICON_COLUMNS_2, "split right", Placement::Right),
        row_button(ICON_ROWS_2, "split below", Placement::Below),
        row_button(ICON_PANEL_LEFT, "use this pane", Placement::Here),
    ];
    match layout {
        PlacementLayout::Menu => column(choices).spacing(SP_TIGHT).into(),
        PlacementLayout::Inline => row(choices).spacing(SP_TIGHT).into(),
    }
}

/// The floating menu an unopened channel's chip opens (`channel_nav`,
/// `Message::ChannelToggled`): `placement_choices` in a bordered, elevated
/// panel (`SURFACE`/`BORDER`) since it floats over the workspace rather
/// than sitting inline in a list.
fn placement_menu(pending: &PendingOpen) -> Element<'static, Message> {
    container(placement_choices(pending, PlacementLayout::Menu))
        .padding(SP_TIGHT)
        .style(|_theme| container::Style {
            background: Some(Background::Color(SURFACE)),
            border: Border {
                color: BORDER,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// Whether `name` belongs in the filtered channel list for `filter` — a
/// case-insensitive substring match. An empty filter matches everything:
/// finding a channel is the filter's job, not hiding the list until you
/// type (`channel_nav`).
fn channel_matches(name: &str, filter: &str) -> bool {
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
}

/// Pinned navigation: a header (the section label plus a `plus` toggling
/// the create-channel form), an always-visible filter over the channel
/// list, and the filtered chips themselves. Lives at the top of the left
/// blade and never toggles away.
///
/// The filter replaces what used to be a second, redundant control — a
/// type-ahead combo box duplicating the exact list already rendered as
/// chips right above it — with one fewer widget doing the same job.
/// Create used to sit inline and always visible; it now lives behind the
/// header's `plus` (`App::creating`), rendered directly beneath the
/// header when open, so the common case — finding and opening an
/// existing channel — isn't sharing space with the rare one.
///
/// An open channel's chip closes its pane on press (`Message::ChannelToggled`),
/// guarded exactly like the pane title bar's own `×` (`panes.len() > 1`) so a
/// chip that cannot close renders disabled instead of publishing a press that
/// does nothing. An unopened channel's chip instead opens a floating
/// placement menu (`placement_menu`), anchored via `Popover` — the same
/// "float over the workspace" machinery the footer's trigger chips use —
/// dismissible by an outside click or a second press of the chip.
fn channel_nav(app: &App) -> Element<'_, Message> {
    let header = row![
        text("channels")
            .size(TEXT_META)
            .color(MUTED)
            .font(semibold()),
        Space::new().width(Fill),
        icon_button(
            ICON_PLUS,
            if app.creating {
                "cancel creating a channel"
            } else {
                "create a channel"
            },
            tooltip::Position::Bottom,
            Message::ToggleCreating,
        ),
    ]
    .align_y(Center);

    let filter_field = text_input("filter channels…", &app.channel_filter)
        .icon(field_icon(ICON_SEARCH))
        .on_input(Message::ChannelFilterChanged)
        .size(TEXT_META)
        .style(field_style)
        .width(Fill)
        .padding(SP);

    let filter = app.channel_filter.trim();
    let filtered: Vec<&String> = app
        .channel_names
        .iter()
        .filter(|name| channel_matches(name, filter))
        .collect();
    let mut list = column![].spacing(SP_TIGHT);
    if filtered.is_empty() && !filter.is_empty() {
        list = list.push(text("no channels match").size(TEXT_META).color(MUTED));
    } else {
        for name in filtered {
            let active = app.panes.iter().any(|(_, state)| state.channel == *name);
            let can_press = !active || app.panes.len() > 1;
            let chip: Element<Message> = button(text(name.as_str()).size(TEXT_BODY))
                .on_press_maybe(can_press.then(|| Message::ChannelToggled(name.clone())))
                .padding([SP_TIGHT, SP])
                .width(Fill)
                .style(move |_t, _s| chip_style(MUTED, active))
                .into();
            let row: Element<Message> = if active {
                chip
            } else {
                let pending = app.pending.as_ref().filter(|pending| {
                    matches!(&pending.target, PendingTarget::Channel) && pending.channel == *name
                });
                // 170px: enough for an icon plus the longest row label ("use
                // this pane") at `TEXT_META` with room to breathe — this menu
                // has no list to grow, just three fixed rows, so it needs
                // nothing near the footer's 360px list panels.
                Popover::new(chip, pending.map(placement_menu))
                    .width(170.0)
                    .on_dismiss(Message::ChannelToggled(name.clone()))
                    .into()
            };
            list = list.push(row);
        }
    }

    let mut nav = column![header];
    if app.creating {
        nav = nav.push(adder(app));
    }
    nav.push(filter_field)
        .push(scrollable(list).height(Fill))
        .spacing(SP)
        .into()
}

/// One focus-board chip: a tagged, coloured summary of a cross-channel
/// "needs you" item. If its channel is already open, jumps straight to the
/// entry (`Message::FocusChipPicked`) — an attention item always means
/// "take me there", so unlike a channel chip this never toggles a pane
/// closed. If it isn't open yet, toggles an inline placement row beneath
/// it instead (`Message::FocusChipToggled`, `attention_placement_row`).
fn focus_chip<'a>(app: &App, item: &'a FocusItem) -> Element<'a, Message> {
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
    let mut chip = button(text(label).size(TEXT_META))
        .padding([SP_TIGHT, SP])
        .style(move |_t, _s| chip_style(color, false));
    if let Some(name) = &item.channel_name {
        let open = app.panes.iter().any(|(_, state)| state.channel == *name);
        chip = chip.on_press(if open {
            Message::FocusChipPicked(name.clone(), item.entry_id.clone())
        } else {
            Message::FocusChipToggled(name.clone(), item.entry_id.clone())
        });
    }
    chip.into()
}

/// The inline placement row beneath a chip whose target isn't placed yet
/// — an attention chip's channel or a session chip's session:
/// `placement_choices` laid out horizontally (`PlacementLayout::Inline`)
/// rather than inside another `Popover` — the attention and sessions
/// panels are both themselves `Popover` popups, and `Popover`'s
/// `Floating` overlay never implements `overlay::Overlay::overlay`, so a
/// nested one would silently never render. Indented under its chip so the
/// association is obvious; pressing the chip again (`Message::
/// FocusChipToggled` / `Message::SessionToggled`) backs out without
/// choosing.
fn inline_placement_row(pending: &PendingOpen) -> Element<'static, Message> {
    row![
        Space::new().width(SP_LOOSE),
        placement_choices(pending, PlacementLayout::Inline),
    ]
    .into()
}

/// The cross-channel "needs you" items — the focus board, relocated out of
/// the permanent top banner into the left blade where it can be put away.
fn attention_view(app: &App) -> Element<'_, Message> {
    // Body moved verbatim from the former top-banner block; it becomes a
    // vertical list rather than a horizontal chip strip, since the blade is
    // tall and narrow rather than short and wide.
    if app.focus_items.is_empty() {
        return text("focus · all clear")
            .size(TEXT_BODY)
            .color(GREEN)
            .into();
    }
    let mut items = column![
        text(format!("needs you ({})", app.focus_items.len()))
            .size(TEXT_BODY)
            .color(YELLOW)
    ]
    .spacing(SP_TIGHT);
    for item in &app.focus_items {
        items = items.push(focus_chip(app, item));
        // Only one inline row at a time, for whichever chip is pending —
        // matched by entry id so two items sharing a channel don't both
        // grow a row.
        if let Some(pending) = &app.pending
            && matches!(&pending.target, PendingTarget::Entry(id) if id == &item.entry_id)
        {
            items = items.push(inline_placement_row(pending));
        }
    }
    scrollable(items).into()
}

/// The left blade: pinned channel navigation, with its own collapse toggle
/// pinned to the blade's own bottom-left corner — a footer row, the last
/// child of the blade's column, rather than the top bar corner an earlier
/// pass moved it to. A toggle at the blade's own outer edge reads as part
/// of THAT blade and can't be mistaken for a control over the other one or
/// the center; xum places its sidebar's collapse chevron the same way. The
/// channel nav is given `.height(Fill)` so it — not the footer — absorbs
/// any extra space, keeping the toggle flush to the bottom regardless of
/// how short the channel list is.
fn left_blade(app: &App) -> Element<'_, Message> {
    let content = container(channel_nav(app)).height(Fill);
    let toggle = container(icon_button(
        ICON_CHEVRON_LEFT,
        "close channels · ctrl+b",
        tooltip::Position::Top,
        Message::ToggleLeftBlade,
    ))
    .id(iced::widget::Id::new("left-blade-toggle"));
    let footer = row![toggle, Space::new().width(Fill)];
    container(column![content, footer].spacing(SP))
        .padding(SP)
        .width(Fill)
        .height(Fill)
        .into()
}

/// The right blade: a switchable Artifacts/Lineage view, with its own
/// collapse toggle in a footer row pinned to the blade's bottom-right
/// corner — mirrors `left_blade`'s bottom-left one.
fn right_blade(app: &App) -> Element<'_, Message> {
    let switcher = row![
        button(
            container(
                row![icon(ICON_FILE_DIFF), text("artifacts").size(TEXT_BODY)]
                    .spacing(SP_TIGHT)
                    .align_y(Center),
            )
            .center_x(Fill),
        )
        .on_press(Message::RightViewPicked(shell::RightView::Artifacts))
        .width(Length::FillPortion(1))
        .padding(SP_TIGHT)
        .style(move |_t, _s| tab_style(app.shell.right_view == shell::RightView::Artifacts)),
        button(
            container(
                row![icon(ICON_GIT_BRANCH), text("lineage").size(TEXT_BODY)]
                    .spacing(SP_TIGHT)
                    .align_y(Center),
            )
            .center_x(Fill),
        )
        .on_press(Message::RightViewPicked(shell::RightView::Lineage))
        .width(Length::FillPortion(1))
        .padding(SP_TIGHT)
        .style(move |_t, _s| tab_style(app.shell.right_view == shell::RightView::Lineage)),
    ]
    .spacing(SP_TIGHT);

    let body: Element<Message> = match app.shell.right_view {
        shell::RightView::Artifacts => artifacts_view(app),
        shell::RightView::Lineage => lineage_view(app),
    };

    let content = container(column![switcher, body].spacing(SP)).height(Fill);
    let toggle = container(icon_button(
        ICON_CHEVRON_RIGHT,
        "close artifacts & lineage · ctrl+r",
        tooltip::Position::Top,
        Message::ToggleRightBlade,
    ))
    .id(iced::widget::Id::new("right-blade-toggle"));
    let footer = row![Space::new().width(Fill), toggle];
    container(column![content, footer].spacing(SP))
        .padding(SP)
        .width(Fill)
        .height(Fill)
        .into()
}

/// The whole lineage DAG as a vertical list, one row per channel,
/// ordered hierarchically (`lineage_hierarchy`'s own doc comment) so a
/// branch and everything that reconnects into it sit together — modelled
/// on Orca's "Commit Tree" rather than the old horizontal time-axis
/// `LineageCanvas` it replaced. That graph scaled its track width with
/// the time span, needing real horizontal room; a narrow blade (the
/// right blade's actual, user-configured width) left it almost nothing
/// to draw in and the whole graph went blank. This rail instead scales
/// with graph DEPTH (`MAX_LANE` lanes, `LANE_W` wide each —
/// `lineage_rail_cell`'s own doc comment covers the pixel budget), which
/// the live data never makes large, so a small fixed-width rail plus a
/// `truncate()`d name survives any blade width, including the narrowest
/// one this shell allows (`lineage_view_tests`).
///
/// Each row's rail cell (`lineage_rail_cell`) is a small `Canvas`
/// drawing that row's `LineageRailRow`: a through-line for every lane a
/// real ancestor still owns below this row, a branch peeling in from the
/// owner's lane, a reconnect curving out to a converge target's lane,
/// and the node's own marker — the "peels off and reconnects" feel a
/// flat list of glyphs never gave.
fn lineage_view(app: &App) -> Element<'_, Message> {
    let Some(graph) = &app.lineage else {
        return text("no lineage yet").size(TEXT_BODY).color(MUTED).into();
    };

    let header = lineage_header(app, graph.nodes.len());
    if app.lineage_collapsed {
        return column![header].into();
    }

    let open: HashSet<String> = app
        .panes
        .iter()
        .map(|(_, pane)| pane.channel.clone())
        .collect();
    let focused_channel = app
        .focus
        .and_then(|id| app.panes.get(id))
        .map(|pane| pane.channel.as_str());
    let by_id: HashMap<&str, &GNode> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let layout = lineage_layout(graph);

    let mut list = column![];
    for node in layout.order.iter().copied() {
        let relations = node_relations(graph, &node.id);
        let is_focused = focused_channel == Some(node.name.as_str());
        let is_open = open.contains(&node.name);
        let color = if is_focused {
            TEXT
        } else if is_open {
            TEAL
        } else {
            MUTED
        };
        let state = LineageRowState {
            rail: layout
                .rails
                .get(node.id.as_str())
                .copied()
                .unwrap_or_default(),
            color,
            is_focused,
            is_open,
            expanded: app.lineage_expanded.contains(&node.id),
        };
        list = list.push(lineage_row(
            node,
            &by_id,
            &relations,
            state,
            app.pending.as_ref(),
        ));
    }

    column![header, scrollable(list).height(Fill)]
        .spacing(SP)
        .height(Fill)
        .into()
}

/// The lineage section's header: a collapse chevron, the section label and
/// its node count (mirrors `channel_nav`'s own `text("channels")…
/// semibold()` header), and a refresh trigger that refetches just the
/// lineage DAG.
fn lineage_header(app: &App, count: usize) -> Element<'_, Message> {
    row![
        icon_button(
            if app.lineage_collapsed {
                ICON_CHEVRON_RIGHT
            } else {
                ICON_CHEVRON_DOWN
            },
            if app.lineage_collapsed {
                "expand lineage"
            } else {
                "collapse lineage"
            },
            tooltip::Position::Bottom,
            Message::ToggleLineageCollapsed,
        ),
        text("lineage")
            .size(TEXT_META)
            .color(MUTED)
            .font(semibold()),
        text(count.to_string()).size(TEXT_META).color(MUTED),
        Space::new().width(Fill),
        icon_button(
            ICON_REFRESH_CW,
            "refresh lineage",
            tooltip::Position::Bottom,
            Message::RefreshLineage,
        ),
    ]
    .spacing(SP_TIGHT)
    .align_y(Center)
    .into()
}

/// A lineage row's presentation state, computed once per node in
/// `lineage_view`'s loop from its rail geometry, focus, and open-ness —
/// bundled into one argument (rather than five) since all five travel
/// together from that loop into `lineage_row`. `Copy`: every field is a
/// plain value (`LineageRailRow` is itself `Copy` — its own doc comment
/// covers why), cheaper to copy than to borrow.
#[derive(Debug, Clone, Copy)]
struct LineageRowState {
    rail: LineageRailRow,
    color: Color,
    is_focused: bool,
    is_open: bool,
    expanded: bool,
}

/// One lineage row: the rail cell, a per-row disclosure chevron, and the
/// channel name, plus — while expanded — that node's relations and
/// milestones underneath. Clicking the name behaves like a channel chip
/// (`channel_nav`): an unopened channel offers the same
/// `placement_choices` menu (`placement_menu`, reused verbatim, not
/// copied); an already-open one is focused rather than closed
/// (`Message::LineageChannelPicked`'s own doc comment covers why).
fn lineage_row<'a>(
    node: &'a GNode,
    by_id: &HashMap<&'a str, &'a GNode>,
    relations: &LineageRelations<'a>,
    state: LineageRowState,
    pending: Option<&PendingOpen>,
) -> Element<'a, Message> {
    let LineageRowState {
        rail,
        color,
        is_focused,
        is_open,
        expanded,
    } = state;
    let disclosure = icon_button(
        if expanded {
            ICON_CHEVRON_DOWN
        } else {
            ICON_CHEVRON_RIGHT
        },
        if expanded {
            "hide relations"
        } else {
            "show relations"
        },
        tooltip::Position::Bottom,
        Message::LineageRowToggled(node.id.clone()),
    );

    let name_button = button(
        text(truncate(&node.name, 28))
            .size(TEXT_BODY)
            .wrapping(text::Wrapping::None),
    )
    .on_press(Message::LineageChannelPicked(node.name.clone()))
    .padding([SP_TIGHT, SP])
    .width(Fill)
    .style(move |_theme, status| lineage_name_style(color, status));
    let name_cell: Element<Message> = if is_open {
        name_button.into()
    } else {
        let row_pending = pending.filter(|pending| {
            matches!(&pending.target, PendingTarget::Channel) && pending.channel == node.name
        });
        Popover::new(name_button, row_pending.map(placement_menu))
            .width(170.0)
            .on_dismiss(Message::LineageChannelPicked(node.name.clone()))
            .into()
    };

    // The marker fill reuses `color` — the same focused (TEXT) / open
    // (TEAL) / neither (MUTED) state already driving the name text below
    // — so an open pane or the focused row is findable by colour alone,
    // without reading every name in the list.
    let header_row = row![
        lineage_rail_cell(rail, color, is_focused),
        disclosure,
        name_cell
    ]
    .spacing(SP_TIGHT)
    .align_y(Center);

    let mut col = column![header_row].spacing(SP_TIGHT);
    if expanded {
        col = col.push(lineage_detail(by_id, node, relations));
    }
    col.into()
}

/// One lane's width in the rail (`lineage_rail_cell`): wide enough for a
/// through-line, a curved connector, and the node marker to stay legible
/// at this size; narrow enough that `MAX_LANE` lanes (`MAX_LANE`'s own
/// doc comment covers the depth cap) plus the disclosure chevron still
/// leave most of a 211px right blade — this shell's narrowest
/// (`a_lineage_row_stays_single_line_at_the_narrowest_reported_blade_width`)
/// — for the channel name.
const LANE_W: f32 = 10.0;

/// The rail cell for one lineage row: a small `Canvas` (`LineageRailCanvas`
/// below) drawing that row's `LineageRailRow`. The `canvas::Program`
/// wiring — `Frame`/`Path`/`Stroke`/`into_geometry` — is lifted from the
/// deleted horizontal `LineageCanvas` (commit `18a18af`); only the
/// geometry changed, from a time axis to vertical lanes.
///
/// Fixed `MAX_LANE * LANE_W` wide, but `Fill` height rather than a
/// hardcoded guess: every row's natural height is already the same
/// constant (the disclosure button is a fixed `ICON_BTN` square and the
/// name is always a single line at one text size — `lineage_view`'s own
/// doc comment on width applies the same reasoning to height), so `Fill`
/// just matches whatever that shared height resolves to, guaranteeing a
/// through-line touches this cell's true top and bottom edges and lines
/// up with the next row's — a hardcoded height even one pixel off would
/// leave a gap and break the rail's continuity.
fn lineage_rail_cell(
    rail: LineageRailRow,
    marker_color: Color,
    focused: bool,
) -> Element<'static, Message> {
    Canvas::new(LineageRailCanvas {
        rail,
        marker_color,
        focused,
    })
    .width(Length::Fixed(MAX_LANE as f32 * LANE_W))
    .height(Fill)
    .into()
}

#[derive(Debug, Clone, Copy)]
struct LineageRailCanvas {
    rail: LineageRailRow,
    marker_color: Color,
    focused: bool,
}

impl canvas::Program<Message> for LineageRailCanvas {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let height = bounds.height;
        let mid = height / 2.0;
        let lane_x = |lane: usize| LANE_W * (lane as f32 + 0.5);
        let connector_stroke = Stroke::default().with_color(MAUVE).with_width(1.5);

        // Every lane a real ancestor still owns below this row, drawn the
        // full cell height — the persistent "main timeline" a branch
        // peels off from and reconnects into.
        for lane in self.rail.through.lanes() {
            frame.stroke(
                &Path::line(
                    Point::new(lane_x(lane), 0.0),
                    Point::new(lane_x(lane), height),
                ),
                Stroke::default().with_color(BORDER).with_width(1.5),
            );
        }

        // Branch peel-in: this row forks off its owner's lane at the top
        // of the cell, arriving at this row's own lane by the centre.
        if let Some(parent_lane) = self.rail.branch_from {
            frame.stroke(
                &lineage_connector(
                    Point::new(lane_x(parent_lane), 0.0),
                    Point::new(lane_x(self.rail.lane), mid),
                ),
                connector_stroke,
            );
        }

        // Reconnect: this row's own outgoing converge curves from its
        // lane at the centre out to the target lane by the bottom edge.
        if let Some(target_lane) = self.rail.converge_to {
            frame.stroke(
                &lineage_connector(
                    Point::new(lane_x(self.rail.lane), mid),
                    Point::new(lane_x(target_lane), height),
                ),
                connector_stroke,
            );
        }

        if self.focused {
            frame.stroke(
                &Path::circle(
                    Point::new(lane_x(self.rail.lane), mid),
                    lineage_marker_radius(self.rail.role) + 2.5,
                ),
                Stroke::default().with_color(MAUVE).with_width(1.5),
            );
        }
        frame.fill(
            &Path::circle(
                Point::new(lane_x(self.rail.lane), mid),
                lineage_marker_radius(self.rail.role),
            ),
            self.marker_color,
        );

        vec![frame.into_geometry()]
    }
}

/// A branch/reconnect connector between two lane positions: one straight
/// segment, the `\` and `/` of `git log --graph`.
///
/// This was a cubic Bézier with vertical tangents at both ends, on the
/// reasoning that a flowing curve suited the branching-timeline reference.
/// Over a 10px lane and a ~25px row that reads as a wiggle rather than a
/// branch — the two tangents turn every connector into an S — and stacked
/// down a column of twenty channels it reads as noise. A git graph angles
/// only at the junction and runs straight everywhere else, which is what
/// makes a lane legible at this scale.
fn lineage_connector(from: Point, to: Point) -> Path {
    Path::line(from, to)
}

/// A structurally significant node (root or fork) gets a slightly larger
/// marker — the drawn geometry's own equivalent of the old glyph swap
/// (`ICON_CIRCLE`/`ICON_GIT_FORK` read visually bigger than a plain dot
/// too), now that the branch/converge connectors carry most of the role
/// signal instead of the marker's own shape.
fn lineage_marker_radius(role: LineageRole) -> f32 {
    match role {
        LineageRole::Root | LineageRole::Fork => 3.5,
        LineageRole::Converged | LineageRole::Ordinary => 2.5,
    }
}

/// Joins names the way a sentence would: "a", "a and b", or "a, b, and c"
/// — used for a fork's "diverged into" line, which otherwise reads as a
/// bare comma list for every real fork with more than one child.
fn join_and(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
    }
}

/// A lineage row's expanded detail: its relations in words — diverged
/// from/into, converged into — then its milestones, each `truncate()`d
/// (the live data's labels run past 70 characters). Indented under the
/// rail so it reads as this row's own detail, not a sibling row.
fn lineage_detail<'a>(
    by_id: &HashMap<&'a str, &'a GNode>,
    node: &'a GNode,
    relations: &LineageRelations<'a>,
) -> Element<'a, Message> {
    let name_of = |id: &str| {
        by_id
            .get(id)
            .map_or_else(|| id.to_string(), |n| n.name.clone())
    };

    let mut lines = Vec::new();
    if let Some(parent) = relations.parent {
        lines.push(format!("diverged from {}", name_of(parent)));
    }
    if !relations.children.is_empty() {
        let names: Vec<String> = relations.children.iter().map(|id| name_of(id)).collect();
        lines.push(format!("diverged into {}", join_and(&names)));
    }
    if let Some(target) = relations.converged_into {
        lines.push(format!("converged into {}", name_of(target)));
    }

    let mut detail = column![].spacing(SP_TIGHT);
    for line in lines {
        detail = detail.push(text(line).size(TEXT_META).color(MUTED));
    }
    let mut milestones: Vec<&MilestoneDto> = node.milestones.iter().collect();
    // The host's own array order isn't documented as chronological — sort
    // explicitly so a node's history always reads oldest-to-newest here,
    // regardless of how it arrived over the wire.
    milestones.sort_by_key(|m| m.ms);
    for milestone in milestones {
        detail = detail.push(
            text(truncate(&milestone.label, 80))
                .size(TEXT_META)
                .color(MUTED),
        );
    }

    container(detail)
        .padding(Padding {
            top: SP_TIGHT,
            right: SP_TIGHT,
            bottom: SP_TIGHT,
            left: ICON_BTN + SP_TIGHT,
        })
        .into()
}

/// The lineage row's channel-name button: `ghost_style`'s own ethos (a
/// list row is not a filled control) but keeping the row's own state
/// colour (focused/open/neither) as the text colour instead of
/// `ghost_style`'s fixed `MUTED` — that colour IS the row's whole signal.
fn lineage_name_style(color: Color, status: button::Status) -> button::Style {
    let tint = |a: f32| Some(Background::Color(Color { a, ..SURFACE }));
    button::Style {
        background: match status {
            button::Status::Hovered => tint(0.4),
            button::Status::Pressed => tint(0.6),
            _ => None,
        },
        text_color: color,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 4.0.into(),
        },
        ..button::Style::default()
    }
}

/// Artifacts attached to the focused channel — diffs, logs, charts. Rendered
/// from the focused pane's existing artifact state rather than a new fetch.
fn artifacts_view(app: &App) -> Element<'_, Message> {
    let Some(id) = app.focus else {
        return text("no channel focused")
            .size(TEXT_BODY)
            .color(MUTED)
            .into();
    };
    let Some(pane) = app.panes.get(id) else {
        return text("no channel focused")
            .size(TEXT_BODY)
            .color(MUTED)
            .into();
    };
    match &pane.content {
        Content::Loading => return text("loading…").size(TEXT_BODY).color(MUTED).into(),
        Content::Error(err) => {
            return row![
                icon(ICON_CIRCLE_ALERT).color(RED),
                text(err).size(TEXT_BODY).color(RED)
            ]
            .spacing(SP_TIGHT)
            .align_y(Center)
            .into();
        }
        Content::Loaded(_) => {}
    }
    let mut items = column![].spacing(SP_TIGHT);
    for entry in pane.artifact_entries() {
        items = items.push(artifact_row(id, pane, entry));
    }
    scrollable(items).height(Fill).into()
}

/// Agent sessions for the focused channel.
fn sessions_view(app: &App) -> Element<'_, Message> {
    let Some(id) = app.focus else {
        return text("no channel focused")
            .size(TEXT_BODY)
            .color(MUTED)
            .into();
    };
    let Some(pane) = app.panes.get(id) else {
        return text("no channel focused")
            .size(TEXT_BODY)
            .color(MUTED)
            .into();
    };
    match &pane.content {
        Content::Loading => return text("loading…").size(TEXT_BODY).color(MUTED).into(),
        Content::Error(err) => {
            return row![
                icon(ICON_CIRCLE_ALERT).color(RED),
                text(err).size(TEXT_BODY).color(RED)
            ]
            .spacing(SP_TIGHT)
            .align_y(Center)
            .into();
        }
        Content::Loaded(_) => {}
    }
    let mut items = column![].spacing(SP_TIGHT);
    for session in pane.session_list() {
        items = items.push(session_row(id, pane, session));
        // Only one inline row at a time, for whichever chip is pending —
        // matched by channel and session id, the sessions-panel analogue
        // of `attention_view`'s own entry-id match.
        if let Some(pending) = &app.pending
            && pending.channel == pane.channel
            && matches!(&pending.target, PendingTarget::Session(session_id) if session_id == &session.id)
        {
            items = items.push(inline_placement_row(pending));
        }
    }
    scrollable(items).into()
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
    let (toggle_icon, toggle_text) = if expanded.is_some() {
        (ICON_CHEVRON_DOWN, "hide content")
    } else {
        (ICON_CHEVRON_RIGHT, "show content")
    };
    let mut card = column![
        row![
            badge(artifact_label(&entry.summary), kind_color(&entry.kind)),
            text(truncate(&entry.author, 24))
                .size(TEXT_META)
                .color(MUTED),
        ]
        .spacing(SP),
        button(
            row![icon(toggle_icon), text(toggle_text).size(TEXT_META)]
                .spacing(SP_TIGHT)
                .align_y(Center),
        )
        .on_press(Message::ToggleArtifact(id, entry.id.clone()))
        .padding([SP_TIGHT, SP])
        .style(|_t, _s| chip_style(TEAL, false)),
    ]
    .spacing(SP);
    match expanded {
        Some(ArtifactContent::Loading) => {
            card = card.push(text("loading…").size(TEXT_META).color(MUTED));
        }
        Some(ArtifactContent::Error(err)) => {
            card = card.push(
                row![
                    icon(ICON_CIRCLE_ALERT).color(RED),
                    text(err).size(TEXT_META).color(RED)
                ]
                .spacing(SP_TIGHT)
                .align_y(Center),
            );
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
        .padding(SP)
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

/// One session chip in the left blade's Sessions view: the same
/// intent/state label and colour as the pane's own inline session-chip
/// row, stacked full-width instead of run inline so it fits the blade
/// rather than overflowing it. The watched session's chip closes the
/// session view on press (`Message::CloseSession`) — the toggle-off a
/// channel chip's own press already performs, matching `chip_style`'s
/// active flag so the watched one reads as selected. An unwatched
/// session's chip instead opens (or, pressed again, closes) an inline
/// placement row (`Message::SessionToggled`, `sessions_view`).
fn session_row<'a>(
    id: pane_grid::Pane,
    pane: &Pane,
    session: &'a SessionDto,
) -> Element<'a, Message> {
    let watching = pane.watched.as_deref() == Some(session.id.as_str());
    let label = format!("{} · {}", truncate(&session.intent, 22), session.state);
    let message = if watching {
        Message::CloseSession(id)
    } else {
        Message::SessionToggled(pane.channel.clone(), session.id.clone())
    };
    button(text(label).size(TEXT_META))
        .on_press(message)
        .width(Fill)
        .padding([SP_TIGHT, SP])
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
        .padding(SP_LOOSE)
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
    let mut col = column![text("settings").size(TEXT_TITLE).font(semibold())].spacing(SP_SECTION);
    if let Some(s) = &app.settings {
        let kv = |k: &str, v: &str| {
            row![
                text(format!("{k}:"))
                    .size(TEXT_BODY)
                    .color(MUTED)
                    .width(110),
                text(v.to_string()).size(TEXT_BODY).color(TEXT),
            ]
            .spacing(SP)
        };
        let mut harness = column![
            text("harness").size(TEXT_BODY).color(TEAL),
            kv("protocol", &s.harness.protocol),
            kv("backend", &s.harness.backend),
            kv("auth", &s.harness.auth),
            kv("detail", &s.harness.detail),
        ]
        .spacing(SP_TIGHT);
        if let Some(hint) = &s.harness.hint {
            harness = harness.push(text(format!("hint: {hint}")).size(TEXT_META).color(YELLOW));
        }
        col = col.push(admin_card(harness));
        let mut subs =
            column![text("home substrates").size(TEXT_BODY).color(TEAL)].spacing(SP_TIGHT);
        for p in &s.substrates {
            subs = subs.push(text(p.clone()).size(TEXT_BODY).color(TEXT));
        }
        col = col.push(admin_card(subs));
        let mut device = column![text("this device").size(TEXT_BODY).color(TEAL)].spacing(SP_TIGHT);
        match &s.identity {
            Some(i) => {
                device = device.push(
                    text(format!("{} <{}>", i.name, i.email))
                        .size(TEXT_BODY)
                        .color(TEXT),
                );
                device = device.push(match &app.device_key_fingerprint {
                    Some(fp) => row![
                        badge("key on file", GREEN),
                        text(fp.clone()).size(TEXT_META).color(MUTED),
                    ]
                    .spacing(SP)
                    .align_y(Center),
                    None => row![
                        text("no device key on file — join a channel below to mint one")
                            .size(TEXT_META)
                            .color(YELLOW)
                    ],
                });
            }
            None => {
                device = device.push(text("(no git identity)").size(TEXT_BODY).color(MUTED));
            }
        }
        col = col.push(admin_card(device));
        col = col.push(
            text(format!("junto {}", s.version))
                .size(TEXT_META)
                .color(MUTED),
        );
    } else {
        col = col.push(text("loading…").size(TEXT_BODY).color(MUTED));
    }

    // Join a channel: paste a founder's invite to mint this device's key
    // pair (`POST /devices/enroll`) — the joiner half of pairing a second
    // machine, replacing `junto enroll` in a terminal.
    let mut join = column![text("join a channel").size(TEXT_BODY).color(TEAL)].spacing(SP);
    join = join.push(
        text_input("paste an invite (junto://enroll?code=…)…", &app.join_invite)
            .on_input(Message::JoinInviteChanged)
            .size(TEXT_BODY)
            .padding(SP),
    );
    let can_join = !app.join_pending && !app.join_invite.trim().is_empty();
    join = join.push(
        button(text(if app.join_pending {
            "joining…"
        } else {
            "join"
        }))
        .on_press_maybe(can_join.then_some(Message::JoinSubmit))
        .padding(SP),
    );
    if let Some(err) = &app.join_error {
        join = join.push(
            row![
                icon(ICON_CIRCLE_ALERT).color(RED),
                text(err).size(TEXT_META).color(RED)
            ]
            .spacing(SP_TIGHT)
            .align_y(Center),
        );
    }
    if let Some(enrolled) = &app.join_result {
        join = join.push(
            column![
                text(format!("joined as {}", enrolled.email))
                    .size(TEXT_BODY)
                    .color(GREEN),
                row![
                    text("enroll code (hand this to the founder)")
                        .size(TEXT_META)
                        .color(MUTED),
                    copy_button(enrolled.url.clone()),
                ]
                .spacing(SP)
                .align_y(Center),
                text(enrolled.url.clone()).size(TEXT_META).color(TEXT),
                text(format!(
                    "fingerprint (read aloud): {}",
                    enrolled.fingerprint
                ))
                .size(TEXT_META)
                .color(TEXT),
                text(format!(
                    "transport fingerprint: {}",
                    enrolled.transport_fingerprint
                ))
                .size(TEXT_META)
                .color(TEXT),
                text("your secret key never leaves this machine")
                    .size(TEXT_META)
                    .color(MUTED),
            ]
            .spacing(SP_TIGHT),
        );
    }
    col = col.push(admin_card(join));

    // Register a repo as a home substrate — the GUI `junto init`.
    let mut repo = column![
        text("register a repo (home substrate)")
            .size(TEXT_BODY)
            .color(TEAL)
    ]
    .spacing(SP);
    repo = repo.push(
        row![
            text_input("git repo path…", &app.repo_path)
                .on_input(Message::RepoPathChanged)
                .padding(SP),
            button("browse…").on_press(Message::BrowseRepo).padding(SP),
        ]
        .spacing(SP)
        .align_y(Center),
    );
    repo = repo.push(
        text_input(
            "ambient channel name (optional; defaults to the dir name)",
            &app.repo_channel,
        )
        .on_input(Message::RepoChannelChanged)
        .size(TEXT_BODY)
        .padding(SP),
    );
    repo = repo.push(button("register").on_press(Message::SetupRepo).padding(SP));
    if let Some(msg) = &app.repo_msg {
        let status: Element<Message> = match msg {
            Ok(m) => text(m.clone()).size(TEXT_META).color(GREEN).into(),
            Err(e) => row![
                icon(ICON_CIRCLE_ALERT).color(RED),
                text(e).size(TEXT_META).color(RED)
            ]
            .spacing(SP_TIGHT)
            .align_y(Center)
            .into(),
        };
        repo = repo.push(status);
    }
    col = col.push(admin_card(repo));
    scrollable(col).height(Fill).into()
}

/// The agents view: the configured agents with edit/delete, plus a create/edit
/// form (core fields — name, harness, role, model).
fn agents_panel(app: &App) -> Element<'_, Message> {
    let mut list = column![text("agents").size(TEXT_TITLE).font(semibold())].spacing(SP);
    if app.agents.is_empty() {
        list = list.push(text("no agents configured").size(TEXT_BODY).color(MUTED));
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
                text(format!("{} · {}{}", a.name, a.harness, detail)).size(TEXT_BODY),
                text(truncate(&role, 70)).size(TEXT_META).color(MUTED),
            ]
            .spacing(SP_TIGHT),
            Space::new().width(Fill),
            button(text("edit").size(TEXT_META))
                .on_press(Message::AgentEdit(a.clone()))
                .padding([SP_TIGHT, SP])
                .style(|_t, _s| chip_style(BLUE, false)),
            button(text("delete").size(TEXT_META))
                .on_press(Message::DeleteAgent(a.slug.clone()))
                .padding([SP_TIGHT, SP])
                .style(|_t, _s| chip_style(RED, false)),
        ]
        .spacing(SP)
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
            .size(TEXT_BODY)
            .color(TEAL)
    ]
    .spacing(SP);
    form = form.push(
        text_input("name (e.g. Security Reviewer)", &app.agent_name)
            .on_input(Message::AgentNameChanged)
            .padding(SP),
    );
    if !harnesses.is_empty() {
        form = form.push(
            pick_list(
                harnesses,
                app.agent_harness.clone(),
                Message::AgentHarnessPicked,
            )
            .placeholder("harness")
            .text_size(TEXT_BODY)
            .padding(SP),
        );
    }
    form = form.push(
        text_input("role / system prompt (optional)", &app.agent_role)
            .on_input(Message::AgentRoleChanged)
            .size(TEXT_BODY)
            .padding(SP),
    );
    form = form.push(
        text_input("model override (optional)", &app.agent_model)
            .on_input(Message::AgentModelChanged)
            .size(TEXT_BODY)
            .padding(SP),
    );

    // --- advanced config: MCP servers, skills, local plugins ---
    let remove_btn = |msg: Message| {
        with_tip(
            icon_button_raw(ICON_X, Some(msg)).style(|_t, _s| chip_style(RED, false)),
            "remove this entry",
            tooltip::Position::Top,
        )
    };
    let add_btn = |label: &'static str, msg: Message| {
        button(text(label).size(TEXT_META))
            .on_press(msg)
            .padding([SP_TIGHT, SP])
            .style(|_t, _s| chip_style(MUTED, false))
    };

    let mut mcp = column![text("MCP servers").size(TEXT_META).color(MUTED)].spacing(SP_TIGHT);
    for (i, (name, url)) in app.agent_mcp.iter().enumerate() {
        mcp = mcp.push(
            row![
                text_input("name", name)
                    .on_input(move |v| Message::McpNameChanged(i, v))
                    .size(TEXT_BODY)
                    .padding(SP)
                    .width(Length::FillPortion(1)),
                text_input("https://…/mcp", url)
                    .on_input(move |v| Message::McpUrlChanged(i, v))
                    .size(TEXT_BODY)
                    .padding(SP)
                    .width(Length::FillPortion(2)),
                remove_btn(Message::McpRemove(i)),
            ]
            .spacing(SP)
            .align_y(Center),
        );
    }
    mcp = mcp.push(add_btn("+ add server", Message::McpAddRow));
    form = form.push(mcp);

    let mut skills = column![text("skills").size(TEXT_META).color(MUTED)].spacing(SP_TIGHT);
    for (i, s) in app.agent_skills.iter().enumerate() {
        skills = skills.push(
            row![
                text_input("skill name (or plugin:skill)", s)
                    .on_input(move |v| Message::SkillChanged(i, v))
                    .size(TEXT_BODY)
                    .padding(SP),
                remove_btn(Message::SkillRemove(i)),
            ]
            .spacing(SP)
            .align_y(Center),
        );
    }
    skills = skills.push(add_btn("+ add skill", Message::SkillAddRow));
    form = form.push(skills);

    let mut plugins = column![text("local plugins").size(TEXT_META).color(MUTED)].spacing(SP_TIGHT);
    for (i, p) in app.agent_plugins.iter().enumerate() {
        plugins = plugins.push(
            row![
                text_input("absolute plugin directory", p)
                    .on_input(move |v| Message::PluginChanged(i, v))
                    .size(TEXT_BODY)
                    .padding(SP),
                add_btn("browse…", Message::PluginBrowse(i)),
                remove_btn(Message::PluginRemove(i)),
            ]
            .spacing(SP)
            .align_y(Center),
        );
    }
    plugins = plugins.push(add_btn("+ add plugin", Message::PluginAddRow));
    form = form.push(plugins);

    let mut actions = row![button("save").on_press(Message::SaveAgent).padding(SP)].spacing(SP);
    if editing {
        actions = actions.push(
            button(text("new").size(TEXT_BODY))
                .on_press(Message::AgentNew)
                .padding(SP)
                .style(|_t, _s| chip_style(MUTED, false)),
        );
    }
    form = form.push(actions);
    if let Some(msg) = &app.agent_msg {
        form = form.push(
            row![
                icon(ICON_CIRCLE_ALERT).color(RED),
                text(msg).size(TEXT_META).color(RED)
            ]
            .spacing(SP_TIGHT)
            .align_y(Center),
        );
    }
    scrollable(column![list, admin_card(form)].spacing(SP_SECTION))
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
        text("remote").size(TEXT_META).color(MUTED),
        text_input("host (blank = local)", pane.remote.as_deref().unwrap_or(""))
            .on_input(move |v| Message::RemoteChanged(id, v))
            .size(TEXT_META)
            .padding(SP_TIGHT)
            .width(Length::FillPortion(2)),
        text_input(&watch_placeholder, &pane.watch_email)
            .on_input(move |v| Message::WatchEmailChanged(id, v))
            .size(TEXT_META)
            .padding(SP_TIGHT)
            .width(Length::FillPortion(1)),
    ]
    .spacing(SP)
    .align_y(Center);
    let mut col = column![inputs].spacing(SP_TIGHT);
    // The live subscription's id is keyed on (session, stream_nonce) — not on
    // these fields — so it keeps a keystroke from tearing down and
    // reconnecting the socket on every character. The cost is that editing
    // either field has no effect on an already-running watch.
    if pane.streaming {
        col = col.push(
            text("applies on next watch — doesn't affect the running connection")
                .size(TEXT_META)
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
        .spacing(SP),
    )
    .width(Fill)
    .height(Fill)
    .padding(SP)
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
        text("brief").size(TEXT_META).color(TEAL),
        Space::new().width(Fill),
        copy_button(raw.to_string()),
    ]
    .align_y(Center);
    container(column![head, body].spacing(SP))
        .padding(SP_LOOSE)
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
    let mut col = column![].spacing(SP);
    match kind {
        LifecycleKind::Diverge => {
            col = col.push(
                text_input("side-quest name…", &pane.lifecycle_text)
                    .on_input(move |v| Message::LifecycleTextChanged(id, v))
                    .on_submit(Message::LifecycleSubmit(id))
                    .size(TEXT_BODY)
                    .padding(SP),
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
                    .text_size(TEXT_BODY)
                    .padding(SP)
                    .width(Fill),
                )
                .push(
                    text_input("rationale (required)…", &pane.lifecycle_text)
                        .on_input(move |v| Message::LifecycleTextChanged(id, v))
                        .on_submit(Message::LifecycleSubmit(id))
                        .size(TEXT_BODY)
                        .padding(SP),
                );
        }
        LifecycleKind::Rename => {
            col = col
                .push(
                    text_input("new channel name…", &pane.lifecycle_target)
                        .on_input(move |v| Message::LifecycleTargetChanged(id, v))
                        .size(TEXT_BODY)
                        .padding(SP),
                )
                .push(
                    text_input("rationale (required)…", &pane.lifecycle_text)
                        .on_input(move |v| Message::LifecycleTextChanged(id, v))
                        .on_submit(Message::LifecycleSubmit(id))
                        .size(TEXT_BODY)
                        .padding(SP),
                );
        }
        LifecycleKind::Close | LifecycleKind::Reopen => {
            col = col.push(
                text_input("rationale (required)…", &pane.lifecycle_text)
                    .on_input(move |v| Message::LifecycleTextChanged(id, v))
                    .on_submit(Message::LifecycleSubmit(id))
                    .size(TEXT_BODY)
                    .padding(SP),
            );
        }
    }
    let confirm_label = if pane.lifecycle_pending {
        "working…"
    } else {
        kind.label()
    };
    let mut confirm = button(text(confirm_label).size(TEXT_META))
        .padding([SP_TIGHT, SP_LOOSE])
        .style(|_t, _s| chip_style(GREEN, true));
    if !pane.lifecycle_pending {
        confirm = confirm.on_press(Message::LifecycleSubmit(id));
    }
    col = col.push(
        row![
            confirm,
            button(text("cancel").size(TEXT_META))
                .on_press(Message::LifecycleCancel(id))
                .padding([SP_TIGHT, SP_LOOSE])
                .style(|_t, _s| chip_style(MUTED, false)),
        ]
        .spacing(SP),
    );
    if let Some(err) = &pane.lifecycle_error {
        col = col.push(
            row![
                icon(ICON_CIRCLE_ALERT).color(RED),
                text(err).size(TEXT_META).color(RED)
            ]
            .spacing(SP_TIGHT)
            .align_y(Center),
        );
    }
    container(col)
        .padding(SP)
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
            Some(err) => text(format!("members: {err}"))
                .size(TEXT_BODY)
                .color(RED)
                .into(),
            None => text("members · loading…")
                .size(TEXT_BODY)
                .color(MUTED)
                .into(),
        };
    };
    let device_count: usize = keys.members.iter().map(|m| m.devices.len()).sum();
    let members_chevron = if pane.members_open {
        ICON_CHEVRON_DOWN
    } else {
        ICON_CHEVRON_RIGHT
    };
    let header = button(
        row![
            icon(members_chevron),
            text(format!(
                "members ({}) · devices: {device_count}",
                keys.members.len(),
            ))
            .size(TEXT_BODY),
        ]
        .spacing(SP_TIGHT)
        .align_y(Center),
    )
    .on_press(Message::MembersToggle(id))
    .padding(SP)
    .style(|_t, _s| chip_style(MUTED, false));

    let mut col = column![header].spacing(SP);
    if let Some(err) = &pane.keys_error {
        col = col.push(
            row![
                icon(ICON_CIRCLE_ALERT).color(RED),
                text(err).size(TEXT_META).color(RED)
            ]
            .spacing(SP_TIGHT)
            .align_y(Center),
        );
    }
    if pane.members_open {
        for member in &keys.members {
            col = col.push(member_row(id, pane, keys, member));
        }
        if keys.viewer_is_founder {
            let mut acts = row![text("members").size(TEXT_META).color(MUTED)]
                .spacing(SP)
                .align_y(Center);
            for form in [IdentityForm::Invite, IdentityForm::Redeem] {
                let active = pane.identity_form.as_ref() == Some(&form);
                let label = form.label();
                acts = acts.push(
                    button(text(label).size(TEXT_META))
                        .on_press(Message::IdentitySelect(id, form))
                        .padding([SP_TIGHT, SP])
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
                        text(notice.clone()).size(TEXT_META).color(GREEN),
                        button(text("dismiss").size(TEXT_META))
                            .on_press(Message::IdentityCancel(id))
                            .padding([SP_TIGHT, SP])
                            .style(|_t, _s| chip_style(MUTED, false)),
                    ]
                    .spacing(SP)
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
        text(&member.display_name).size(TEXT_BODY),
        badge(&member.kind, kind_badge_color),
    ]
    .spacing(SP)
    .align_y(Center);
    let can_revoke =
        keys.viewer_is_founder && !member.revoked && member.email != keys.founder_email;
    if can_revoke {
        let target = IdentityForm::Revoke {
            email: member.email.clone(),
        };
        let active = pane.identity_form.as_ref() == Some(&target);
        head = head.push(
            button(text("revoke").size(TEXT_META))
                .on_press(Message::IdentitySelect(id, target))
                .padding([SP_TIGHT, SP])
                .style(move |_t, _s| chip_style(RED, active)),
        );
    }

    let mut col = column![
        head,
        text(member_summary(member)).size(TEXT_META).color(MUTED)
    ]
    .spacing(SP_TIGHT)
    .padding(Padding::default().left(SP_LOOSE));
    for grant in &member.devices {
        let mut line = row![text(device_line(grant)).size(TEXT_META).color(MUTED)]
            .spacing(SP)
            .align_y(Center);
        let can_retire = keys.viewer_is_founder && grant.retired_at.is_none();
        if can_retire {
            let target = IdentityForm::Retire {
                grant: grant.granted_by.clone(),
            };
            let active = pane.identity_form.as_ref() == Some(&target);
            line = line.push(
                button(text("retire").size(TEXT_META))
                    .on_press(Message::IdentitySelect(id, target))
                    .padding([SP_TIGHT, SP])
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
    let mut col = column![].spacing(SP);
    // Whether the confirm button may activate at all — each kind's own
    // required-input rule, checked here so an unmet requirement makes the
    // button simply absent-of-`on_press`, never present-but-silently-refusing.
    let mut ready = true;
    match form {
        IdentityForm::Invite => {
            col = col.push(
                text_input("member email…", &pane.identity_member)
                    .on_input(move |v| Message::IdentityInput(id, IdentityField::Member, v))
                    .size(TEXT_BODY)
                    .padding(SP),
            );
            let mut channels = row![text("channels").size(TEXT_META).color(MUTED)]
                .spacing(SP)
                .align_y(Center);
            for (idx, (name, ticked)) in pane.identity_channels.iter().enumerate() {
                channels = channels.push(
                    checkbox(*ticked)
                        .label(name.clone())
                        .on_toggle(move |_| Message::IdentityChannelToggle(id, idx))
                        .size(TEXT_BODY)
                        .text_size(TEXT_META),
                );
            }
            col = col.push(channels);
            let any_channel = pane.identity_channels.iter().any(|(_, on)| *on);
            ready = !pane.identity_member.trim().is_empty() && any_channel;
            if let Some(dto) = &pane.invite_minted {
                let remaining = countdown(dto.expires_at, now_millis());
                let status: Element<Message> = if remaining == "expired" {
                    text("expired — mint another")
                        .size(TEXT_META)
                        .color(YELLOW)
                        .into()
                } else {
                    column![
                        row![
                            text(dto.url.clone()).size(TEXT_META).color(TEAL),
                            copy_button(dto.url.clone()),
                            text(remaining).size(TEXT_META).color(MUTED),
                        ]
                        .spacing(SP)
                        .align_y(Center),
                        text(format!("covers: {}", dto.channels.join(", ")))
                            .size(TEXT_META)
                            .color(MUTED),
                    ]
                    .spacing(SP_TIGHT)
                    .into()
                };
                col = col.push(status);
            }
        }
        IdentityForm::Redeem => {
            if !pane.redeem_outcomes.is_empty() {
                let mut outcomes = column![].spacing(SP_TIGHT);
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
                            .size(TEXT_META)
                            .color(color),
                    );
                    if let Some(warning) = outcome.warning.as_deref() {
                        outcomes = outcomes
                            .push(text(format!("  {warning}")).size(TEXT_META).color(YELLOW));
                    }
                }
                col = col.push(outcomes);
            } else {
                col = col.push(
                    text_input("enroll code (pasted invite)…", &pane.identity_paste)
                        .on_input(move |v| Message::IdentityInput(id, IdentityField::Paste, v))
                        .size(TEXT_BODY)
                        .padding(SP),
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
                        .size(TEXT_META)
                        .color(MUTED),
                    );
                    let mut kind_row = row![text("kind").size(TEXT_META).color(MUTED)]
                        .spacing(SP)
                        .align_y(Center);
                    for kind in ["human", "agent"] {
                        let active = pane.identity_kind == kind;
                        kind_row = kind_row.push(
                            button(text(kind).size(TEXT_META))
                                .on_press(Message::IdentityInput(
                                    id,
                                    IdentityField::Kind,
                                    kind.to_string(),
                                ))
                                .padding([SP_TIGHT, SP])
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
                    .size(TEXT_BODY)
                    .padding(SP),
            );
            ready = !pane.identity_rationale.trim().is_empty();
        }
        IdentityForm::Revoke { .. } => {
            col = col.push(
                text("the member stays in the party; only their entries after now stop counting.")
                    .size(TEXT_META)
                    .color(MUTED),
            );
            col = col.push(
                text_input("rationale (required)…", &pane.identity_rationale)
                    .on_input(move |v| Message::IdentityInput(id, IdentityField::Rationale, v))
                    .on_submit(Message::IdentitySubmit(id))
                    .size(TEXT_BODY)
                    .padding(SP),
            );
            ready = !pane.identity_rationale.trim().is_empty();
        }
    }

    let outcomes_shown = matches!(form, IdentityForm::Redeem) && !pane.redeem_outcomes.is_empty();
    if outcomes_shown {
        col = col.push(
            button(text("dismiss").size(TEXT_META))
                .on_press(Message::IdentityCancel(id))
                .padding([SP_TIGHT, SP_LOOSE])
                .style(|_t, _s| chip_style(MUTED, false)),
        );
    } else {
        let confirm_label = if pane.identity_pending {
            "working…"
        } else {
            form.label()
        };
        let mut confirm = button(text(confirm_label).size(TEXT_META))
            .padding([SP_TIGHT, SP_LOOSE])
            .style(|_t, _s| chip_style(GREEN, true));
        if !pane.identity_pending && ready {
            confirm = confirm.on_press(Message::IdentitySubmit(id));
        }
        col = col.push(
            row![
                confirm,
                button(text("cancel").size(TEXT_META))
                    .on_press(Message::IdentityCancel(id))
                    .padding([SP_TIGHT, SP_LOOSE])
                    .style(|_t, _s| chip_style(MUTED, false)),
            ]
            .spacing(SP),
        );
    }
    if let Some(err) = &pane.identity_error {
        col = col.push(
            row![
                icon(ICON_CIRCLE_ALERT).color(RED),
                text(err).size(TEXT_META).color(RED)
            ]
            .spacing(SP_TIGHT)
            .align_y(Center),
        );
    }
    container(col)
        .padding(SP)
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
        .size(TEXT_BODY)
        .padding(SP)
        .width(Length::FillPortion(2));
    let lines_input = text_input("lines (\"12\" or \"12-14\")", &pane.annotate_lines)
        .on_input(move |v| Message::AnnotateLinesChanged(id, v))
        .size(TEXT_BODY)
        .padding(SP)
        .width(Length::FillPortion(1));
    let body_input = text_input("annotate…", &pane.annotate_body)
        .on_input(move |v| Message::AnnotateBodyChanged(id, v))
        .on_submit(Message::AnnotateSubmit(id))
        .size(TEXT_BODY)
        .padding(SP);
    let urgent = checkbox(pane.annotate_urgent)
        .label("urgent")
        .on_toggle(move |on| Message::AnnotateUrgentToggled(id, on))
        .size(TEXT_BODY)
        .text_size(TEXT_META);
    let submit = button(text("comment").size(TEXT_META))
        .on_press(Message::AnnotateSubmit(id))
        .padding(SP);
    // What this comment will actually be anchored to, stated plainly. The
    // inputs alone can't say it: an empty path could mean the newest event or a
    // block that was pointed at, and those land on different ops.
    let mut aimed = row![text(aim_label(pane)).size(TEXT_META).color(TEAL)]
        .spacing(SP)
        .align_y(Center);
    if pane.annotate_op.is_some() || !pane.annotate_path.trim().is_empty() {
        aimed = aimed.push(
            button(text("clear").size(TEXT_META))
                .on_press(Message::AnchorClear(id))
                .padding([SP_TIGHT, SP])
                .style(|_t, _s| chip_style(MUTED, false)),
        );
    }
    column![
        row![text("annotate").size(TEXT_META).color(MUTED), aimed]
            .spacing(SP)
            .align_y(Center),
        row![path_input, lines_input].spacing(SP),
        row![body_input, urgent, submit].spacing(SP).align_y(Center),
    ]
    .spacing(SP_TIGHT)
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
        text(aim_label(pane)).size(TEXT_META).color(TEAL),
        Space::new().width(Fill),
        with_tip(
            icon_button_raw(ICON_X, Some(Message::AnchorClear(id)))
                .style(|_t, _s| chip_style(MUTED, false)),
            "clear this aim",
            tooltip::Position::Bottom,
        ),
    ]
    .spacing(SP)
    .align_y(Center);
    let body_input = text_input("comment on these lines…", &pane.annotate_body)
        .on_input(move |v| Message::AnnotateBodyChanged(id, v))
        .on_submit(Message::AnnotateSubmit(id))
        .size(TEXT_BODY)
        .padding(SP);
    let urgent = checkbox(pane.annotate_urgent)
        .label("urgent")
        .on_toggle(move |on| Message::AnnotateUrgentToggled(id, on))
        .size(TEXT_BODY)
        .text_size(TEXT_META);
    let submit = button(text("comment").size(TEXT_META))
        .on_press(Message::AnnotateSubmit(id))
        .padding(SP);
    container(
        column![
            head,
            body_input,
            row![Space::new().width(Fill), urgent, submit]
                .spacing(SP)
                .align_y(Center),
        ]
        .spacing(SP),
    )
    .padding(SP_LOOSE)
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
            return container(text("loading…").color(MUTED))
                .padding(SP_LOOSE)
                .into();
        }
        Content::Error(err) => {
            return container(text(format!("error: {err}")).color(RED))
                .padding(SP_LOOSE)
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
    let mut header = column![].spacing(SP);
    header = header.push(members_disclosure(id, pane));
    if dto.closed {
        header = header.push(row![badge("closed", RED)].spacing(SP).align_y(Center));
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
    let mut bar = row![text("channel").size(TEXT_META).color(MUTED)]
        .spacing(SP)
        .align_y(Center);
    for &k in acts {
        let active = pane.lifecycle == Some(k);
        bar = bar.push(
            button(text(k.label()).size(TEXT_META))
                .on_press(Message::LifecycleSelect(id, k))
                .padding([SP_TIGHT, SP])
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
        .padding(SP);
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
    .padding(SP);
    if !pane.launching {
        launch_btn = launch_btn.on_press(Message::Launch(id));
    }
    let options_chevron = if pane.launch_expanded {
        ICON_CHEVRON_DOWN
    } else {
        ICON_CHEVRON_RIGHT
    };
    let options_toggle = button(
        row![icon(options_chevron), text("options").size(TEXT_BODY)]
            .spacing(SP_TIGHT)
            .align_y(Center),
    )
    .on_press(Message::ToggleLaunchOptions(id))
    .padding(SP)
    .style(|_t, _s| chip_style(MUTED, false));
    let intent_row = row![intent_input, launch_btn, options_toggle].spacing(SP);
    let agent_picker: Element<Message> = if agents.is_empty() {
        text("no agents configured")
            .size(TEXT_META)
            .color(MUTED)
            .into()
    } else {
        pick_list(agents.to_vec(), pane.launch_agent.clone(), move |a| {
            Message::LaunchAgentPicked(id, a)
        })
        .placeholder("default agent")
        .text_size(TEXT_BODY)
        .padding(SP)
        .into()
    };
    // Mode as a checkbox (matches the web): unchecked = a single turn (default);
    // checked = the code-PR push-gate verify/Grader loop (docs/adr/0025).
    let mode_checkbox = checkbox(pane.launch_outcome)
        .label("code-PR push-gate (verify loop)")
        .on_toggle(move |on| Message::LaunchModeChanged(id, on))
        .size(TEXT_BODY)
        .text_size(TEXT_BODY);
    let options_row = row![
        agent_picker,
        text_input(
            "workspace repo path (remembered after first launch)",
            &pane.launch_workspace,
        )
        .on_input(move |v| Message::LaunchWorkspaceChanged(id, v))
        .size(TEXT_BODY)
        .padding(SP),
        button(text("browse…").size(TEXT_BODY))
            .on_press(Message::BrowseWorkspace(id))
            .padding(SP),
    ]
    .spacing(SP)
    .align_y(Center);
    let mut launch = column![intent_row].spacing(SP);
    if pane.launch_expanded {
        launch = launch.push(options_row).push(mode_checkbox);
    }
    if let Some(err) = &pane.launch_error {
        launch = launch.push(
            row![
                icon(ICON_CIRCLE_ALERT).color(RED),
                text(err).size(TEXT_META).color(RED)
            ]
            .spacing(SP_TIGHT)
            .align_y(Center),
        );
    }

    // Session chips — click to stream a session's live feed.
    let mut chips = row![].spacing(SP);
    for session in &dto.sessions {
        let watching = pane.watched.as_deref() == Some(session.id.as_str());
        let label = format!("{} · {}", truncate(&session.intent, 22), session.state);
        let chip = button(text(label).size(TEXT_META))
            .on_press(Message::Watch(id, session.id.clone()))
            .padding([SP_TIGHT, SP])
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
        let mut header = row![text(format!("session · {}", truncate(&intent, 36))).size(TEXT_BODY)]
            .spacing(SP)
            .align_y(Center);
        if !state_label.is_empty() {
            header = header.push(badge(&state_label, status_color(&state_label)));
        }
        if pane.streaming {
            header = header.push(text("● live").size(TEXT_META).color(GREEN));
        }
        // Presence, in the header rather than buried in the feed: who else is
        // looking at this session right now (`Message::Watchers`).
        if !pane.watchers.is_empty() {
            header = header.push(watchers_chip(&pane.watchers));
        }
        header = header.push(Space::new().width(Fill));
        header = header.push(
            button(
                row![icon(ICON_X), text("close").size(TEXT_META)]
                    .spacing(SP_TIGHT)
                    .align_y(Center),
            )
            .on_press(Message::CloseSession(id))
            .padding([SP_TIGHT, SP])
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
        let mut record = column![].spacing(SP);
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
            record = record.push(text("— live turn —").size(TEXT_META).color(MUTED));
            let mut feed = column![].spacing(SP);
            for item in &pane.feed {
                feed = feed.push(feed_block(id, item, pane.annotate_op));
            }
            if pane.streaming {
                feed = feed.push(text("● working…").size(TEXT_META).color(YELLOW));
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
            .padding(SP);
        let mut interrupt_btn = button("interrupt").padding(SP);
        if pane.streaming {
            interrupt_btn = interrupt_btn.on_press(Message::Interrupt(id));
        }
        let steer = row![
            steer_input,
            button("steer").on_press(Message::Steer(id)).padding(SP),
            interrupt_btn,
        ]
        .spacing(SP);
        let main_area: Element<Message> = match primary {
            Some(artifact) => row![
                container(code_panel(id, pane, artifact, aim)).width(Length::FillPortion(3)),
                container(record_scroll).width(Length::FillPortion(2)),
            ]
            .spacing(SP_LOOSE)
            .height(Fill)
            .into(),
            None => record_scroll.into(),
        };
        let mut session_col = column![header, main_area, steer].spacing(SP);
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
                .size(TEXT_META)
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
                    text("needs you").size(TEXT_META).color(YELLOW),
                    Space::new().width(Fill),
                    button(text("dismiss").size(TEXT_META).color(MUTED))
                        .on_press(Message::ClearHighlight(id))
                        .padding([SP_TIGHT, SP])
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
                .spacing(SP_TIGHT)
                .into()
            })
        });
        let total = dto.entries.len();
        let mut timeline = column![].spacing(SP);
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
                "hide full history".to_string()
            } else {
                format!("show full history ({total} entries)")
            };
            let history_chevron = if show_all {
                ICON_CHEVRON_DOWN
            } else {
                ICON_CHEVRON_RIGHT
            };
            timeline = timeline.push(
                button(
                    row![icon(history_chevron), text(label).size(TEXT_META)]
                        .spacing(SP_TIGHT)
                        .align_y(Center),
                )
                .on_press(Message::ToggleHistory(id))
                .padding([SP_TIGHT, SP])
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
            Some(pinned) => column![pinned, scroll].spacing(SP).into(),
            None => scroll.into(),
        }
    };

    column![header, launch, chips, main]
        .spacing(SP)
        .padding([SP, SP_LOOSE])
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
            .padding([0.0, SP_TIGHT])
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
        .spacing(SP_TIGHT)
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
                .size(TEXT_BODY)
                .color(BLUE),
        )
        .padding([SP_TIGHT, SP])
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
    text(body).size(TEXT_BODY).color(color).into()
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
        text(artifact.summary.clone()).size(TEXT_BODY).color(MUTED),
    ]
    .spacing(SP)
    .align_y(Center);
    let body: Element<Message> = match pane.artifacts.get(&artifact.id) {
        Some(ArtifactContent::Loaded {
            format,
            body,
            md,
            digest,
        }) => artifact_body(id, &artifact.id, digest, format, body, md.as_deref(), aim),
        Some(ArtifactContent::Loading) => text("loading the diff…")
            .size(TEXT_META)
            .color(MUTED)
            .into(),
        Some(ArtifactContent::Error(err)) => row![
            icon(ICON_CIRCLE_ALERT).color(RED),
            text(err).size(TEXT_META).color(RED)
        ]
        .spacing(SP_TIGHT)
        .align_y(Center)
        .into(),
        None => button(text("show the diff").size(TEXT_META))
            .on_press(Message::ToggleArtifact(id, artifact.id.clone()))
            .padding(SP)
            .into(),
    };
    column![head, scrollable(body).height(Fill)]
        .spacing(SP)
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
    .spacing(SP_LOOSE)
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
        text(entry.author.clone()).size(TEXT_META).color(MUTED)
    ]
    .spacing(SP);
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
        text(entry.summary.clone())
            .size(TEXT_BODY)
            .color(TEXT)
            .into()
    };
    let mut card = column![head, body].spacing(SP);

    // Inline verification acts on open assertions/proposals. The decision
    // frame's pre-baked options come first as one-click buttons (each carries
    // its drafted rationale — adopt it without typing, `docs/adr/0019`), then a
    // free-text fallback box for a custom rationale. Acting refetches the pane,
    // so the controls clear once the entry resolves.
    if let Some((affirm, decline)) = entry_acts(entry) {
        let entry_id = entry.id.clone();
        let mut acts = column![].spacing(SP);

        // Pre-baked frame options coherent with this entry's two acts. Stacked
        // full-width so they stay readable in a narrow pane (no horizontal
        // overflow); the act is tagged on the right of each row.
        let mut options = column![].spacing(SP_TIGHT);
        let mut has_options = false;
        for opt in &entry.frame {
            if opt.act != affirm && opt.act != decline {
                continue;
            }
            has_options = true;
            let affirmative = opt.act == affirm;
            let color = if affirmative { GREEN } else { RED };
            let inner = row![
                text(opt.label.clone()).size(TEXT_META),
                Space::new().width(Fill),
                text(opt.act.clone()).size(TEXT_META),
            ]
            .spacing(SP)
            .align_y(Center);
            let mut opt_btn = button(inner)
                .width(Fill)
                .padding([SP_TIGHT, SP_LOOSE])
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
            .size(TEXT_BODY)
            .padding(SP);
        let mut affirm_btn = button(text(affirm).size(TEXT_META))
            .padding([SP_TIGHT, SP_LOOSE])
            .style(|_t, _s| chip_style(GREEN, true));
        let mut decline_btn = button(text(decline).size(TEXT_META))
            .padding([SP_TIGHT, SP_LOOSE])
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
                .spacing(SP)
                .align_y(Center),
        );
        if pending {
            acts = acts.push(
                text("recording… (writing to the ledger)")
                    .size(TEXT_META)
                    .color(YELLOW),
            );
        } else if let Some(err) = error {
            acts = acts.push(
                row![
                    icon(ICON_CIRCLE_ALERT).color(RED),
                    text(err).size(TEXT_META).color(RED)
                ]
                .spacing(SP_TIGHT)
                .align_y(Center),
            );
        }
        card = card.push(acts);
    }

    // Artifacts: a toggle that lazy-loads the diff/memo/log content inline.
    if entry.kind == "artifact" {
        let expanded = artifact.is_some();
        let (toggle_icon, toggle_text) = if expanded {
            (ICON_CHEVRON_DOWN, "hide content")
        } else {
            (ICON_CHEVRON_RIGHT, "show content")
        };
        card = card.push(
            button(
                row![icon(toggle_icon), text(toggle_text).size(TEXT_META)]
                    .spacing(SP_TIGHT)
                    .align_y(Center),
            )
            .on_press(Message::ToggleArtifact(id, entry.id.clone()))
            .padding([SP_TIGHT, SP])
            .style(|_t, _s| chip_style(TEAL, false)),
        );
        match artifact {
            Some(ArtifactContent::Loading) => {
                card = card.push(text("loading…").size(TEXT_META).color(MUTED));
            }
            Some(ArtifactContent::Error(err)) => {
                card = card.push(
                    row![
                        icon(ICON_CIRCLE_ALERT).color(RED),
                        text(err).size(TEXT_META).color(RED)
                    ]
                    .spacing(SP_TIGHT)
                    .align_y(Center),
                );
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
        .padding(SP_LOOSE)
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
        .padding(SP)
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
    let mut col = column![].spacing(SP_TIGHT);
    for (i, line) in lines.iter().enumerate().take(MAX_DIFF_ROWS) {
        let color = if is_diff { diff_line_color(line) } else { TEXT };
        let Some(aim) = aim else {
            col = col.push(
                text((*line).to_string())
                    .font(iced::Font::MONOSPACE)
                    .size(TEXT_BODY)
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
            .size(TEXT_META)
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
        .padding(SP)
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
            .size(TEXT_BODY)
            .color(color),
    )
    .width(Fill)
    .padding([0.0, SP_TIGHT])
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
    button(text("copy").size(TEXT_META))
        .on_press(Message::Copy(text_to_copy))
        .padding([SP_TIGHT, SP])
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
    let mut chip = row![].spacing(SP_TIGHT).align_y(Center);
    for email in watchers.iter().take(MAX_AVATARS) {
        chip = chip.push(avatar(email));
    }
    if watchers.len() > MAX_AVATARS {
        chip = chip.push(
            text(format!("+{}", watchers.len() - MAX_AVATARS))
                .size(TEXT_META)
                .color(MUTED),
        );
    }
    chip.push(
        text(format!("{} watching", watchers.len()))
            .size(TEXT_META)
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
            .size(TEXT_META)
            .color(Color::from_rgb(0.12, 0.12, 0.18)),
    )
    .padding([SP_TIGHT, SP_TIGHT])
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
        container(text(email.to_string()).size(TEXT_META).color(TEXT))
            .padding([SP_TIGHT, SP])
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
            .size(TEXT_META)
            .color(Color::from_rgb(0.12, 0.12, 0.18)),
    )
    .padding([SP_TIGHT, SP])
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

/// Fetch the whole lineage DAG rendered by `lineage_view`'s vertical list.
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

/// Fetch the list of channel names for the chip list and its filter.
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
            ui.find("hide content").is_err(),
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

    /// A floating panel must never reserve layout space — `Popover`'s
    /// overlay draws over the workspace without participating in ordinary
    /// layout, so the center pane grid keeps its full height whether or
    /// not a panel is open. Guards against ever reintroducing a docked
    /// drawer that squeezes it — the pre-`Popover` regression this test
    /// used to catch when a 240px fixed-height drawer sat in the root
    /// column.
    #[test]
    fn the_center_pane_grid_keeps_real_height_with_a_bottom_panel_open() {
        let (mut app, _) = App::new();
        app.shell = shell::ShellState::default();
        app.shell.bottom = Some(shell::BottomView::Attention);

        let mut ui = iced_test::simulator(app.view());
        let outer = ui
            .find(iced::widget::Id::new("center-pane-grid"))
            .expect("the center container must be laid out")
            .bounds();

        assert!(
            outer.height > 100.0,
            "expected the center pane grid to keep real height in a \
             768px-tall window with the attention panel open, got \
             {outer:?} — a docked/reserved-space panel would squeeze it",
        );
    }

    /// The bottom status strip omits its attention segment entirely when
    /// nothing needs the user — showing "0 need you" would be worse than
    /// showing nothing, since the footer's whole point is to carry only
    /// metadata worth a glance.
    #[test]
    fn the_footer_omits_the_attention_segment_when_nothing_needs_attention() {
        let (mut app, _) = App::new();
        app.shell = shell::ShellState::default();
        assert!(
            app.focus_items.is_empty(),
            "this test assumes the default app starts with nothing needing attention"
        );

        let mut ui = iced_test::simulator(app.view());

        assert!(
            ui.find("need you").is_err(),
            "the attention segment must be entirely absent at zero, not shown as \"0 need you\""
        );
        ui.find("junto-dev")
            .expect("the focused channel's name must still be on the footer");
        ui.find("1 pane")
            .expect("the singular pane count must still be on the footer");
        ui.find("local")
            .expect("the default host must still be on the footer");
    }

    /// The footer toggle placement this task landed: xum puts the collapse
    /// chevron in the blade's own footer, not the top bar. An expanded
    /// blade must show that footer toggle; a collapsed one must show the
    /// edge tab instead — never both, never neither.
    #[test]
    fn an_expanded_blade_shows_its_footer_toggle_and_a_collapsed_one_shows_the_edge_tab() {
        let (mut app, _) = App::new();
        app.shell = shell::ShellState::default();

        {
            let mut both_expanded = iced_test::simulator(app.view());
            both_expanded
                .find(iced::widget::Id::new("left-blade-toggle"))
                .expect("the expanded left blade must show its footer toggle");
            both_expanded
                .find(iced::widget::Id::new("right-blade-toggle"))
                .expect("the expanded right blade must show its footer toggle");
            assert!(
                both_expanded
                    .find(iced::widget::Id::new("left-edge-tab"))
                    .is_err(),
                "an expanded left blade must not also render the collapsed edge tab"
            );
            assert!(
                both_expanded
                    .find(iced::widget::Id::new("right-edge-tab"))
                    .is_err(),
                "an expanded right blade must not also render the collapsed edge tab"
            );
        }

        app.shell.left_collapsed = true;
        app.shell.right_collapsed = true;
        let mut both_collapsed = iced_test::simulator(app.view());
        both_collapsed
            .find(iced::widget::Id::new("left-edge-tab"))
            .expect("the collapsed left blade must show its edge tab");
        both_collapsed
            .find(iced::widget::Id::new("right-edge-tab"))
            .expect("the collapsed right blade must show its edge tab");
        assert!(
            both_collapsed
                .find(iced::widget::Id::new("left-blade-toggle"))
                .is_err(),
            "a collapsed left blade must not render its footer toggle — it has no footer"
        );
        assert!(
            both_collapsed
                .find(iced::widget::Id::new("right-blade-toggle"))
                .is_err(),
            "a collapsed right blade must not render its footer toggle — it has no footer"
        );
    }

    /// The point of the edge tab: it must be `EDGE_TAB_W` wide, not the full
    /// `ICON_BTN` rail the previous `blade_stub` rendered — the visible
    /// difference between "the sidebar is gone but for a sliver" and "the
    /// sidebar is still a full column, just badge-only".
    #[test]
    fn a_collapsed_blades_edge_tab_is_edge_tab_wide_not_icon_button_wide() {
        let (mut app, _) = App::new();
        app.shell = shell::ShellState::default();
        app.shell.left_collapsed = true;

        let mut ui = iced_test::simulator(app.view());
        let tab = ui
            .find(iced::widget::Id::new("left-edge-tab"))
            .expect("the collapsed left blade's edge tab must be laid out")
            .bounds();

        assert!(
            (tab.width - EDGE_TAB_W).abs() < 1.0,
            "expected the edge tab to be EDGE_TAB_W ({EDGE_TAB_W}) wide, got {}",
            tab.width
        );
        assert!(
            tab.width < ICON_BTN,
            "the edge tab ({}) must be narrower than a full ICON_BTN ({ICON_BTN}) — \
             otherwise it is `blade_stub`'s rail again, not a thin tab",
            tab.width
        );
    }

    /// The footer toggle is the LAST child of the blade's own column, with
    /// the blade's content given `.height(Fill)` above it — so it sits
    /// flush to the blade's bottom edge no matter how short that content
    /// is, rather than floating directly under it.
    #[test]
    fn the_footer_toggle_sits_at_the_blades_bottom_edge_not_floating_under_its_content() {
        let (mut app, _) = App::new();
        app.shell = shell::ShellState::default();

        let mut ui = iced_test::simulator(app.view());
        let blade = ui
            .find(iced::widget::Id::new("left-blade"))
            .expect("the expanded left blade must be laid out")
            .bounds();
        let toggle = ui
            .find(iced::widget::Id::new("left-blade-toggle"))
            .expect("the expanded left blade's footer toggle must be laid out")
            .bounds();

        let gap = (blade.y + blade.height) - (toggle.y + toggle.height);
        assert!(
            (0.0..20.0).contains(&gap),
            "expected the footer toggle's bottom to sit within the blade's own \
             padding of the blade's bottom edge, got a gap of {gap} — blade {blade:?}, \
             toggle {toggle:?}; a large gap means the footer floated under a short \
             channel list instead of pinning to the bottom"
        );
    }

    /// Collapsing either blade to its thin edge tab must not squeeze the
    /// center pane grid — the same `Popover`-era regression
    /// `the_center_pane_grid_keeps_real_height_with_a_bottom_panel_open`
    /// guards for the bottom drawer, now for the edge tab.
    #[test]
    fn the_center_pane_grid_keeps_real_height_with_either_blade_collapsed() {
        let (mut app, _) = App::new();
        app.shell = shell::ShellState::default();
        app.shell.left_collapsed = true;

        {
            let mut left_collapsed = iced_test::simulator(app.view());
            let outer = left_collapsed
                .find(iced::widget::Id::new("center-pane-grid"))
                .expect("the center container must be laid out")
                .bounds();
            assert!(
                outer.height > 100.0,
                "expected the center pane grid to keep real height with the left \
                 blade collapsed to its edge tab, got {outer:?}"
            );
        }

        app.shell.left_collapsed = false;
        app.shell.right_collapsed = true;
        let mut right_collapsed = iced_test::simulator(app.view());
        let outer = right_collapsed
            .find(iced::widget::Id::new("center-pane-grid"))
            .expect("the center container must be laid out")
            .bounds();
        assert!(
            outer.height > 100.0,
            "expected the center pane grid to keep real height with the right \
             blade collapsed to its edge tab, got {outer:?}"
        );
    }
}

#[cfg(test)]
mod placement_tests {
    use super::{Placement, pane_grid};

    #[test]
    fn placing_right_splits_on_the_vertical_axis() {
        assert_eq!(Placement::Right.axis(), Some(pane_grid::Axis::Vertical));
    }

    #[test]
    fn placing_below_splits_on_the_horizontal_axis() {
        assert_eq!(Placement::Below.axis(), Some(pane_grid::Axis::Horizontal));
    }

    #[test]
    fn placing_here_has_no_axis_since_it_reuses_the_focused_pane_instead_of_splitting() {
        assert_eq!(Placement::Here.axis(), None);
    }
}

#[cfg(test)]
mod channel_chip_tests {
    use super::*;

    /// A two-pane app ("junto-dev" and "other") with both names known to
    /// the nav list, matching the shape `App::new` plus a second open
    /// channel takes once `ChannelsLoaded` has landed.
    fn two_pane_app() -> App {
        let (mut app, _) = App::new();
        let first = app.focus.expect("App::new focuses its one pane");
        app.panes
            .split(pane_grid::Axis::Vertical, first, Pane::loading("other"))
            .expect("splitting the only pane must succeed");
        app.channel_names = vec!["junto-dev".to_string(), "other".to_string()];
        app
    }

    #[test]
    fn pressing_an_open_channels_chip_closes_its_pane() {
        let mut app = two_pane_app();
        assert_eq!(app.panes.len(), 2);

        let mut ui = iced_test::simulator(channel_nav(&app));
        ui.click("other").expect("the open chip is a click target");
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(&messages[..], [Message::ChannelToggled(name)] if name.as_str() == "other"),
            "clicking an open chip must publish exactly one ChannelToggled, got {messages:?}"
        );

        for message in messages {
            let _ = app.update(message);
        }
        assert_eq!(
            app.panes.len(),
            1,
            "closing the open chip's pane must leave only its sibling"
        );
        assert!(
            app.panes
                .iter()
                .any(|(_, state)| state.channel == "junto-dev"),
            "the remaining pane must be the sibling, not the closed channel"
        );
    }

    #[test]
    fn the_last_remaining_panes_chip_cannot_be_closed() {
        let (mut app, _) = App::new();
        assert_eq!(app.panes.len(), 1);
        app.channel_names = vec!["junto-dev".to_string()];

        let mut ui = iced_test::simulator(channel_nav(&app));
        ui.click("junto-dev")
            .expect("the chip itself is still on screen");
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            messages.is_empty(),
            "a chip for the only remaining pane must not be pressable, got {messages:?}"
        );
    }

    #[test]
    fn pressing_an_unopened_channels_chip_opens_its_placement_menu() {
        let mut app = two_pane_app();
        app.channel_names.push("unopened".to_string());

        let mut ui = iced_test::simulator(channel_nav(&app));
        ui.click("unopened")
            .expect("an unopened channel's chip is a click target");
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let _ = app.update(message);
        }
        assert_eq!(
            app.pending,
            Some(PendingOpen {
                channel: "unopened".to_string(),
                target: PendingTarget::Channel,
            })
        );

        let mut menu = iced_test::simulator(channel_nav(&app));
        menu.find("split right")
            .expect("the right placement is offered");
        menu.find("split below")
            .expect("the below placement is offered");
        menu.find("use this pane")
            .expect("the here placement is offered");
    }

    #[test]
    fn pressing_an_unopened_chips_placement_menu_open_again_closes_it() {
        let mut app = two_pane_app();
        app.channel_names.push("unopened".to_string());
        app.pending = Some(PendingOpen {
            channel: "unopened".to_string(),
            target: PendingTarget::Channel,
        });

        let _ = app.update(Message::ChannelToggled("unopened".to_string()));

        assert_eq!(app.pending, None, "a second press must clear the menu");
    }

    #[test]
    fn placing_a_channel_with_no_focused_pane_falls_back_to_opening_it() {
        let (mut app, _) = App::new();
        app.focus = None;

        let _ = app.update(Message::ChannelPlaced(
            PendingOpen {
                channel: "new-channel".to_string(),
                target: PendingTarget::Channel,
            },
            Placement::Here,
        ));

        assert!(
            app.panes
                .iter()
                .any(|(_, state)| state.channel == "new-channel"),
            "with nothing focused, every placement must degrade to opening the channel"
        );
    }

    #[test]
    fn placing_a_channel_here_replaces_the_focused_panes_channel() {
        let (mut app, _) = App::new();
        let focused = app.focus.expect("App::new focuses its one pane");

        let _ = app.update(Message::ChannelPlaced(
            PendingOpen {
                channel: "replacement".to_string(),
                target: PendingTarget::Channel,
            },
            Placement::Here,
        ));

        assert_eq!(app.panes.len(), 1, "'here' must not open a new pane");
        assert_eq!(
            app.panes.get(focused).map(|state| state.channel.as_str()),
            Some("replacement"),
            "the focused pane must now show the placed channel"
        );
    }

    #[test]
    fn placing_a_channel_to_the_right_splits_the_focused_pane() {
        let (mut app, _) = App::new();

        let _ = app.update(Message::ChannelPlaced(
            PendingOpen {
                channel: "sibling".to_string(),
                target: PendingTarget::Channel,
            },
            Placement::Right,
        ));

        assert_eq!(app.panes.len(), 2, "'right' must split off a new pane");
        assert!(
            app.panes
                .iter()
                .any(|(_, state)| state.channel == "junto-dev"),
            "the original pane must survive the split"
        );
        assert!(
            app.panes
                .iter()
                .any(|(_, state)| state.channel == "sibling"),
            "the new pane must show the placed channel"
        );
    }
}

#[cfg(test)]
mod attention_chip_tests {
    use super::*;

    fn focus_item(channel: &str, entry_id: &str) -> FocusItem {
        FocusItem {
            kind: "gate".to_string(),
            entry_id: entry_id.to_string(),
            channel: channel.to_string(),
            channel_name: Some(channel.to_string()),
            author: "someone".to_string(),
            summary: "needs a look".to_string(),
        }
    }

    #[test]
    fn clicking_an_attention_chip_for_an_already_open_channel_focuses_it_and_highlights_the_entry_with_no_placement_row()
     {
        let (mut app, _) = App::new();
        app.focus_items = vec![focus_item("junto-dev", "entry-1")];

        let mut ui = iced_test::simulator(attention_view(&app));
        ui.click("gate · junto-dev · someone: needs a look")
            .expect("the attention chip is a click target");
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(
                &messages[..],
                [Message::FocusChipPicked(name, entry)]
                    if name == "junto-dev" && entry == "entry-1"
            ),
            "an already-open channel's chip must publish FocusChipPicked, got {messages:?}"
        );

        for message in messages {
            let _ = app.update(message);
        }
        let focused = app.focus.expect("App::new focuses its one pane");
        assert_eq!(
            app.panes
                .get(focused)
                .and_then(|state| state.highlight_entry.as_deref()),
            Some("entry-1"),
            "the entry must be pinned in the focused pane"
        );

        let mut ui = iced_test::simulator(attention_view(&app));
        assert!(
            ui.find("split right").is_err(),
            "an already-open channel's chip must never show a placement row"
        );
    }

    #[test]
    fn clicking_an_attention_chip_for_an_unopened_channel_shows_the_placement_row_without_opening_it()
     {
        let (mut app, _) = App::new();
        app.focus_items = vec![focus_item("unopened", "entry-2")];

        let mut ui = iced_test::simulator(attention_view(&app));
        ui.click("gate · unopened · someone: needs a look")
            .expect("the attention chip is a click target");
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(
                &messages[..],
                [Message::FocusChipToggled(name, entry)]
                    if name == "unopened" && entry == "entry-2"
            ),
            "an unopened channel's chip must publish FocusChipToggled, got {messages:?}"
        );

        for message in messages {
            let _ = app.update(message);
        }
        assert!(
            !app.panes
                .iter()
                .any(|(_, state)| state.channel == "unopened"),
            "showing the placement row must not itself open a pane"
        );

        let mut menu = iced_test::simulator(attention_view(&app));
        menu.find("split right")
            .expect("the right placement is offered inline");
        menu.find("split below")
            .expect("the below placement is offered inline");
        menu.find("use this pane")
            .expect("the here placement is offered inline");
    }

    #[test]
    fn choosing_an_inline_placement_opens_the_pane_and_pins_the_entry() {
        let (mut app, _) = App::new();
        app.focus_items = vec![focus_item("unopened", "entry-3")];
        app.pending = Some(PendingOpen {
            channel: "unopened".to_string(),
            target: PendingTarget::Entry("entry-3".to_string()),
        });

        let mut ui = iced_test::simulator(attention_view(&app));
        ui.click("split right")
            .expect("the inline right placement is a click target");
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let _ = app.update(message);
        }

        let pane = app
            .panes
            .iter()
            .find(|(_, state)| state.channel == "unopened")
            .map(|(id, _)| id)
            .copied()
            .expect("the placement must open a pane for the pending channel");
        assert_eq!(
            app.panes
                .get(pane)
                .and_then(|state| state.highlight_entry.as_deref()),
            Some("entry-3"),
            "placing an attention chip's channel must pin its entry"
        );
        assert_eq!(app.pending, None, "placing must clear the pending state");
    }

    #[test]
    fn pending_is_cleared_when_its_channel_leaves_the_focus_items() {
        let (mut app, _) = App::new();
        app.pending = Some(PendingOpen {
            channel: "unopened".to_string(),
            target: PendingTarget::Entry("entry-4".to_string()),
        });

        let _ = app.update(Message::FocusLoaded(vec![focus_item(
            "elsewhere",
            "entry-5",
        )]));

        assert_eq!(
            app.pending, None,
            "a pending attention placement must clear once its channel drops off the board"
        );
    }
}

#[cfg(test)]
mod session_chip_tests {
    use super::*;

    fn session(id: &str, intent: &str) -> SessionDto {
        SessionDto {
            id: id.to_string(),
            state: "running".to_string(),
            intent: intent.to_string(),
        }
    }

    /// A one-pane app focused on "junto-dev", its channel already loaded
    /// with two sessions ("s1"/"alpha", "s2"/"beta") — the shape
    /// `sessions_view` needs to render session chips at all.
    fn app_with_sessions() -> App {
        let (mut app, _) = App::new();
        let focused = app.focus.expect("App::new focuses its one pane");
        let pane = app
            .panes
            .get_mut(focused)
            .expect("the pane App::new just created");
        pane.content = Content::Loaded(ChannelDto {
            id: "junto-dev".to_string(),
            name: Some("junto-dev".to_string()),
            closed: false,
            party: Vec::new(),
            workspace: None,
            sessions: vec![session("s1", "alpha"), session("s2", "beta")],
            entries: Vec::new(),
        });
        app
    }

    #[test]
    fn pressing_the_watched_sessions_chip_clears_watched_and_shows_no_placement_row() {
        let mut app = app_with_sessions();
        let focused = app.focus.expect("App::new focuses its one pane");
        app.panes
            .get_mut(focused)
            .expect("the pane just built")
            .watched = Some("s1".to_string());

        let mut ui = iced_test::simulator(sessions_view(&app));
        ui.click("alpha · running")
            .expect("the watched session's chip is a click target");
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(&messages[..], [Message::CloseSession(pane)] if *pane == focused),
            "the watched chip must publish exactly one CloseSession, got {messages:?}"
        );

        for message in messages {
            let _ = app.update(message);
        }
        assert_eq!(
            app.panes
                .get(focused)
                .and_then(|state| state.watched.clone()),
            None,
            "pressing the watched chip must clear watched"
        );

        let mut ui = iced_test::simulator(sessions_view(&app));
        assert!(
            ui.find("split right").is_err(),
            "clearing watched must not leave a placement row behind"
        );
    }

    #[test]
    fn pressing_an_unwatched_sessions_chip_shows_the_placement_row_without_changing_watched() {
        let mut app = app_with_sessions();
        let focused = app.focus.expect("App::new focuses its one pane");

        let mut ui = iced_test::simulator(sessions_view(&app));
        ui.click("beta · running")
            .expect("an unwatched session's chip is a click target");
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(
                &messages[..],
                [Message::SessionToggled(channel, session)]
                    if channel == "junto-dev" && session == "s2"
            ),
            "an unwatched chip must publish exactly one SessionToggled, got {messages:?}"
        );

        for message in messages {
            let _ = app.update(message);
        }
        assert_eq!(
            app.panes
                .get(focused)
                .and_then(|state| state.watched.clone()),
            None,
            "showing the placement row must not itself start watching"
        );

        let mut ui = iced_test::simulator(sessions_view(&app));
        ui.find("split right")
            .expect("the right placement is offered inline");
        ui.find("split below")
            .expect("the below placement is offered inline");
        ui.find("use this pane")
            .expect("the here placement is offered inline");
    }

    #[test]
    fn choosing_here_watches_the_session_in_the_focused_pane_with_no_new_pane() {
        let mut app = app_with_sessions();
        let focused = app.focus.expect("App::new focuses its one pane");
        app.pending = Some(PendingOpen {
            channel: "junto-dev".to_string(),
            target: PendingTarget::Session("s2".to_string()),
        });

        let mut ui = iced_test::simulator(sessions_view(&app));
        ui.click("use this pane")
            .expect("the inline here placement is a click target");
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let _ = app.update(message);
        }

        assert_eq!(app.panes.len(), 1, "'here' must not open a new pane");
        let state = app
            .panes
            .get(focused)
            .expect("the focused pane must still exist");
        assert_eq!(
            state.watched.as_deref(),
            Some("s2"),
            "'here' must start watching the pending session in the focused pane"
        );
        assert!(
            state.streaming,
            "'here' must actually start streaming, not just record `watched`"
        );
        assert_eq!(app.pending, None, "choosing a placement must clear pending");
    }

    #[test]
    fn choosing_right_splits_a_new_pane_on_the_same_channel_already_watching_the_session() {
        let mut app = app_with_sessions();
        let focused = app.focus.expect("App::new focuses its one pane");
        app.pending = Some(PendingOpen {
            channel: "junto-dev".to_string(),
            target: PendingTarget::Session("s2".to_string()),
        });

        let mut ui = iced_test::simulator(sessions_view(&app));
        ui.click("split right")
            .expect("the inline right placement is a click target");
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let _ = app.update(message);
        }

        assert_eq!(app.panes.len(), 2, "'right' must split off a new pane");
        let (_, new_pane) = app
            .panes
            .iter()
            .find(|(id, state)| **id != focused && state.channel == "junto-dev")
            .expect("the split must open a new pane on the same channel");
        assert_eq!(
            new_pane.watched.as_deref(),
            Some("s2"),
            "the new pane must already be watching the placed session"
        );
        assert!(
            new_pane.streaming,
            "the new pane must actually start streaming, not just record `watched`"
        );
        assert_eq!(app.pending, None, "choosing a placement must clear pending");
    }

    #[test]
    fn a_pending_session_that_leaves_the_panes_session_list_is_cleared() {
        let mut app = app_with_sessions();
        let focused = app.focus.expect("App::new focuses its one pane");
        app.pending = Some(PendingOpen {
            channel: "junto-dev".to_string(),
            target: PendingTarget::Session("s2".to_string()),
        });

        let _ = app.update(Message::Fetched(
            focused,
            Ok(ChannelDto {
                id: "junto-dev".to_string(),
                name: Some("junto-dev".to_string()),
                closed: false,
                party: Vec::new(),
                workspace: None,
                sessions: vec![session("s1", "alpha")],
                entries: Vec::new(),
            }),
        ));

        assert_eq!(
            app.pending, None,
            "a pending session must clear once it leaves the focused pane's session list"
        );
    }
}

#[cfg(test)]
mod channel_filter_tests {
    use super::*;

    #[test]
    fn a_filter_matches_a_channel_whose_name_contains_it() {
        assert!(channel_matches("junto-dev", "dev"));
    }

    #[test]
    fn a_filter_that_is_not_a_substring_does_not_match() {
        assert!(!channel_matches("junto-dev", "xyz"));
    }

    #[test]
    fn filtering_is_case_insensitive() {
        assert!(channel_matches("Junto-Dev", "DEV"));
    }

    #[test]
    fn an_empty_filter_matches_every_channel() {
        assert!(channel_matches("anything-at-all", ""));
    }

    #[test]
    fn an_empty_filter_shows_every_channel_as_chips() {
        let (mut app, _) = App::new();
        app.channel_names = vec!["junto-dev".to_string(), "other".to_string()];

        let mut ui = iced_test::simulator(channel_nav(&app));
        ui.find("junto-dev").expect("junto-dev must be listed");
        ui.find("other").expect("other must be listed");
        assert!(
            ui.find("no channels match").is_err(),
            "an empty filter must never show the quiet message"
        );
    }

    #[test]
    fn a_filter_shows_only_the_channels_that_match_it() {
        let (mut app, _) = App::new();
        app.channel_names = vec!["junto-dev".to_string(), "other".to_string()];
        app.channel_filter = "junto".to_string();

        let mut ui = iced_test::simulator(channel_nav(&app));
        ui.find("junto-dev")
            .expect("a matching channel must still be listed");
        assert!(
            ui.find("other").is_err(),
            "a non-matching channel must be filtered out"
        );
    }

    #[test]
    fn a_filter_matching_nothing_shows_the_quiet_message_instead_of_an_empty_list() {
        let (mut app, _) = App::new();
        app.channel_names = vec!["junto-dev".to_string(), "other".to_string()];
        app.channel_filter = "nope".to_string();

        let mut ui = iced_test::simulator(channel_nav(&app));
        ui.find("no channels match")
            .expect("a non-matching filter must show the quiet message");
        assert!(
            ui.find("junto-dev").is_err(),
            "a non-matching filter must hide every chip"
        );
    }

    #[test]
    fn the_create_form_is_absent_until_the_header_plus_is_pressed() {
        let (app, _) = App::new();
        assert!(!app.creating, "App::new must not start in create mode");

        let mut ui = iced_test::simulator(channel_nav(&app));
        assert!(
            ui.find("create").is_err(),
            "the create form must not render until `creating` is true"
        );
    }

    #[test]
    fn the_create_form_appears_once_creating_is_true() {
        let (mut app, _) = App::new();
        app.creating = true;

        let mut ui = iced_test::simulator(channel_nav(&app));
        ui.find("create")
            .expect("the create form must render while `creating` is true");
    }

    #[test]
    fn pressing_the_header_plus_toggles_creating() {
        let (mut app, _) = App::new();
        assert!(!app.creating);

        let _ = app.update(Message::ToggleCreating);
        assert!(app.creating, "the first press must open the create form");

        let _ = app.update(Message::ToggleCreating);
        assert!(!app.creating, "the second press must close it again");
    }
}

#[cfg(test)]
mod lineage_tests {
    use super::*;

    /// Test-local convenience: the hierarchical order alone, without the
    /// lane/rail structure `lineage_hierarchy` derives alongside it —
    /// `lineage_view` itself goes through `lineage_layout` for both at
    /// once, so this has no production caller of its own.
    fn lineage_row_order(graph: &LineageGraphDto) -> Vec<&GNode> {
        lineage_hierarchy(graph).0
    }

    /// A small hand-built graph mirroring the real data's shape: a
    /// parentless "hub" that forks three channels — one of which
    /// ("child-a") itself forks a "sub" channel AND converges back into
    /// the hub, the "both children and a converge" case — plus a
    /// converge-only channel ("child-b"), an ordinary interior channel
    /// ("child-c"), and a channel wired into nothing at all ("ghostly"),
    /// with no timestamps either.
    fn hub_graph() -> LineageGraphDto {
        let node = |id: &str, last_ms: Option<i64>| GNode {
            id: id.to_string(),
            name: id.to_string(),
            last_ms,
            milestones: Vec::new(),
        };
        let edge = |from: &str, to: &str, relation: &str| GEdge {
            from: from.to_string(),
            to: to.to_string(),
            relation: relation.to_string(),
        };
        LineageGraphDto {
            nodes: vec![
                node("hub", Some(1000)),
                node("child-a", Some(4000)),
                node("child-b", Some(3000)),
                node("child-c", Some(2000)),
                node("sub", Some(5000)),
                node("ghostly", None),
            ],
            edges: vec![
                edge("hub", "child-a", "diverge"),
                edge("hub", "child-b", "diverge"),
                edge("hub", "child-c", "diverge"),
                edge("child-a", "sub", "diverge"),
                edge("child-a", "hub", "converge"),
                edge("child-b", "hub", "converge"),
            ],
        }
    }

    #[test]
    fn rows_are_ordered_hierarchically_with_each_subtree_contiguous() {
        let graph = hub_graph();
        let order: Vec<&str> = lineage_row_order(&graph)
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        // "hub" (the root) leads; its own children follow newest-first,
        // each immediately followed by ITS descendants before the next
        // sibling — "sub" (child-a's own child) sits right after
        // "child-a", not off at the top by raw activity time (5000, the
        // newest of all). "ghostly" (an unrelated root) trails the whole
        // "hub" subtree rather than interleaving into it.
        assert_eq!(
            order,
            vec!["hub", "child-a", "sub", "child-b", "child-c", "ghostly"]
        );
    }

    #[test]
    fn a_node_with_no_last_ms_sorts_after_every_timestamped_node() {
        let graph = hub_graph();
        let order = lineage_row_order(&graph);
        assert_eq!(
            order.last().map(|n| n.id.as_str()),
            Some("ghostly"),
            "no recorded activity is no evidence of being recent, so it belongs at the bottom of a newest-first list, not the top"
        );
    }

    #[test]
    fn nodes_tied_on_last_ms_break_the_tie_by_name() {
        let mut graph = hub_graph();
        // Ties "hub" at last_ms 1000 but sorts first alphabetically.
        graph.nodes.push(GNode {
            id: "aardvark".to_string(),
            name: "aardvark".to_string(),
            last_ms: Some(1000),
            milestones: Vec::new(),
        });
        let order: Vec<&str> = lineage_row_order(&graph)
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        let hub_pos = order.iter().position(|id| *id == "hub").unwrap();
        let aardvark_pos = order.iter().position(|id| *id == "aardvark").unwrap();
        assert!(
            aardvark_pos < hub_pos,
            "a tied last_ms must break deterministically by name, not by insertion order"
        );
    }

    #[test]
    fn a_roots_relations_have_no_parent() {
        let graph = hub_graph();
        let relations = node_relations(&graph, "hub");
        assert_eq!(relations.parent, None);
        assert_eq!(relations.children, vec!["child-a", "child-b", "child-c"]);
        assert_eq!(relations.converged_into, None);
    }

    #[test]
    fn a_forking_and_converging_nodes_relations_carry_both() {
        let graph = hub_graph();
        let relations = node_relations(&graph, "child-a");
        assert_eq!(relations.parent, Some("hub"));
        assert_eq!(relations.children, vec!["sub"]);
        assert_eq!(relations.converged_into, Some("hub"));
    }

    #[test]
    fn a_leaf_nodes_relations_have_no_children_or_convergence() {
        let graph = hub_graph();
        let relations = node_relations(&graph, "sub");
        assert_eq!(relations.parent, Some("child-a"));
        assert_eq!(relations.children, Vec::<&str>::new());
        assert_eq!(relations.converged_into, None);
    }

    #[test]
    fn a_hub_with_several_diverges_is_a_fork() {
        assert_eq!(
            node_role(&node_relations(&hub_graph(), "hub")),
            LineageRole::Fork
        );
    }

    #[test]
    fn a_node_with_both_children_and_an_outgoing_converge_is_a_fork_not_converged() {
        // "child-a" forked "sub" AND converged back into "hub" — the fork
        // wins, since other channels branching off a node is the more
        // structurally significant fact about it.
        assert_eq!(
            node_role(&node_relations(&hub_graph(), "child-a")),
            LineageRole::Fork
        );
    }

    #[test]
    fn a_node_with_only_an_outgoing_converge_is_converged() {
        assert_eq!(
            node_role(&node_relations(&hub_graph(), "child-b")),
            LineageRole::Converged
        );
    }

    #[test]
    fn a_parented_non_forking_non_converging_node_is_ordinary() {
        assert_eq!(
            node_role(&node_relations(&hub_graph(), "child-c")),
            LineageRole::Ordinary
        );
    }

    #[test]
    fn an_isolated_node_with_no_parent_and_no_children_is_root() {
        assert_eq!(
            node_role(&node_relations(&hub_graph(), "ghostly")),
            LineageRole::Root
        );
    }

    /// Mirrors the real graph's shape at small scale, for the hierarchy,
    /// lane, and rail-row tests: a "spine" root forking two plain leaves
    /// and a "branchy" fork; "branchy" itself forks "nested" AND is the
    /// reconnect target of two converge-only nodes with NO diverge
    /// parent at all ("drifter-1"/"drifter-2" — the live data's
    /// `pointing-dogfood-20260823` trap); an unrelated isolated root
    /// ("lonely"); and a converge-only cycle ("loop-a"/"loop-b" converge
    /// into each other, neither has a diverge parent).
    fn rail_graph() -> LineageGraphDto {
        let node = |id: &str, last_ms: i64| GNode {
            id: id.to_string(),
            name: id.to_string(),
            last_ms: Some(last_ms),
            milestones: Vec::new(),
        };
        let edge = |from: &str, to: &str, relation: &str| GEdge {
            from: from.to_string(),
            to: to.to_string(),
            relation: relation.to_string(),
        };
        LineageGraphDto {
            nodes: vec![
                node("spine", 100),
                node("twig-a", 500),
                node("twig-b", 400),
                node("branchy", 300),
                node("nested", 600),
                node("drifter-1", 250),
                node("drifter-2", 260),
                node("lonely", 50),
                node("loop-a", 10),
                node("loop-b", 20),
            ],
            edges: vec![
                edge("spine", "twig-a", "diverge"),
                edge("spine", "twig-b", "diverge"),
                edge("spine", "branchy", "diverge"),
                edge("branchy", "nested", "diverge"),
                edge("drifter-1", "branchy", "converge"),
                edge("drifter-2", "branchy", "converge"),
                edge("loop-a", "loop-b", "converge"),
                edge("loop-b", "loop-a", "converge"),
            ],
        }
    }

    /// `n0 -> n1 -> ... -> n{depth}`, one diverge each — for exercising
    /// `MAX_LANE`'s clamp against a chain deeper than the cap.
    fn chain_graph(depth: usize) -> LineageGraphDto {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for i in 0..=depth {
            nodes.push(GNode {
                id: format!("n{i}"),
                name: format!("n{i}"),
                last_ms: Some(i as i64),
                milestones: Vec::new(),
            });
            if i > 0 {
                edges.push(GEdge {
                    from: format!("n{}", i - 1),
                    to: format!("n{i}"),
                    relation: "diverge".to_string(),
                });
            }
        }
        LineageGraphDto { nodes, edges }
    }

    fn rail_order(graph: &LineageGraphDto) -> Vec<&str> {
        lineage_row_order(graph)
            .iter()
            .map(|n| n.id.as_str())
            .collect()
    }

    #[test]
    fn a_forking_childs_own_child_is_contiguous_with_its_parent() {
        let graph = rail_graph();
        let order = rail_order(&graph);
        let branchy_pos = order.iter().position(|id| *id == "branchy").unwrap();
        let nested_pos = order.iter().position(|id| *id == "nested").unwrap();
        assert_eq!(
            nested_pos,
            branchy_pos + 1,
            "a fork's own child must be the very next row, not scattered by activity time (nested is the newest node in the whole graph)"
        );
    }

    #[test]
    fn converge_only_nodes_with_no_diverge_parent_sit_with_their_reconnect_target() {
        let graph = rail_graph();
        let order = rail_order(&graph);
        let branchy_pos = order.iter().position(|id| *id == "branchy").unwrap();
        let lonely_pos = order.iter().position(|id| *id == "lonely").unwrap();
        let drifter1_pos = order.iter().position(|id| *id == "drifter-1").unwrap();
        let drifter2_pos = order.iter().position(|id| *id == "drifter-2").unwrap();
        // Neither drifter has a diverge edge at all — only `lineage_owner`'s
        // converge-target fallback keeps them inside "branchy"'s own
        // contiguous block instead of stranded among unrelated roots.
        assert!(
            branchy_pos < drifter1_pos && drifter1_pos < lonely_pos,
            "a converge-only node must sit inside its reconnect target's subtree, not after it: {order:?}"
        );
        assert!(
            branchy_pos < drifter2_pos && drifter2_pos < lonely_pos,
            "a converge-only node must sit inside its reconnect target's subtree, not after it: {order:?}"
        );
    }

    #[test]
    fn a_mutual_converge_cycle_terminates_and_visits_each_node_exactly_once() {
        // Neither "loop-a" nor "loop-b" has a diverge parent, and each
        // converges into the other — `lineage_owner` makes each the
        // other's owner, a cycle no real root ever reaches. Completing at
        // all (not hanging) is half the proof; visiting each exactly once
        // is the other half.
        let graph = rail_graph();
        let order = rail_order(&graph);
        assert_eq!(order.iter().filter(|id| **id == "loop-a").count(), 1);
        assert_eq!(order.iter().filter(|id| **id == "loop-b").count(), 1);
        assert_eq!(order.len(), graph.nodes.len());
    }

    fn lanes_of(graph: &LineageGraphDto) -> HashMap<String, usize> {
        let (order, _ends, parents) = lineage_hierarchy(graph);
        lineage_lanes(&order, &parents)
            .into_iter()
            .map(|(id, lane)| (id.to_string(), lane))
            .collect()
    }

    #[test]
    fn a_root_is_lane_zero() {
        let lanes = lanes_of(&rail_graph());
        assert_eq!(lanes["spine"], 0);
        assert_eq!(lanes["lonely"], 0);
    }

    #[test]
    fn a_childs_lane_is_its_parents_lane_plus_one() {
        let lanes = lanes_of(&rail_graph());
        assert_eq!(lanes["twig-a"], 1);
        assert_eq!(lanes["branchy"], 1);
    }

    #[test]
    fn a_grandchilds_lane_is_two() {
        let lanes = lanes_of(&rail_graph());
        assert_eq!(lanes["nested"], 2);
    }

    #[test]
    fn a_converge_only_trap_node_shares_its_owners_childrens_lane() {
        // "drifter-1"/"drifter-2" have no diverge parent, but
        // `lineage_owner`'s fallback still nests them under "branchy" —
        // the same lane as "nested", "branchy"'s real diverge child.
        let lanes = lanes_of(&rail_graph());
        assert_eq!(lanes["drifter-1"], 2);
        assert_eq!(lanes["drifter-2"], 2);
    }

    #[test]
    fn a_chain_deeper_than_max_lane_clamps_instead_of_growing() {
        let graph = chain_graph(6);
        let lanes = lanes_of(&graph);
        assert_eq!(lanes["n0"], 0);
        assert_eq!(lanes["n1"], 1);
        assert_eq!(lanes["n2"], 2);
        assert_eq!(lanes["n3"], MAX_LANE - 1);
        assert_eq!(
            lanes["n6"],
            MAX_LANE - 1,
            "a node past the cap must clamp to the last lane, not grow the rail"
        );
    }

    fn rails_of(graph: &LineageGraphDto) -> HashMap<String, LineageRailRow> {
        let layout = lineage_layout(graph);
        layout
            .rails
            .into_iter()
            .map(|(id, row)| (id.to_string(), row))
            .collect()
    }

    #[test]
    fn a_roots_row_has_no_branch_in_and_no_through_lines() {
        let rails = rails_of(&rail_graph());
        let spine = rails["spine"];
        assert_eq!(spine.branch_from, None);
        assert_eq!(spine.through, LaneSet::default());
        assert_eq!(spine.converge_to, None);
        assert_eq!(spine.role, LineageRole::Fork);
    }

    #[test]
    fn a_childs_row_peels_from_its_parents_lane() {
        let rails = rails_of(&rail_graph());
        assert_eq!(rails["twig-a"].branch_from, Some(0));
    }

    #[test]
    fn a_row_carries_a_through_line_while_its_ancestors_subtree_still_has_rows_left() {
        let rails = rails_of(&rail_graph());
        // "twig-a" is "spine"'s first child; "spine" still owns "twig-b",
        // "branchy", "nested", and both drifters below it.
        assert!(rails["twig-a"].through.contains(0));
    }

    #[test]
    fn the_last_row_of_a_finished_subtree_carries_no_through_line_for_it() {
        let rails = rails_of(&rail_graph());
        // "drifter-1" is the very last row inside BOTH "branchy"'s and
        // "spine"'s subtrees — nothing of either continues below it.
        let drifter1 = rails["drifter-1"];
        assert!(!drifter1.through.contains(0));
        assert!(!drifter1.through.contains(1));
    }

    #[test]
    fn a_converge_only_trap_node_peels_from_and_reconnects_to_the_same_lane() {
        // No real diverge, so the "branch" and the "converge" both point
        // at "branchy"'s lane — a peel-out-and-back-in shape, not a
        // misleading fork.
        let rails = rails_of(&rail_graph());
        let drifter2 = rails["drifter-2"];
        assert_eq!(drifter2.branch_from, drifter2.converge_to);
        assert_eq!(drifter2.branch_from, Some(1));
        assert_eq!(drifter2.role, LineageRole::Converged);
    }

    #[test]
    fn an_isolated_root_has_no_rail_connectors_at_all() {
        let rails = rails_of(&rail_graph());
        let lonely = rails["lonely"];
        assert_eq!(lonely.branch_from, None);
        assert_eq!(lonely.converge_to, None);
        assert_eq!(lonely.through, LaneSet::default());
        assert_eq!(lonely.role, LineageRole::Root);
    }

    #[test]
    fn the_cycle_broken_side_has_no_branch_in_but_still_shows_its_converge() {
        // Whichever of "loop-a"/"loop-b" the walk reaches first (here,
        // "loop-b", newer) is never actually nested under the other —
        // `lineage_visit`'s own doc comment covers why — so it has no
        // `branch_from`, even though it still has a real outgoing
        // converge to draw.
        let rails = rails_of(&rail_graph());
        let loop_b = rails["loop-b"];
        assert_eq!(loop_b.branch_from, None);
        assert!(loop_b.converge_to.is_some());
    }

    #[test]
    fn the_other_cycle_node_peels_from_the_lane_the_walk_assigned_it() {
        let rails = rails_of(&rail_graph());
        let lanes = lanes_of(&rail_graph());
        assert_eq!(rails["loop-a"].branch_from, Some(lanes["loop-b"]));
    }
}

#[cfg(test)]
mod lineage_view_tests {
    use super::*;

    fn one_node_graph(name: &str) -> LineageGraphDto {
        LineageGraphDto {
            nodes: vec![GNode {
                id: "n1".to_string(),
                name: name.to_string(),
                last_ms: Some(1),
                milestones: Vec::new(),
            }],
            edges: Vec::new(),
        }
    }

    fn app_with_lineage(graph: LineageGraphDto) -> App {
        let (mut app, _) = App::new();
        app.shell.right_view = shell::RightView::Lineage;
        app.lineage = Some(graph);
        app
    }

    fn app_with_lineage_view(name: &str) -> App {
        app_with_lineage(one_node_graph(name))
    }

    #[test]
    fn no_lineage_shows_the_quiet_fallback_message() {
        let (app, _) = App::new();
        let mut ui = iced_test::simulator(lineage_view(&app));
        ui.find("no lineage yet")
            .expect("with no lineage fetched yet, the quiet message must show");
    }

    #[test]
    fn the_header_shows_the_node_count() {
        let app = app_with_lineage(LineageGraphDto {
            nodes: vec![
                GNode {
                    id: "a".into(),
                    name: "a".into(),
                    last_ms: None,
                    milestones: Vec::new(),
                },
                GNode {
                    id: "b".into(),
                    name: "b".into(),
                    last_ms: None,
                    milestones: Vec::new(),
                },
            ],
            edges: Vec::new(),
        });
        let mut ui = iced_test::simulator(lineage_view(&app));
        ui.find("2").expect("the header must show the node count");
    }

    #[test]
    fn collapsing_the_section_hides_the_row_list_but_keeps_the_header() {
        let mut app = app_with_lineage_view("junto-dev");
        app.lineage_collapsed = true;
        let mut ui = iced_test::simulator(lineage_view(&app));
        ui.find("lineage")
            .expect("the header must stay visible while collapsed");
        assert!(
            ui.find("junto-dev").is_err(),
            "a collapsed section must not render its row list"
        );
    }

    #[test]
    fn pressing_the_header_chevron_toggles_lineage_collapsed() {
        let (mut app, _) = App::new();
        assert!(!app.lineage_collapsed);
        let _ = app.update(Message::ToggleLineageCollapsed);
        assert!(
            app.lineage_collapsed,
            "the first press must collapse the section"
        );
        let _ = app.update(Message::ToggleLineageCollapsed);
        assert!(
            !app.lineage_collapsed,
            "the second press must expand it again"
        );
    }

    #[test]
    fn pressing_a_rows_disclosure_expands_and_collapses_its_detail() {
        let (mut app, _) = App::new();
        let _ = app.update(Message::LineageRowToggled("n1".to_string()));
        assert!(app.lineage_expanded.contains("n1"));
        let _ = app.update(Message::LineageRowToggled("n1".to_string()));
        assert!(!app.lineage_expanded.contains("n1"));
    }

    #[test]
    fn expanding_a_row_shows_its_relations_and_milestones() {
        let mut app = app_with_lineage(LineageGraphDto {
            nodes: vec![
                GNode {
                    id: "hub".into(),
                    name: "hub".into(),
                    last_ms: Some(1),
                    milestones: Vec::new(),
                },
                GNode {
                    id: "child".into(),
                    name: "child".into(),
                    last_ms: Some(2),
                    milestones: vec![MilestoneDto {
                        ms: 1,
                        label: "kickoff".to_string(),
                    }],
                },
            ],
            edges: vec![GEdge {
                from: "hub".into(),
                to: "child".into(),
                relation: "diverge".into(),
            }],
        });
        app.lineage_expanded.insert("child".to_string());
        let mut ui = iced_test::simulator(lineage_view(&app));
        ui.find("diverged from hub")
            .expect("an expanded child row must name its parent");
        ui.find("kickoff")
            .expect("an expanded row must list its milestones");
    }

    #[test]
    fn the_focused_channels_row_still_renders_its_name_now_that_the_rail_is_drawn() {
        // `App::new`'s own pane is "junto-dev" and focused. The rail's
        // focus/role distinction is now drawn geometry
        // (`LineageRailCanvas`), not a findable glyph — `lineage_tests`
        // covers that geometry directly; this just proves the row (rail
        // cell included) still renders without panicking.
        let app = app_with_lineage_view("junto-dev");
        let mut ui = iced_test::simulator(lineage_view(&app));
        ui.find("junto-dev")
            .expect("the focused channel's row must still render its name");
    }

    #[test]
    fn a_forking_nodes_row_still_renders_both_names_now_that_the_rail_is_drawn() {
        let app = app_with_lineage(LineageGraphDto {
            nodes: vec![
                GNode {
                    id: "hub".into(),
                    name: "hub".into(),
                    last_ms: Some(2),
                    milestones: Vec::new(),
                },
                GNode {
                    id: "child".into(),
                    name: "child".into(),
                    last_ms: Some(1),
                    milestones: Vec::new(),
                },
            ],
            edges: vec![GEdge {
                from: "hub".into(),
                to: "child".into(),
                relation: "diverge".into(),
            }],
        });
        let mut ui = iced_test::simulator(lineage_view(&app));
        ui.find("hub")
            .expect("the fork row must still render its name");
        ui.find("child")
            .expect("the forked child row must still render its name");
    }

    #[test]
    fn picking_an_open_channels_row_focuses_it_without_closing_its_pane() {
        let (mut app, _) = App::new();
        let first = app.focus.expect("App::new focuses its one pane");
        let (other_pane, _split) = app
            .panes
            .split(pane_grid::Axis::Vertical, first, Pane::loading("other"))
            .expect("splitting the only pane must succeed");
        // `split` leaves focus on the original pane, not the new one.

        let _ = app.update(Message::LineageChannelPicked("other".to_string()));

        assert_eq!(
            app.panes.len(),
            2,
            "picking an open channel's row must not close its pane"
        );
        assert_eq!(
            app.focus,
            Some(other_pane),
            "picking an open channel's row must focus its pane"
        );
    }

    #[test]
    fn picking_an_unopened_channels_row_offers_placement_like_a_channel_chip() {
        let (mut app, _) = App::new();
        let _ = app.update(Message::LineageChannelPicked("unopened".to_string()));
        assert_eq!(
            app.pending,
            Some(PendingOpen {
                channel: "unopened".to_string(),
                target: PendingTarget::Channel,
            })
        );
    }

    #[test]
    fn a_lineage_row_stays_single_line_at_the_narrowest_reported_blade_width() {
        // 211px: the exact right-blade width that left the old horizontal
        // `LineageCanvas` almost nothing to draw in and rendered it blank —
        // the width this vertical list must survive without a row's name
        // wrapping to a second line and inflating the row's height.
        for name in [
            "junto-dev",
            "a-very-long-channel-name-that-would-overflow-a-narrow-blade",
        ] {
            let app = app_with_lineage_view(name);
            let mut ui = iced_test::Simulator::with_size(
                iced_test::core::Settings::default(),
                Size::new(211.0, 600.0),
                right_blade(&app),
            );
            let expected = truncate(name, 28);
            let target = ui
                .find(expected.as_str())
                .unwrap_or_else(|_| panic!("the row for {name:?} must render a findable name"));
            let bounds = target.bounds();
            assert!(
                bounds.height < 20.0,
                "the row for {name:?} must render on one line at a 211px blade width, not wrap to two; got {bounds:?}"
            );
        }
    }
}
