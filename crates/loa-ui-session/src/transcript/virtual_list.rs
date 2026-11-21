//! The transcript's column of rows, whose widget state follows each row.
//!
//! A `Column` pairs widget state with children by position. The transcript
//! builds only a window of rows, so whenever the window slides every row's
//! state lands on another row: paragraphs are shaped again and every sensor
//! reports its size again. [`VirtualList`] pairs state by row key instead,
//! and takes the room of the rows outside the window as top and bottom
//! padding, so no spacer shifts the children either.

use std::collections::HashMap;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Shell, Widget, mouse, overlay, renderer};
use iced::{Alignment, Element, Event, Length, Padding, Rectangle, Renderer, Size, Theme, Vector};

/// Rows of the transcript's window, `top` and `bottom` pixels from the ends
/// of the content. `keys[i]` identifies the row `children[i]` draws.
pub(crate) struct VirtualList<'a, M> {
    pub(crate) top: f32,
    pub(crate) bottom: f32,
    pub(crate) keys: Vec<u64>,
    pub(crate) children: Vec<Element<'a, M>>,
}

/// The row keys `Tree::children` belong to, in order.
struct Keys(Vec<u64>);

impl<M> Widget<M, Theme, Renderer> for VirtualList<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Keys>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Keys(self.keys.clone()))
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        if tree.state.downcast_ref::<Keys>().0 == self.keys {
            tree.diff_children(&self.children);
            return;
        }
        let old_keys = std::mem::take(&mut tree.state.downcast_mut::<Keys>().0);
        let mut old: HashMap<u64, Tree> = old_keys.into_iter().zip(tree.children.drain(..)).collect();
        tree.children = self
            .keys
            .iter()
            .zip(&self.children)
            .map(|(key, child)| match old.remove(key) {
                Some(mut state) => {
                    state.diff(child);
                    state
                }
                None => Tree::new(child),
            })
            .collect();
        tree.state.downcast_mut::<Keys>().0.clone_from(&self.keys);
    }

    fn size(&self) -> Size<Length> {
        Size { width: Length::Fill, height: Length::Shrink }
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::flex::resolve(
            layout::flex::Axis::Vertical,
            renderer,
            limits,
            Length::Fill,
            Length::Shrink,
            Padding { top: self.top, bottom: self.bottom, ..Padding::ZERO },
            0.0,
            Alignment::Start,
            &mut self.children,
            &mut tree.children,
        )
    }

    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.children.iter_mut().zip(&mut tree.children).zip(layout.children()).for_each(|((child, state), layout)| {
                child.as_widget_mut().operate(state, layout, renderer, operation);
            });
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self.children.iter_mut().zip(&mut tree.children).zip(layout.children()) {
            child.as_widget_mut().update(state, event, layout, cursor, renderer, clipboard, shell, viewport);
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| child.as_widget().mouse_interaction(state, layout, cursor, viewport, renderer))
            .max()
            .unwrap_or_default()
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
        if layout.bounds().intersection(viewport).is_none() {
            return;
        }
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .filter(|(_, layout)| layout.bounds().intersects(viewport))
        {
            child.as_widget().draw(state, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        overlay::from_children(&mut self.children, tree, layout, renderer, viewport, translation)
    }
}

impl<'a, M: 'a> From<VirtualList<'a, M>> for Element<'a, M> {
    fn from(list: VirtualList<'a, M>) -> Self {
        Element::new(list)
    }
}
