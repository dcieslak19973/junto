//! SPIKE — a native junto surface in Iced (`docs/native-ui-toolkit-assessment.md`).
//!
//! A tmux-style **vertical-split pane workspace**: each pane is a junto channel,
//! rendered from the host's structured JSON read-API (`/channels/{name}/view.json`)
//! into native widgets — a **lineage strip** (the split/side-quest history), the
//! party, and the **entry timeline** as colour-coded cards. Type a channel name
//! and `+ pane` to split a column; drag dividers to resize. The point is to feel
//! whether native (Iced) beats the webview as the desktop power-surface.

use iced::widget::pane_grid::{self, PaneGrid};
use iced::widget::{
    button, column, combo_box, container, row, scrollable, text, text_input, Space,
};
use iced::{Background, Border, Center, Color, Element, Fill, Length, Task, Theme};
use serde::Deserialize;

const HOST: &str = "http://127.0.0.1:1727";

// A catppuccin-ish dark palette, close to the web surface.
const SURFACE: Color = Color::from_rgb(0.19, 0.20, 0.27);
const TEXT: Color = Color::from_rgb(0.80, 0.84, 0.96);
const MUTED: Color = Color::from_rgb(0.49, 0.51, 0.63);
const TEAL: Color = Color::from_rgb(0.58, 0.89, 0.84);
const GREEN: Color = Color::from_rgb(0.65, 0.89, 0.63);
const RED: Color = Color::from_rgb(0.95, 0.55, 0.66);
const YELLOW: Color = Color::from_rgb(0.98, 0.89, 0.69);
const MAUVE: Color = Color::from_rgb(0.80, 0.65, 0.97);
const BLUE: Color = Color::from_rgb(0.54, 0.71, 0.98);

fn main() -> iced::Result {
    let icon = iced::window::icon::from_file_data(include_bytes!("../icon.png"), None).ok();
    iced::application("junto — native spike", App::update, App::view)
        .subscription(App::subscription)
        .theme(|_| Theme::CatppuccinMocha)
        .window(iced::window::Settings {
            icon,
            ..Default::default()
        })
        .run_with(App::new)
}

struct App {
    panes: pane_grid::State<Pane>,
    focus: Option<pane_grid::Pane>,
    /// Available channel names for the type-ahead picker.
    channels: combo_box::State<String>,
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
    name: Option<String>,
    #[allow(dead_code)]
    closed: bool,
    party: Vec<String>,
    lineage: Vec<LineageDto>,
    sessions: Vec<SessionDto>,
    entries: Vec<EntryDto>,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionDto {
    id: String,
    state: String,
    intent: String,
}

#[derive(Debug, Clone, Deserialize)]
struct LineageDto {
    #[allow(dead_code)]
    relation: String,
    direction: String,
    #[allow(dead_code)]
    other: String,
    other_name: Option<String>,
    #[allow(dead_code)]
    label: String,
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
    Fetched(pane_grid::Pane, Result<ChannelDto, String>),
    Clicked(pane_grid::Pane),
    Dragged(pane_grid::DragEvent),
    Resized(pane_grid::ResizeEvent),
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
            channels: combo_box::State::new(Vec::new()),
        };
        (app, Task::batch([fetch(first, "junto-dev"), fetch_channels()]))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ChannelsLoaded(names) => {
                self.channels = combo_box::State::new(names);
                Task::none()
            }
            Message::ChannelPicked(name) => {
                let Some(target) = self.focus.or_else(|| self.panes.iter().next().map(|(p, _)| *p))
                else {
                    return Task::none();
                };
                if let Some((new_pane, _)) =
                    self.panes
                        .split(pane_grid::Axis::Vertical, target, Pane::loading(&name))
                {
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
            Message::Clicked(pane) => {
                self.focus = Some(pane);
                Task::none()
            }
            Message::Dragged(pane_grid::DragEvent::Dropped { pane, target }) => {
                self.panes.drop(pane, target);
                Task::none()
            }
            Message::Dragged(_) => Task::none(),
            Message::Resized(pane_grid::ResizeEvent { split, ratio }) => {
                self.panes.resize(split, ratio);
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
            .width(360),
        ]
        .spacing(8)
        .align_y(Center);

        let grid = PaneGrid::new(&self.panes, |id, pane, _is_maximized| {
            let title = row![
                text(pane.channel.clone()).size(15),
                Space::with_width(Fill),
                button("↻").on_press(Message::Refresh(id)).padding(4),
                button("×").on_press(Message::Close(id)).padding(4),
            ]
            .spacing(6);

            pane_grid::Content::new(pane_body(id, pane))
                .title_bar(pane_grid::TitleBar::new(title).padding(8))
        })
        .spacing(6)
        .on_click(Message::Clicked)
        .on_drag(Message::Dragged)
        .on_resize(8, Message::Resized);

        column![adder, grid].spacing(10).padding(10).into()
    }
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
    let channel = dto.name.clone().unwrap_or_else(|| "(unopened)".into());

    // Pinned at top: the lineage strip + party.
    let mut header = column![lineage_strip(&channel, dto)].spacing(8);
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
                color: Color { a: 0.5, ..accent },
                width: 1.0,
                radius: 6.0.into(),
            },
            text_color: Some(TEXT),
            ..container::Style::default()
        })
        .into()
}

/// The lineage **timeline strip** pinned at the top of a channel pane: parents/
/// predecessors on the left, this channel highlighted in the middle, side-quests/
/// continuations on the right — the split history at a glance.
fn lineage_strip(channel: &str, dto: &ChannelDto) -> Element<'static, Message> {
    let mut strip = row![].spacing(8).align_y(Center);
    for edge in dto.lineage.iter().filter(|e| e.direction == "incoming") {
        let label = edge.other_name.clone().unwrap_or_else(|| "parent".into());
        strip = strip.push(node(label, MUTED, false));
        strip = strip.push(text("→").size(16).color(MUTED));
    }
    strip = strip.push(node(channel.to_string(), TEAL, true));
    for edge in dto.lineage.iter().filter(|e| e.direction == "outgoing") {
        let label = edge.other_name.clone().unwrap_or_else(|| "side-quest".into());
        strip = strip.push(text("→").size(16).color(MUTED));
        strip = strip.push(node(label, MAUVE, false));
    }
    container(
        scrollable(strip)
            .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::default())),
    )
    .padding([8, 12])
    .width(Fill)
    .center_y(Length::Fixed(64.0))
    .style(|_theme| container::Style {
        background: Some(Background::Color(Color { a: 0.5, ..SURFACE })),
        border: Border {
            radius: 6.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    })
    .into()
}

/// One node in the lineage strip. Long channel names are truncated to keep the
/// strip glanceable.
fn node(label: String, color: Color, highlight: bool) -> Element<'static, Message> {
    let text_color = if highlight {
        Color::from_rgb(0.12, 0.12, 0.18)
    } else {
        TEXT
    };
    let background = if highlight {
        color
    } else {
        Color { a: 0.18, ..color }
    };
    let label = if label.chars().count() > 30 {
        format!("{}…", label.chars().take(29).collect::<String>())
    } else {
        label
    };
    container(text(label).size(13).color(text_color))
        .padding([6, 12])
        .style(move |_theme| container::Style {
            background: Some(Background::Color(background)),
            border: Border {
                color,
                width: 1.0,
                radius: 6.0.into(),
            },
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
