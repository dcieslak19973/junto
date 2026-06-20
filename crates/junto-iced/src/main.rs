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
    button, column, container, row, scrollable, text, text_input, Space,
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
    new_channel: String,
}

struct Pane {
    channel: String,
    content: Content,
}

enum Content {
    Loading,
    Loaded(ChannelDto),
    Error(String),
}

// --- the host's view.json shape ---

#[derive(Debug, Clone, Deserialize)]
struct ChannelDto {
    #[allow(dead_code)]
    id: String,
    name: Option<String>,
    closed: bool,
    party: Vec<String>,
    lineage: Vec<LineageDto>,
    entries: Vec<EntryDto>,
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
    NewChannelChanged(String),
    AddChannel,
    Fetched(pane_grid::Pane, Result<ChannelDto, String>),
    Clicked(pane_grid::Pane),
    Dragged(pane_grid::DragEvent),
    Resized(pane_grid::ResizeEvent),
    Refresh(pane_grid::Pane),
    Close(pane_grid::Pane),
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let (panes, first) = pane_grid::State::new(Pane::loading("junto-dev"));
        let app = App {
            panes,
            focus: Some(first),
            new_channel: String::new(),
        };
        (app, fetch(first, "junto-dev"))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::NewChannelChanged(value) => {
                self.new_channel = value;
                Task::none()
            }
            Message::AddChannel => {
                let name = self.new_channel.trim().to_string();
                if name.is_empty() {
                    return Task::none();
                }
                let Some(target) = self.focus.or_else(|| self.panes.iter().next().map(|(p, _)| *p))
                else {
                    return Task::none();
                };
                if let Some((new_pane, _)) =
                    self.panes
                        .split(pane_grid::Axis::Vertical, target, Pane::loading(&name))
                {
                    self.focus = Some(new_pane);
                    self.new_channel.clear();
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
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let adder = row![
            text_input("channel name…", &self.new_channel)
                .on_input(Message::NewChannelChanged)
                .on_submit(Message::AddChannel)
                .padding(8),
            button("+ pane").on_press(Message::AddChannel).padding(8),
        ]
        .spacing(8);

        let grid = PaneGrid::new(&self.panes, |id, pane, _is_maximized| {
            let title = row![
                text(pane.channel.clone()).size(15),
                Space::with_width(Fill),
                button("↻").on_press(Message::Refresh(id)).padding(4),
                button("×").on_press(Message::Close(id)).padding(4),
            ]
            .spacing(6);

            pane_grid::Content::new(pane_body(&pane.content))
                .title_bar(pane_grid::TitleBar::new(title).padding(8))
        })
        .spacing(6)
        .on_click(Message::Clicked)
        .on_drag(Message::Dragged)
        .on_resize(8, Message::Resized);

        column![adder, grid].spacing(10).padding(10).into()
    }
}

fn pane_body(content: &Content) -> Element<'_, Message> {
    match content {
        Content::Loading => container(text("loading…").color(MUTED)).padding(12).into(),
        Content::Error(err) => container(text(format!("error: {err}")).color(RED))
            .padding(12)
            .into(),
        Content::Loaded(dto) => {
            let channel = dto.name.clone().unwrap_or_else(|| "(unopened)".into());

            // Pinned at top: the lineage strip (the split / side-quest history)
            // and the party — they don't scroll with the timeline below.
            let mut header = column![lineage_strip(&channel, dto)].spacing(8);
            if !dto.party.is_empty() {
                header = header.push(
                    text(format!("party: {}", dto.party.join(", ")))
                        .size(12)
                        .color(MUTED),
                );
            }

            // The entry timeline scrolls under it — git-log style: each entry is
            // a node on a left rail, newest last (canonical order).
            let mut timeline = column![].spacing(8);
            for entry in &dto.entries {
                timeline = timeline.push(timeline_entry(entry));
            }

            column![header, scrollable(timeline).height(Fill)]
                .spacing(10)
                .padding(12)
                .into()
        }
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
