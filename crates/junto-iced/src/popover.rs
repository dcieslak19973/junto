//! A floating, interactive popover anchored to the widget it wraps.
//!
//! Iced 0.13 has no popover primitive. Overlays exist, but only inside
//! `pick_list`/`combo_box`/`tooltip`, and `tooltip`'s overlay is **draw-only**:
//! it implements neither `on_event` nor `operate`, so nothing inside it can be
//! clicked or take focus. An interactive floating panel therefore needs a custom
//! [`Widget`] returning a real [`overlay::Overlay`], which is what this is.
//!
//! It exists for one reason: clicking a diff row put the comment box ~800
//! physical pixels away at the bottom of the pane, so the gesture and its
//! consequence lived in different places (ledger `02ff24be`). Anchoring the
//! comment surface to the clicked row is what Zed's Delta and Warp both do.
//!
//! Unlike an inline expansion this does **not** reflow the diff: the panel
//! floats above it and escapes the enclosing `scrollable`'s clip, because
//! overlays are laid out against the whole viewport.

use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Layout, Shell, layout, mouse, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, event};

/// Draws `anchor` inline and, while `popup` is `Some`, floats it just below the
/// anchor as an interactive overlay.
pub struct Popover<'a, Message, Theme, Renderer> {
    anchor: Element<'a, Message, Theme, Renderer>,
    popup: Option<Element<'a, Message, Theme, Renderer>>,
    /// Vertical gap between the anchor's bottom edge and the panel.
    gap: f32,
    /// Panel width. A comment box wants a stable width, not one that shrinks to
    /// its content, so the caller sets it.
    width: f32,
}

impl<'a, Message, Theme, Renderer> Popover<'a, Message, Theme, Renderer> {
    /// Wrap `anchor`, floating `popup` beneath it when present.
    pub fn new(
        anchor: impl Into<Element<'a, Message, Theme, Renderer>>,
        popup: Option<Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        Self {
            anchor: anchor.into(),
            popup,
            gap: 4.0,
            width: 560.0,
        }
    }

    /// Set the floating panel's width.
    #[must_use]
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Popover<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn children(&self) -> Vec<Tree> {
        // The popup takes a child slot only while open, so opening it builds a
        // fresh tree — a newly opened comment box starts with empty state.
        let mut children = vec![Tree::new(&self.anchor)];
        if let Some(popup) = &self.popup {
            children.push(Tree::new(popup));
        }
        children
    }

    fn diff(&self, tree: &mut Tree) {
        let mut children = vec![&self.anchor];
        if let Some(popup) = &self.popup {
            children.push(popup);
        }
        tree.diff_children(&children);
    }

    fn size(&self) -> Size<Length> {
        self.anchor.as_widget().size()
    }

    fn layout(
        &self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // Only the anchor takes part in ordinary layout; the panel is laid out
        // by the overlay against the viewport, so the diff never reflows.
        self.anchor
            .as_widget()
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
        self.anchor.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) -> event::Status {
        self.anchor.as_widget_mut().on_event(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        )
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.anchor.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn operate(
        &self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.anchor
            .as_widget()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let _ = renderer;
        // `translation` carries the enclosing scrollable's offset, so the panel
        // tracks its row as the diff scrolls.
        let anchor_bounds = layout.bounds() + translation;
        let popup = self.popup.as_mut()?;
        let popup_tree = tree.children.get_mut(1)?;
        Some(overlay::Element::new(Box::new(Floating {
            popup,
            tree: popup_tree,
            anchor_bounds,
            gap: self.gap,
            width: self.width,
        })))
    }
}

impl<'a, Message, Theme, Renderer> From<Popover<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(popover: Popover<'a, Message, Theme, Renderer>) -> Self {
        Element::new(popover)
    }
}

/// The overlay half: lays the panel out just below its anchor, nudged to stay
/// inside the viewport, and forwards every interaction to it.
struct Floating<'a, 'b, Message, Theme, Renderer> {
    popup: &'b mut Element<'a, Message, Theme, Renderer>,
    tree: &'b mut Tree,
    anchor_bounds: Rectangle,
    gap: f32,
    width: f32,
}

impl<Message, Theme, Renderer> overlay::Overlay<Message, Theme, Renderer>
    for Floating<'_, '_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let viewport = Rectangle::with_size(bounds);
        let width = self.width.min(viewport.width - 16.0).max(120.0);
        let node = self.popup.as_widget().layout(
            self.tree,
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, viewport.height)).width(width),
        );
        let size = node.size();

        // Placement is delegated so it can be unit-tested without a renderer
        // (`pointing::popover_position`): below the row, flipped above when
        // there is no room, and pulled back inside the right edge.
        let (x, y) = crate::pointing::popover_position(
            crate::pointing::Rect {
                x: self.anchor_bounds.x,
                y: self.anchor_bounds.y,
                width: self.anchor_bounds.width,
                height: self.anchor_bounds.height,
            },
            (size.width, size.height),
            (viewport.width, viewport.height),
            self.gap,
        );

        layout::Node::with_children(size, vec![node]).translate(Vector::new(x, y))
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let bounds = layout.bounds();
        self.popup.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            layout.children().next().unwrap_or(layout),
            cursor,
            &bounds,
        );
    }

    fn on_event(
        &mut self,
        event: Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) -> event::Status {
        let bounds = layout.bounds();
        let inner = layout.children().next().unwrap_or(layout);
        self.popup.as_widget_mut().on_event(
            self.tree, event, inner, cursor, renderer, clipboard, shell, &bounds,
        )
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.popup.as_widget().mouse_interaction(
            self.tree,
            layout.children().next().unwrap_or(layout),
            cursor,
            viewport,
            renderer,
        )
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        // Required for focus to reach a text input inside the panel — exactly
        // what `tooltip`'s overlay omits, and why its content is inert.
        self.popup.as_widget().operate(
            self.tree,
            layout.children().next().unwrap_or(layout),
            renderer,
            operation,
        );
    }

    fn is_over(&self, layout: Layout<'_>, _renderer: &Renderer, cursor: Point) -> bool {
        layout.bounds().contains(cursor)
    }
}
