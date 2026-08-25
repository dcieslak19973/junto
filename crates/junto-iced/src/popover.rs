//! A floating, interactive popover anchored to the widget it wraps.
//!
//! Iced still has no popover primitive in 0.14. `float` (added in 0.14, PR
//! #2916) is NOT one: its `layout` delegates to `self.content.layout(...)`, so
//! the content keeps reserving layout space and `float` only lifts it into an
//! overlay for drawing and interaction. That is right for a dragged or zoomed
//! element and wrong for a panel that must not reflow the diff underneath it.
//! `tooltip` is no help either — its overlay draws only.
//!
//! So this remains a custom [`Widget`] returning a real [`overlay::Overlay`].
//! It exists because clicking a diff row used to put the comment box ~800
//! physical pixels away at the bottom of the pane, leaving the gesture and its
//! consequence in different places (ledger `02ff24be`).
//!
//! Unlike an inline expansion the panel floats above the diff without moving
//! it, and escapes the enclosing `scrollable`'s clip, because overlays are laid
//! out against the whole viewport.

use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Layout, Shell, layout, mouse, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

/// Breathing room kept between a floated panel and the viewport edge, matching
/// the 8px inset `pointing::popover_position` already applies horizontally.
const EDGE_MARGIN: f32 = 8.0;

/// A floor for the height cap, so a panel anchored hard against an edge is
/// still given a usable box rather than being squeezed to nothing.
const MIN_PANEL_HEIGHT: f32 = 120.0;

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
    /// Message published when a left click lands outside the open panel.
    /// `None` (the default) means no outside-click dismissal at all.
    on_dismiss: Option<Message>,
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
            on_dismiss: None,
        }
    }

    /// Set the floating panel's width.
    #[must_use]
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Publish `message` when a left click lands outside the open panel.
    /// Opt-in: the annotate popup leaves this unset and is unaffected.
    #[must_use]
    pub fn on_dismiss(mut self, message: Message) -> Self {
        self.on_dismiss = Some(message);
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
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // Only the anchor takes part in ordinary layout; the panel is laid out
        // by the overlay against the viewport, so the diff never reflows.
        self.anchor
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
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.anchor.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
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
        self.anchor.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.anchor
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        let _ = (renderer, viewport);
        // `translation` carries the enclosing scrollable's offset, so the panel
        // tracks its row as the diff scrolls.
        let anchor_bounds = layout.bounds() + translation;
        let popup = self.popup.as_mut()?;
        let popup_tree = tree.children.get_mut(1)?;
        let on_dismiss = self.on_dismiss.take();
        Some(overlay::Element::new(Box::new(Floating {
            popup,
            tree: popup_tree,
            anchor_bounds,
            gap: self.gap,
            width: self.width,
            on_dismiss,
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
    /// Taken from `Popover` for this frame's overlay; published on an
    /// outside left click and then consumed.
    on_dismiss: Option<Message>,
}

impl<Message, Theme, Renderer> overlay::Overlay<Message, Theme, Renderer>
    for Floating<'_, '_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let viewport = Rectangle::with_size(bounds);
        let width = self.width.min(viewport.width - 16.0).max(120.0);
        // Cap the panel to the room actually available on the roomier side of
        // the anchor, rather than to the whole viewport. Two reasons, and the
        // first is correctness: `popover_position` flips a panel above its
        // anchor with `(anchor.y - panel_h - gap).max(0.0)`, so a panel taller
        // than the space above an anchor near the bottom edge — a status-strip
        // chip, say — clamps to y = 0 and then extends back down OVER the
        // anchor. The second is that a list panel should grow to nearly the
        // window's height before it starts scrolling, so scrollbars appear only
        // when the content genuinely cannot fit.
        //
        // This is a cap, not a height: the popup still measures its own
        // content, so a short list hugs it and only a long one reaches the cap.
        let space_below = (viewport.height
            - (self.anchor_bounds.y + self.anchor_bounds.height)
            - self.gap
            - EDGE_MARGIN)
            .max(0.0);
        let space_above = (self.anchor_bounds.y - self.gap - EDGE_MARGIN).max(0.0);
        let max_height = space_below.max(space_above).max(MIN_PANEL_HEIGHT);
        let node = self.popup.as_widget_mut().layout(
            self.tree,
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, max_height)).width(width),
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

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let bounds = layout.bounds();
        let inner = layout.children().next().unwrap_or(layout);
        self.popup.as_widget_mut().update(
            self.tree, event, inner, cursor, renderer, clipboard, shell, &bounds,
        );

        // Outside-click dismissal: only a left press that lands off the panel
        // counts, so a drag that starts inside and ends outside (e.g. text
        // selection) does not close it.
        //
        // The anchor is deliberately NOT "outside". This overlay does not
        // capture the event, so the press still reaches the anchor's own
        // `on_press` underneath. If it ALSO dismissed here, a trigger whose
        // press toggles the panel would get two messages in one input cycle —
        // close, then open — and look unable to close itself. Pressing a
        // trigger is the trigger's business.
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) && !cursor.is_over(bounds)
            && !cursor.is_over(self.anchor_bounds)
            && let Some(message) = self.on_dismiss.take()
        {
            shell.publish(message);
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();
        self.popup.as_widget().mouse_interaction(
            self.tree,
            layout.children().next().unwrap_or(layout),
            cursor,
            &bounds,
            renderer,
        )
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        // Required for focus to reach a text input inside the panel — exactly
        // what `tooltip`'s overlay omits, and why its content is inert.
        self.popup.as_widget_mut().operate(
            self.tree,
            layout.children().next().unwrap_or(layout),
            renderer,
            operation,
        );
    }
}
