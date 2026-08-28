//! The screencast widget: it renders one browser frame and injects input back
//! over CDP.
//!
//! A custom [`Widget`] rather than a bare `image` in a `mouse_area`, because the
//! browser view needs three things those primitives cannot jointly give: its
//! own laid-out bounds (to size the emulated viewport to the blade), 1:1 local
//! coordinates for press/release (`mouse_area::on_press` carries no position),
//! and keyboard capture while the page is focused. It wraps a caller-supplied
//! `content` element (the frame `image`, or a plain background before the first
//! frame) and delegates layout/draw to it — the same wrapping shape as
//! `popover.rs` — so the pixels come from iced's own image widget while this
//! layer owns only interaction.
//!
//! Focus is click-local: a press inside focuses the page, a press anywhere else
//! unfocuses it, so typing in the URL bar (a press outside these bounds) is
//! never also injected into the page. Captured keys never reach the
//! `Ctrl/Cmd+B|R` blade shortcuts either, since `iced::keyboard::listen()` only
//! sees events the UI left uncaptured.

use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Layout, Shell, layout, mouse, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, keyboard};

use crate::{browser, cdp};

/// Lines-to-pixels factor for a line-based scroll wheel, matching the step
/// other iced scrollables assume.
const LINE_HEIGHT: f32 = 40.0;

type MouseFn<'a, Message> =
    Box<dyn Fn(cdp::MouseKind, browser::PagePoint, cdp::MouseButton) -> Message + 'a>;
type ScrollFn<'a, Message> = Box<dyn Fn(browser::PagePoint, f32, f32) -> Message + 'a>;
type KeyFn<'a, Message> =
    Box<dyn Fn(browser::Key, browser::KeyPhase, browser::Modifiers) -> Message + 'a>;

/// A screencast surface wrapping `content` (the frame image or a placeholder
/// background).
pub struct Screencast<'a, Message, Theme = iced::Theme, Renderer = iced::Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_resize: Option<Box<dyn Fn(Size) -> Message + 'a>>,
    on_mouse: Option<MouseFn<'a, Message>>,
    on_scroll: Option<ScrollFn<'a, Message>>,
    on_key: Option<KeyFn<'a, Message>>,
}

/// Build a screencast surface over `content`.
pub fn screencast<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> Screencast<'a, Message, Theme, Renderer> {
    Screencast {
        content: content.into(),
        on_resize: None,
        on_mouse: None,
        on_scroll: None,
        on_key: None,
    }
}

impl<'a, Message, Theme, Renderer> Screencast<'a, Message, Theme, Renderer> {
    /// Publish the widget's logical size whenever it changes — the app turns
    /// this into the emulated viewport.
    #[must_use]
    pub fn on_resize(mut self, f: impl Fn(Size) -> Message + 'a) -> Self {
        self.on_resize = Some(Box::new(f));
        self
    }

    /// Publish a mouse press/release/move in page coordinates.
    #[must_use]
    pub fn on_mouse(
        mut self,
        f: impl Fn(cdp::MouseKind, browser::PagePoint, cdp::MouseButton) -> Message + 'a,
    ) -> Self {
        self.on_mouse = Some(Box::new(f));
        self
    }

    /// Publish a wheel scroll at a page point.
    #[must_use]
    pub fn on_scroll(mut self, f: impl Fn(browser::PagePoint, f32, f32) -> Message + 'a) -> Self {
        self.on_scroll = Some(Box::new(f));
        self
    }

    /// Publish a keyboard event while the page is focused.
    #[must_use]
    pub fn on_key(
        mut self,
        f: impl Fn(browser::Key, browser::KeyPhase, browser::Modifiers) -> Message + 'a,
    ) -> Self {
        self.on_key = Some(Box::new(f));
        self
    }
}

/// The widget's own state: its last bounds (to detect resize) and whether the
/// page currently has keyboard focus.
#[derive(Default)]
struct State {
    bounds: Rectangle,
    focused: bool,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Screencast<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: Length::Fill,
            height: Length::Fill,
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // The content is built Fill by the caller, so delegating fills the blade.
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();

        // Report a resize before anything else — the app needs the new size to
        // re-size the emulated viewport, which also forces a fresh frame.
        {
            let state: &mut State = tree.state.downcast_mut();
            if state.bounds != bounds {
                state.bounds = bounds;
                if let Some(on_resize) = &self.on_resize {
                    shell.publish(on_resize(bounds.size()));
                }
            }
        }

