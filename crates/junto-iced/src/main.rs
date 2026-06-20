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
    button, column, combo_box, container, row, scrollable, text, text_input, Space,
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
}

struct Pane {
    channel: String,
    content: Content,
    /// The session whose live feed this pane is streaming, if any.
    watched: Option<String>,
    /// Accumulated live events for the watched session.
    feed: Vec<LiveEvent>,
    launch_intent: String,
    steer_text: String,
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

/// One frame of a session's live feed (mirrors the host's `LiveEvent`).
#[derive(Debug, Clone, Deserialize)]
struct LiveEvent {
    kind: String,
    text: String,
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    html: bool,
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
    author: String,
    kind: String,
    summary: String,
    status: Option<String>,
    unrecognized: bool,
}

#[derive(Debug, Clone)]
enum Message {
    ChannelsLoaded(Vec<String>),
    ChannelPicked(String),
    LineageGraphLoaded(Option<LineageGraphDto>),
    FocusLoaded(Vec<FocusItem>),
    Fetched(pane_grid::Pane, Result<ChannelDto, String>),
    Refresh(pane_grid::Pane),
    Close(pane_grid::Pane),
    // Live session pane.
    Watch(pane_grid::Pane, String),
    Live(String, LiveEvent),
    LiveEnded(String),
    LaunchIntentChanged(pane_grid::Pane, String),
    Launch(pane_grid::Pane),
    SteerTextChanged(pane_grid::Pane, String),
    Steer(pane_grid::Pane),
    Interrupt(pane_grid::Pane),
    Posted(pane_grid::Pane),
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
        };
        (
            app,
            Task::batch([
                fetch(first, "junto-dev"),
                fetch_channels(),
                fetch_lineage_graph(),
                fetch_focus(),
            ]),
        )
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
            Message::ChannelPicked(name) => {
                let Some(target) = self.focus.or_else(|| self.order.last().copied())
                else {
                    return Task::none();
                };
                if let Some((new_pane, _)) =
                    self.panes
                        .split(pane_grid::Axis::Vertical, target, Pane::loading(&name))
                {
                    self.order.push(new_pane);
                    self.focus = Some(new_pane);
                    return fetch(new_pane, &name);
                }
                Task::none()
            }
            Message::Fetched(pane, result) => {
                if let Some(state) = self.panes.get_mut(pane) {
                    state.content = match result {
                        Ok(dto) => Content::Loaded(dto),
                        Err(err) => Content::Error(err),
                    };
                }
                Task::none()
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
                    state.feed.clear();
                }
                Task::none()
            }
            Message::Live(session, event) => {
                for (_, state) in self.panes.iter_mut() {
                    if state.watched.as_deref() == Some(session.as_str()) {
                        // Coalesce streaming Markdown segments by seq.
                        match state.feed.last_mut() {
                            Some(last) if event.seq != 0 && last.seq == event.seq => *last = event,
                            _ => state.feed.push(event),
                        }
                        break;
                    }
                }
                Task::none()
            }
            Message::LiveEnded(session) => {
                let mut to_refresh = None;
                for (pane, state) in self.panes.iter_mut() {
                    if state.watched.as_deref() == Some(session.as_str()) {
                        state.watched = None; // ends the subscription; feed stays
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
            Message::Launch(pane) => {
                let Some(state) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                let intent = state.launch_intent.trim().to_string();
                if intent.is_empty() {
                    return Task::none();
                }
                let channel = state.channel.clone();
                state.launch_intent.clear();
                post_launch(pane, channel, intent)
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
                post_act(pane, channel, session, "steer", Some(text))
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
        }
    }

    fn subscription(&self) -> iced::Subscription<Message> {
        // One SSE subscription per pane that is watching a session.
        let streams: Vec<_> = self
            .panes
            .iter()
            .filter_map(|(_, state)| {
                state.watched.as_ref().map(|session| {
                    iced::Subscription::run_with_id(
                        session.clone(),
                        session_stream(state.channel.clone(), session.clone()),
                    )
                })
            })
            .collect();
        iced::Subscription::batch(streams)
    }

    fn view(&self) -> Element<'_, Message> {
        let adder = row![
            text("open a channel ▸").size(13).color(MUTED),
            combo_box(
                &self.channels,
                "type to search channels…",
                None,
                Message::ChannelPicked,
            )
            .width(320),
        ]
        .spacing(8)
        .align_y(Center);

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
                    chip = chip.on_press(Message::ChannelPicked(name.clone()));
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
                body = body.push(column_pane(*id, pane));
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
fn column_pane(id: pane_grid::Pane, pane: &Pane) -> Element<'_, Message> {
    container(column![title_row(id, pane), pane_body(id, pane)].spacing(8))
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

fn pane_body(id: pane_grid::Pane, pane: &Pane) -> Element<'_, Message> {
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

    // The channel's own header: just the party now (lineage lives in the top
    // window-wide branch graph, so the per-pane strip is gone).
    let mut header = column![].spacing(8);
    if !dto.party.is_empty() {
        header = header.push(
            text(format!("party: {}", dto.party.join(", ")))
                .size(12)
                .color(MUTED),
        );
    }

    // Launch a session.
    let launch = row![
        text_input("launch a session — what should it do?", &pane.launch_intent)
            .on_input(move |v| Message::LaunchIntentChanged(id, v))
            .on_submit(Message::Launch(id))
            .padding(6),
        button("launch").on_press(Message::Launch(id)).padding(6),
    ]
    .spacing(6);

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

    // Main area: the live feed (if watching) or the entry timeline.
    let main: Element<Message> = if pane.watched.is_some() {
        let mut feed = column![].spacing(4);
        for event in &pane.feed {
            feed = feed.push(feed_line(event));
        }
        let steer = row![
            text_input("steer the running turn…", &pane.steer_text)
                .on_input(move |v| Message::SteerTextChanged(id, v))
                .on_submit(Message::Steer(id))
                .padding(6),
            button("steer").on_press(Message::Steer(id)).padding(6),
            button("interrupt").on_press(Message::Interrupt(id)).padding(6),
        ]
        .spacing(6);
        column![scrollable(feed).height(Fill), steer].spacing(8).into()
    } else {
        let mut timeline = column![].spacing(8);
        for entry in &dto.entries {
            timeline = timeline.push(timeline_entry(entry));
        }
        scrollable(timeline).height(Fill).into()
    };

    column![header, launch, chips, main]
        .spacing(10)
        .padding(12)
        .into()
}

/// One live-feed line — kind-coloured, with any HTML the host rendered stripped
/// back to plain text (native can't paint HTML).
fn feed_line(event: &LiveEvent) -> Element<'static, Message> {
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
fn timeline_entry(entry: &EntryDto) -> Element<'_, Message> {
    row![rail(kind_color(&entry.kind)), entry_card(entry)]
        .spacing(10)
        .into()
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
fn entry_card(entry: &EntryDto) -> Element<'_, Message> {
    let accent = kind_color(&entry.kind);
    let mut head = row![badge(&entry.kind, accent), text(entry.author.clone()).size(11).color(MUTED)]
        .spacing(8);
    if let Some(status) = &entry.status {
        head = head.push(badge(status, status_color(status)));
    }
    if entry.unrecognized {
        head = head.push(badge("unrecognized", MUTED));
    }

    let card = column![head, text(entry.summary.clone()).size(13).color(TEXT)].spacing(6);

    container(card)
        .padding(10)
        .width(Fill)
        .style(move |_theme| container::Style {
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

/// A small filled pill.
fn badge(label: &str, color: Color) -> Element<'_, Message> {
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
            feed: Vec::new(),
            launch_intent: String::new(),
            steer_text: String::new(),
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

/// POST a launch (a new session) for `channel` with `intent`.
fn post_launch(pane: pane_grid::Pane, channel: String, intent: String) -> Task<Message> {
    let url = format!("{HOST}/channels/{channel}/sessions");
    Task::perform(
        async move {
            let _ = reqwest::Client::new()
                .post(&url)
                .form(&[("intent", intent.as_str()), ("mode", "single")])
                .send()
                .await;
        },
        move |()| Message::Posted(pane),
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
