//! SPIKE — a native junto surface in Iced, to *feel* whether native is worth a
//! second codebase (docs/native-ui-toolkit-assessment.md). Minimal first: a
//! window that compiles, to lock the Iced 0.13 API before layering on
//! `pane_grid` + live channel data from the host.

use iced::widget::{button, column, text};
use iced::Element;

fn main() -> iced::Result {
    iced::run("junto (native spike)", App::update, App::view)
}

#[derive(Default)]
struct App {
    count: i32,
}

#[derive(Debug, Clone)]
enum Message {
    Increment,
}

impl App {
    fn update(&mut self, message: Message) {
        match message {
            Message::Increment => self.count += 1,
        }
    }

    fn view(&self) -> Element<'_, Message> {
        column![
            text(format!("junto native spike — count {}", self.count)),
            button("increment").on_press(Message::Increment),
        ]
        .padding(20)
        .spacing(12)
        .into()
    }
}
