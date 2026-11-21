//! A popover: content drawn as an overlay above its anchor (below when there is no room), so
//! opening a menu never moves the page. The popover is as wide as the anchor or, with
//! [`Popover::own_width`], as wide as itself from the anchor's left edge.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, mouse, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector};

/// `anchor` with `popup` floating above it, when there is one.
pub struct Popover<'a, M, T, R> {
    anchor: Element<'a, M, T, R>,
    popup: Option<Element<'a, M, T, R>>,
    gap: f32,
    fit_anchor: bool,
    on_dismiss: Option<M>,
}

/// `anchor` with `popup` floating above it.
pub fn popover<'a, M, T, R>(anchor: impl Into<Element<'a, M, T, R>>, popup: Option<Element<'a, M, T, R>>) -> Popover<'a, M, T, R> {
    Popover { anchor: anchor.into(), popup, gap: 6.0, fit_anchor: true, on_dismiss: None }
}

impl<M, T, R> Popover<'_, M, T, R> {
    /// Lays the popup out at its own width, from the anchor's left edge and shifted left as
    /// needed to stay inside the window: a menu that opens from a small button.
    pub fn own_width(mut self) -> Self {
        self.fit_anchor = false;
        self
    }

    /// Publishes `message` when a mouse button is pressed outside both the open popup and its
    /// anchor. The anchor is left out, so its own toggle still closes the popup.
    pub fn on_dismiss(mut self, message: M) -> Self {
        self.on_dismiss = Some(message);
        self
    }
}

impl<M: Clone, T, R: renderer::Renderer> Widget<M, T, R> for Popover<'_, M, T, R> {
    // The anchor is always child 0, so its state survives the popup coming and going.
    fn children(&self) -> Vec<Tree> { std::iter::once(Tree::new(&self.anchor)).chain(self.popup.as_ref().map(Tree::new)).collect() }

    fn diff(&self, tree: &mut Tree) {
        match &self.popup {
            Some(popup) => tree.diff_children(&[self.anchor.as_widget(), popup.as_widget()]),
            None => tree.diff_children(&[self.anchor.as_widget()]),
        }
    }

    fn size(&self) -> Size<Length> { self.anchor.as_widget().size() }

    fn size_hint(&self) -> Size<Length> { self.anchor.as_widget().size_hint() }

    fn layout(&mut self, tree: &mut Tree, r: &R, limits: &layout::Limits) -> layout::Node { self.anchor.as_widget_mut().layout(&mut tree.children[0], r, limits) }

    fn update(&mut self, tree: &mut Tree, e: &Event, l: Layout<'_>, c: mouse::Cursor, r: &R, cb: &mut dyn Clipboard, s: &mut Shell<'_, M>, v: &Rectangle) {
        self.anchor.as_widget_mut().update(&mut tree.children[0], e, l, c, r, cb, s, v);
    }

    fn mouse_interaction(&self, tree: &Tree, l: Layout<'_>, c: mouse::Cursor, v: &Rectangle, r: &R) -> mouse::Interaction {
        self.anchor.as_widget().mouse_interaction(&tree.children[0], l, c, v, r)
    }

    fn draw(&self, tree: &Tree, r: &mut R, t: &T, s: &renderer::Style, l: Layout<'_>, c: mouse::Cursor, v: &Rectangle) {
        self.anchor.as_widget().draw(&tree.children[0], r, t, s, l, c, v);
    }

    fn operate(&mut self, tree: &mut Tree, l: Layout<'_>, r: &R, op: &mut dyn Operation) { self.anchor.as_widget_mut().operate(&mut tree.children[0], l, r, op) }

    fn overlay<'b>(&'b mut self, tree: &'b mut Tree, l: Layout<'b>, r: &R, v: &Rectangle, at: Vector) -> Option<overlay::Element<'b, M, T, R>> {
        let mut children = tree.children.iter_mut();
        let anchor = self.anchor.as_widget_mut().overlay(children.next()?, l, r, v, at);
        let popup = match (&mut self.popup, children.next()) {
            (Some(popup), Some(tree)) => {
                let (gap, fit_anchor, on_dismiss) = (self.gap, self.fit_anchor, self.on_dismiss.clone());
                Some(overlay::Element::new(Box::new(Overlay { popup, tree, anchor: l.bounds() + at, gap, fit_anchor, on_dismiss })))
            }
            _ => None,
        };
        match (anchor, popup) {
            (None, None) => None,
            (anchor, popup) => Some(overlay::Group::with_children(anchor.into_iter().chain(popup).collect()).overlay()),
        }
    }
}

struct Overlay<'a, 'b, M, T, R> {
    popup: &'b mut Element<'a, M, T, R>,
    tree: &'b mut Tree,
    anchor: Rectangle,
    gap: f32,
    fit_anchor: bool,
    on_dismiss: Option<M>,
}

impl<M: Clone, T, R: renderer::Renderer> overlay::Overlay<M, T, R> for Overlay<'_, '_, M, T, R> {
    fn layout(&mut self, r: &R, bounds: Size) -> layout::Node {
        let above = (self.anchor.y - self.gap).max(0.0);
        let below = (bounds.height - self.anchor.y - self.anchor.height - self.gap).max(0.0);
        let max_width = if self.fit_anchor { self.anchor.width } else { bounds.width };
        let limits = layout::Limits::new(Size::ZERO, Size::new(max_width, above.max(below)));
        let node = self.popup.as_widget_mut().layout(self.tree, r, &limits);
        let Size { width, height } = node.size();
        let y = if height <= above || above >= below { self.anchor.y - self.gap - height } else { self.anchor.y + self.anchor.height + self.gap };
        let x = if self.fit_anchor { self.anchor.x } else { self.anchor.x.min(bounds.width - width).max(0.0) };
        node.move_to(Point::new(x, y.max(0.0)))
    }

    fn draw(&self, r: &mut R, t: &T, s: &renderer::Style, l: Layout<'_>, c: mouse::Cursor) { self.popup.as_widget().draw(self.tree, r, t, s, l, c, &l.bounds()) }

    fn operate(&mut self, l: Layout<'_>, r: &R, op: &mut dyn Operation) { self.popup.as_widget_mut().operate(self.tree, l, r, op) }

    fn update(&mut self, e: &Event, l: Layout<'_>, c: mouse::Cursor, r: &R, cb: &mut dyn Clipboard, s: &mut Shell<'_, M>) {
        if let (Some(message), Event::Mouse(mouse::Event::ButtonPressed(_))) = (&self.on_dismiss, e)
            && !c.is_over(l.bounds())
            && !c.is_over(self.anchor)
        {
            s.publish(message.clone());
        }
        self.popup.as_widget_mut().update(self.tree, e, l, c, r, cb, s, &l.bounds());
    }

    fn mouse_interaction(&self, l: Layout<'_>, c: mouse::Cursor, r: &R) -> mouse::Interaction { self.popup.as_widget().mouse_interaction(self.tree, l, c, &l.bounds(), r) }

    fn overlay<'c>(&'c mut self, l: Layout<'c>, r: &R) -> Option<overlay::Element<'c, M, T, R>> { self.popup.as_widget_mut().overlay(self.tree, l, r, &l.bounds(), Vector::ZERO) }
}

impl<'a, M: Clone + 'a, T: 'a, R: renderer::Renderer + 'a> From<Popover<'a, M, T, R>> for Element<'a, M, T, R> {
    fn from(p: Popover<'a, M, T, R>) -> Self { Element::new(p) }
}
