//! Message-scoped gesture state shared by the selectable Markdown blocks in document order.
use super::{Message, Selectable};
use iced::advanced::graphics::text::cosmic_text::Cursor;
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, renderer};
use iced::widget::{markdown::Uri, text::{LineHeight, Span}};
use iced::{Element, Event, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Theme, keyboard, mouse};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Default)]
pub(in crate::transcript) struct Group(Rc<Shared>);

// The tree owns gestures across rebuilds; Group shares them with the current view's blocks.
#[derive(Default)]
struct Shared {
    next: Cell<usize>,
    gesture: Cell<Gesture>,
    copying: Cell<bool>,
    copied: RefCell<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Endpoint {
    pub block: usize,
    pub caret: Cursor,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Gesture {
    pub anchor: Option<Endpoint>,
    pub end: Option<Endpoint>,
    pub press: Option<Press>,
}

#[derive(Clone, Copy)]
pub(super) struct Press {
    pub at: Point,
    pub link: Option<usize>,
    pub dragged: bool,
}

impl Gesture {
    pub(super) fn range(self, block: usize, last: Cursor) -> Option<(Cursor, Cursor)> {
        let (anchor, end) = (self.anchor?, self.end?);
        let (lo, hi) = (anchor.min(end), anchor.max(end));
        if lo == hi || !(lo.block..=hi.block).contains(&block) { return None; }
        Some((if lo.block == block { lo.caret } else { Cursor::new(0, 0) }, if hi.block == block { hi.caret } else { last }))
    }
}

impl Group {
    pub(in crate::transcript) fn text<'a>(&self, spans: Vec<Span<'a, Uri>>, size: Pixels, line_height: LineHeight, font: Font) -> Element<'a, Message> {
        let block = self.0.next.get();
        self.0.next.set(block + 1);
        Element::new(Selectable { spans, size, line_height, font, block, group: self.clone() })
    }

    pub(in crate::transcript) fn wrap<'a>(self, body: Element<'a, Message>) -> Element<'a, Message> {
        Element::new(SelectionArea { body, group: self })
    }

    pub(super) fn gesture(&self) -> Gesture { self.0.gesture.get() }
    pub(super) fn set(&self, gesture: Gesture) { self.0.gesture.set(gesture); }
    pub(super) fn is_single(&self) -> bool { self.0.next.get() == 1 }

    pub(super) fn copy(&self, text: &str) {
        if !self.0.copying.get() || text.is_empty() { return; }
        let mut out = self.0.copied.borrow_mut();
        if !out.is_empty() { out.push_str("\n\n"); }
        out.push_str(text);
    }

    pub(super) fn copying(&self) -> bool { self.0.copying.get() }
}

struct SelectionArea<'a> {
    body: Element<'a, Message>,
    group: Group,
}

impl Widget<Message, Theme, Renderer> for SelectionArea<'_> {
    fn tag(&self) -> tree::Tag { tree::Tag::of::<Gesture>() }
    fn state(&self) -> tree::State { tree::State::new(Gesture::default()) }
    fn children(&self) -> Vec<Tree> { vec![Tree::new(&self.body)] }
    fn diff(&self, tree: &mut Tree) { tree.diff_children(std::slice::from_ref(&self.body)); }
    fn size(&self) -> Size<Length> { self.body.as_widget().size() }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        self.group.set(*tree.state.downcast_ref::<Gesture>());
        let node = self.body.as_widget_mut().layout(&mut tree.children[0], renderer, limits);
        *tree.state.downcast_mut::<Gesture>() = self.group.gesture();
        node
    }

    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.body.as_widget_mut().operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(&mut self, tree: &mut Tree, event: &Event, layout: Layout<'_>, cursor: mouse::Cursor,
        renderer: &Renderer, clipboard: &mut dyn Clipboard, shell: &mut Shell<'_, Message>, viewport: &Rectangle,
    ) {
        let stored = tree.state.downcast_mut::<Gesture>();
        let reset = matches!(event, Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) | Event::Window(iced::window::Event::Unfocused));
        if reset && stored.anchor.is_some() { shell.request_redraw(); }
        self.group.set(if reset { Gesture::default() } else { *stored });
        let copying = stored.anchor != stored.end && layout.bounds().intersects(viewport) && !shell.is_event_captured()
            && matches!(event, Event::Keyboard(keyboard::Event::KeyPressed { key, physical_key, modifiers, .. })
                if modifiers.command() && !modifiers.alt() && !modifiers.shift()
                    && key.to_latin(*physical_key).is_some_and(|c| c.eq_ignore_ascii_case(&'c')));
        self.group.0.copying.set(copying);
        self.body.as_widget_mut().update(&mut tree.children[0], event, layout, cursor, renderer, clipboard, shell, viewport);
        if copying {
            self.group.0.copying.set(false);
            let text = self.group.0.copied.take();
            if !text.is_empty() && !shell.is_event_captured() {
                shell.publish(Message::Copy(text));
                shell.capture_event();
            }
        }
        let mut gesture = self.group.gesture();
        if matches!(event, Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))) && let Some(press) = gesture.press.take() {
            if !press.dragged { gesture = Gesture::default(); }
            self.group.set(gesture);
            shell.request_redraw();
            shell.capture_event();
        }
        *stored = gesture;
    }

    fn draw(&self, tree: &Tree, renderer: &mut Renderer, theme: &Theme, style: &renderer::Style,
        layout: Layout<'_>, cursor: mouse::Cursor, viewport: &Rectangle,
    ) {
        self.body.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: mouse::Cursor,
        viewport: &Rectangle, renderer: &Renderer,
    ) -> mouse::Interaction {
        self.body.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }
}
