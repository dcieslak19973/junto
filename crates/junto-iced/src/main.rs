//! SPIKE — a native junto surface in Iced (`docs/native-ui-toolkit-assessment.md`).
//!
//! A tmux-style **vertical-split pane workspace**: each pane is a junto channel,
//! its `/brief` fetched live from the running host and rendered as Markdown.
//! Type a channel name and `+ pane` to split a new column; drag the dividers to
//! resize; markdown links open in the OS browser. The point is to *feel* whether
//! native (Iced) is worth a second surface alongside the served web pages.

use iced::widget::pane_grid::{self, PaneGrid};
use iced::widget::{
    button, column, container, markdown, row, scrollable, text, text_input, Space,
};
use iced::{Element, Fill, Task, Theme};

const HOST: &str = "http://127.0.0.1:1727";

fn main() -> iced::Result {
    iced::application("junto — native spike", App::update, App::view)
        .theme(|_| Theme::Dark)
        .run_with(App::new)
}

struct App {
    panes: pane_grid::State<Pane>,
    focus: Option<pane_grid::Pane>,
    new_channel: String,
}

/// One channel column.
struct Pane {
    channel: String,
    content: Content,
}

enum Content {
    Loading,
    Brief(Vec<markdown::Item>),
    Error(String),
}

#[derive(Debug, Clone)]
enum Message {
    NewChannelChanged(String),
    AddChannel,
    Fetched(pane_grid::Pane, Result<String, String>),
    Clicked(pane_grid::Pane),
    Dragged(pane_grid::DragEvent),
    Resized(pane_grid::ResizeEvent),
    Refresh(pane_grid::Pane),
    Close(pane_grid::Pane),
    LinkClicked(markdown::Url),
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
                // Vertical axis ⇒ a vertical divider ⇒ side-by-side columns.
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
                        Ok(body) => Content::Brief(markdown::parse(&body).collect()),
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
            Message::LinkClicked(url) => {
                let _ = open::that(url.to_string());
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
                text(pane.channel.clone()),
                Space::with_width(Fill),
                button("↻").on_press(Message::Refresh(id)).padding(4),
                button("×").on_press(Message::Close(id)).padding(4),
            ]
            .spacing(6);

            pane_grid::Content::new(pane_body(&pane.content))
                .title_bar(pane_grid::TitleBar::new(title).padding(6))
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
        Content::Loading => container(text("loading…")).padding(10).into(),
        Content::Error(err) => container(text(format!("error: {err}")))
            .padding(10)
            .into(),
        Content::Brief(items) => scrollable(
            container(
                markdown::view(
                    items,
                    markdown::Settings::default(),
                    markdown::Style::from_palette(Theme::Dark.palette()),
                )
                .map(Message::LinkClicked),
            )
            .padding(10),
        )
        .height(Fill)
        .into(),
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

/// Fetch a channel's `/brief` (Markdown) from the host into `pane`.
fn fetch(pane: pane_grid::Pane, channel: &str) -> Task<Message> {
    let url = format!("{HOST}/channels/{channel}/brief");
    Task::perform(
        async move {
            let response = reqwest::get(&url).await.map_err(|e| e.to_string())?;
            response.text().await.map_err(|e| e.to_string())
        },
        move |result| Message::Fetched(pane, result),
    )
}