        let over = cursor.position_in(bounds);
        let page = |p: Point| browser::map_cursor(p.x, p.y, bounds.width, bounds.height);

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                // A press inside focuses the page; a press elsewhere unfocuses
                // it, so the URL bar and the page never both take a keystroke.
                let state: &mut State = tree.state.downcast_mut();
                state.focused = over.is_some();
                if let (Some(p), Some(on_mouse), Some(btn)) =
                    (over, &self.on_mouse, cdp_button(*button))
                {
                    shell.publish(on_mouse(cdp::MouseKind::Pressed, page(p), btn));
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(button)) => {
                if let (Some(p), Some(on_mouse), Some(btn)) =
                    (over, &self.on_mouse, cdp_button(*button))
                {
                    shell.publish(on_mouse(cdp::MouseKind::Released, page(p), btn));
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let (Some(p), Some(on_mouse)) = (over, &self.on_mouse) {
                    shell.publish(on_mouse(
                        cdp::MouseKind::Moved,
                        page(p),
                        cdp::MouseButton::Left,
                    ));
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                if let (Some(p), Some(on_scroll)) = (over, &self.on_scroll) {
                    let (dx, dy) = scroll_pixels(*delta);
                    shell.publish(on_scroll(page(p), dx, dy));
                    shell.capture_event();
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                text,
                modifiers,
                ..
            }) => {
                if tree.state.downcast_ref::<State>().focused
                    && let Some(k) = to_key(key, text.as_deref())
                    && let Some(on_key) = &self.on_key
                {
                    shell.publish(on_key(k, browser::KeyPhase::Down, to_mods(*modifiers)));
                    shell.capture_event();
                }
            }
            Event::Keyboard(keyboard::Event::KeyReleased { key, modifiers, .. }) => {
                if tree.state.downcast_ref::<State>().focused
                    && let Some(k) = to_key(key, None)
                    && let Some(on_key) = &self.on_key
                {
                    shell.publish(on_key(k, browser::KeyPhase::Up, to_mods(*modifiers)));
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }
}

impl<'a, Message, Theme, Renderer> From<Screencast<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(widget: Screencast<'a, Message, Theme, Renderer>) -> Self {
        Element::new(widget)
    }
}

/// The CDP button for an iced mouse button, or `None` for the ones the page
/// does not take (back/forward/other are handled by the browser's own history
/// controls, not injected).
fn cdp_button(button: mouse::Button) -> Option<cdp::MouseButton> {
    match button {
        mouse::Button::Left => Some(cdp::MouseButton::Left),
        mouse::Button::Right => Some(cdp::MouseButton::Right),
        mouse::Button::Middle => Some(cdp::MouseButton::Middle),
        _ => None,
    }
}

/// Wheel delta in CDP pixels. winit reports positive = up/left; CDP's
/// `deltaX`/`deltaY` are positive = right/down, so both axes are negated. A
/// line-based wheel is scaled by `LINE_HEIGHT`.
fn scroll_pixels(delta: mouse::ScrollDelta) -> (f32, f32) {
    match delta {
        mouse::ScrollDelta::Pixels { x, y } => (-x, -y),
        mouse::ScrollDelta::Lines { x, y } => (-x * LINE_HEIGHT, -y * LINE_HEIGHT),
    }
}

/// Translate an iced key to the neutral `browser::Key`, or `None` for a key v1
/// does not inject. Printable text is preferred (it respects the layout and
/// modifiers the platform already resolved).
fn to_key(key: &keyboard::Key, text: Option<&str>) -> Option<browser::Key> {
    use keyboard::key::Named as N;

    if let Some(t) = text
        && let Some(c) = t.chars().next()
        && !c.is_control()
    {
        return Some(browser::Key::Char(c));
    }

    let named = match key {
        keyboard::Key::Named(N::Enter) => browser::NamedKey::Enter,
        keyboard::Key::Named(N::Tab) => browser::NamedKey::Tab,
        keyboard::Key::Named(N::Backspace) => browser::NamedKey::Backspace,
        keyboard::Key::Named(N::Delete) => browser::NamedKey::Delete,
        keyboard::Key::Named(N::Escape) => browser::NamedKey::Escape,
        keyboard::Key::Named(N::ArrowUp) => browser::NamedKey::ArrowUp,
        keyboard::Key::Named(N::ArrowDown) => browser::NamedKey::ArrowDown,
        keyboard::Key::Named(N::ArrowLeft) => browser::NamedKey::ArrowLeft,
        keyboard::Key::Named(N::ArrowRight) => browser::NamedKey::ArrowRight,
        keyboard::Key::Named(N::Home) => browser::NamedKey::Home,
        keyboard::Key::Named(N::End) => browser::NamedKey::End,
        keyboard::Key::Named(N::PageUp) => browser::NamedKey::PageUp,
        keyboard::Key::Named(N::PageDown) => browser::NamedKey::PageDown,
        _ => return None,
    };
    Some(browser::Key::Named(named))
}

/// Fold iced modifiers into the neutral `browser::Modifiers`.
pub(crate) fn to_mods(m: keyboard::Modifiers) -> browser::Modifiers {
    browser::Modifiers {
        alt: m.alt(),
        ctrl: m.control(),
        meta: m.logo(),
        shift: m.shift(),
    }
}
