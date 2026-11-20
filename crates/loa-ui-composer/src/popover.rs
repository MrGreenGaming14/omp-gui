//! A popover: content drawn as an overlay above its anchor, so opening a
//! menu never moves the page. The anchor lays out as if the popover weren't
//! there; the popover sits `gap` above it, as wide as the anchor or, with
//! [`Popover::own_width`], as wide as itself from the anchor's left edge.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, mouse, overlay};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector};

/// `anchor` with `popup` floating above it, when there is one.
pub struct Popover<'a, Message, Theme, Renderer> {
    anchor: Element<'a, Message, Theme, Renderer>,
    popup: Option<Element<'a, Message, Theme, Renderer>>,
    gap: f32,
    fit_anchor: bool,
    on_dismiss: Option<Message>,
}

impl<'a, Message, Theme, Renderer> Popover<'a, Message, Theme, Renderer> {
    pub fn new(anchor: impl Into<Element<'a, Message, Theme, Renderer>>, popup: Option<Element<'a, Message, Theme, Renderer>>) -> Self {
        Self { anchor: anchor.into(), popup, gap: 6.0, fit_anchor: true, on_dismiss: None }
    }

    /// Lays the popup out at its own width, starting at the anchor's left
    /// edge and shifted left as needed to stay inside the window. For a menu
    /// that opens from a small button.
    pub fn own_width(mut self) -> Self {
        self.fit_anchor = false;
        self
    }

    /// Publishes `message` when a mouse button is pressed outside both the
    /// open popup and its anchor. The anchor is left out, so its own toggle
    /// still closes the popup.
    pub fn on_dismiss(mut self, message: Message) -> Self {
        self.on_dismiss = Some(message);
        self
    }
}

impl<Message: Clone, Theme, Renderer> Widget<Message, Theme, Renderer> for Popover<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    // The anchor is always child 0, so its state survives the popup coming
    // and going; the popup, when there is one, is child 1.
    fn children(&self) -> Vec<Tree> {
        std::iter::once(Tree::new(&self.anchor)).chain(self.popup.as_ref().map(Tree::new)).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        match &self.popup {
            Some(popup) => tree.diff_children(&[self.anchor.as_widget(), popup.as_widget()]),
            None => tree.diff_children(&[self.anchor.as_widget()]),
        }
    }

    fn size(&self) -> Size<Length> {
        self.anchor.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.anchor.as_widget().size_hint()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        self.anchor.as_widget_mut().layout(&mut tree.children[0], renderer, limits)
    }

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
        self.anchor.as_widget_mut().update(&mut tree.children[0], event, layout, cursor, renderer, clipboard, shell, viewport);
    }

    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: mouse::Cursor, viewport: &Rectangle, renderer: &Renderer) -> mouse::Interaction {
        self.anchor.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
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
        self.anchor.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }

    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.anchor.as_widget_mut().operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let mut children = tree.children.iter_mut();
        let anchor_tree = children.next()?;
        let anchor = self.anchor.as_widget_mut().overlay(anchor_tree, layout, renderer, viewport, translation);
        let popup = match (&mut self.popup, children.next()) {
            (Some(popup), Some(popup_tree)) => Some(overlay::Element::new(Box::new(Overlay {
                popup,
                tree: popup_tree,
                anchor: layout.bounds() + translation,
                gap: self.gap,
                fit_anchor: self.fit_anchor,
                on_dismiss: self.on_dismiss.clone(),
            }))),
            _ => None,
        };
        match (anchor, popup) {
            (None, None) => None,
            (anchor, popup) => Some(overlay::Group::with_children(anchor.into_iter().chain(popup).collect()).overlay()),
        }
    }
}

struct Overlay<'a, 'b, Message, Theme, Renderer> {
    popup: &'b mut Element<'a, Message, Theme, Renderer>,
    tree: &'b mut Tree,
    anchor: Rectangle,
    gap: f32,
    fit_anchor: bool,
    on_dismiss: Option<Message>,
}

impl<Message: Clone, Theme, Renderer> overlay::Overlay<Message, Theme, Renderer> for Overlay<'_, '_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        // Above the anchor when it fits there, else below it.
        let above = (self.anchor.y - self.gap).max(0.0);
        let below = (bounds.height - self.anchor.y - self.anchor.height - self.gap).max(0.0);
        let room = above.max(below);
        let max_width = if self.fit_anchor { self.anchor.width } else { bounds.width };
        let limits = layout::Limits::new(Size::ZERO, Size::new(max_width, room));
        let node = self.popup.as_widget_mut().layout(self.tree, renderer, &limits);
        let Size { width, height } = node.size();
        let y = if height <= above || above >= below { self.anchor.y - self.gap - height } else { self.anchor.y + self.anchor.height + self.gap };
        let x = if self.fit_anchor { self.anchor.x } else { self.anchor.x.min(bounds.width - width).max(0.0) };
        node.move_to(Point::new(x, y.max(0.0)))
    }

    fn draw(&self, renderer: &mut Renderer, theme: &Theme, style: &renderer::Style, layout: Layout<'_>, cursor: mouse::Cursor) {
        self.popup.as_widget().draw(self.tree, renderer, theme, style, layout, cursor, &layout.bounds());
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.popup.as_widget_mut().operate(self.tree, layout, renderer, operation);
    }

    fn update(&mut self, event: &Event, layout: Layout<'_>, cursor: mouse::Cursor, renderer: &Renderer, clipboard: &mut dyn Clipboard, shell: &mut Shell<'_, Message>) {
        let bounds = layout.bounds();
        if let (Some(message), Event::Mouse(mouse::Event::ButtonPressed(_))) = (&self.on_dismiss, event)
            && !cursor.is_over(bounds)
            && !cursor.is_over(self.anchor)
        {
            shell.publish(message.clone());
        }
        self.popup.as_widget_mut().update(self.tree, event, layout, cursor, renderer, clipboard, shell, &bounds);
    }

    fn mouse_interaction(&self, layout: Layout<'_>, cursor: mouse::Cursor, renderer: &Renderer) -> mouse::Interaction {
        self.popup.as_widget().mouse_interaction(self.tree, layout, cursor, &layout.bounds(), renderer)
    }

    fn overlay<'c>(&'c mut self, layout: Layout<'c>, renderer: &Renderer) -> Option<overlay::Element<'c, Message, Theme, Renderer>> {
        self.popup.as_widget_mut().overlay(self.tree, layout, renderer, &layout.bounds(), Vector::ZERO)
    }
}

impl<'a, Message, Theme, Renderer> From<Popover<'a, Message, Theme, Renderer>> for Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(popover: Popover<'a, Message, Theme, Renderer>) -> Self {
        Element::new(popover)
    }
}

/// `anchor` with `popup` floating above it.
pub fn popover<'a, Message, Theme, Renderer>(
    anchor: impl Into<Element<'a, Message, Theme, Renderer>>,
    popup: Option<Element<'a, Message, Theme, Renderer>>,
) -> Popover<'a, Message, Theme, Renderer> {
    Popover::new(anchor, popup)
}
