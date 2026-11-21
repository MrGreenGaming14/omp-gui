//! Test helpers for UI crates: synthetic keys, one-shot simulator clicks, message delivery and
//! frame-by-frame runs through one widget cache.

use iced::keyboard::key::{Named, NativeCode, Physical};
use iced::keyboard::{self, Key, Modifiers};
use iced::{Element, Event, Font, Pixels, Point, Size, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};

/// A key press of `key` with `modifiers`, typing `text`.
pub fn key(key: Key, modifiers: Modifiers, text: Option<&str>) -> Event {
    Event::Keyboard(keyboard::Event::KeyPressed {
        modified_key: key.clone(),
        key,
        physical_key: Physical::Unidentified(NativeCode::Unidentified),
        location: keyboard::Location::Standard,
        modifiers,
        repeat: false,
        text: text.map(Into::into),
    })
}

/// A press of a named key such as Enter or Escape.
pub fn named(n: Named, modifiers: Modifiers) -> Event { key(Key::Named(n), modifiers, None) }

/// A press of a character key, typing it unless a command modifier is held.
pub fn char(c: &str, modifiers: Modifiers) -> Event { key(Key::Character(c.into()), modifiers, (!modifiers.command()).then_some(c)) }

/// The messages produced by clicking the widget labelled `label` in `view`.
pub fn click_label<'a, M>(view: impl Into<Element<'a, M>>, label: &str) -> Vec<M> {
    let mut ui = iced_test::Simulator::with_size(iced::Settings::default(), Size::new(1280.0, 800.0), view);
    ui.click(label).unwrap_or_else(|e| panic!("no `{label}` to click: {e:?}"));
    ui.into_messages().collect()
}

/// Feeds `messages` to `update` in order, collecting the events it returns.
pub fn deliver<S, M, E>(state: &mut S, messages: impl IntoIterator<Item = M>, mut update: impl FnMut(&mut S, M) -> Option<E>) -> Vec<E> {
    messages.into_iter().filter_map(|m| update(state, m)).collect()
}

/// A change applied to the state between frames.
pub type Step<S> = Box<dyn Fn(&mut S)>;

/// Draws frames one after another through one `UserInterface` cache, sending each frame events,
/// as the running app does. `iced_test`'s simulator builds every view from scratch, so it never
/// diffs one frame against the last and misses widget-state mix-ups between frames.
pub struct Frames {
    renderer: iced::Renderer,
    cache: Option<Cache>,
}

impl Frames {
    const CURSOR: Point = Point::new(400.0, 300.0);

    pub fn new() -> Self {
        use iced::advanced::renderer::Headless;
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(Font::DEFAULT, Pixels(16.0), None)).expect("headless renderer");
        Self { renderer, cache: Some(Cache::default()) }
    }

    /// Draws `view` after `events` (the cursor moves to the middle first) and returns the
    /// messages it published.
    pub fn frame<M>(&mut self, view: Element<'_, M>, events: &[Event]) -> Vec<M> {
        use iced::advanced::clipboard;
        let events: Vec<Event> = std::iter::once(Event::Mouse(mouse::Event::CursorMoved { position: Self::CURSOR }))
            .chain(events.iter().cloned())
            .chain(std::iter::once(Event::Window(iced::window::Event::RedrawRequested(std::time::Instant::now()))))
            .collect();
        let mut messages = Vec::new();
        let mut ui = UserInterface::build(view, Size::new(900.0, 700.0), self.cache.take().unwrap_or_default(), &mut self.renderer);
        let _ = ui.update(&events, mouse::Cursor::Available(Self::CURSOR), &mut self.renderer, &mut clipboard::Null, &mut messages);
        self.cache = Some(ui.into_cache());
        messages
    }
}

impl Default for Frames {
    fn default() -> Self { Self::new() }
}

/// Renders `state` frame after frame through one cache, applying `steps` between frames and two
/// frames after each. Catches widget-state mix-ups a fresh simulator never sees.
pub fn run_frames<S, M>(state: &mut S, view: impl Fn(&S) -> Element<'_, M>, mut update: impl FnMut(&mut S, M), steps: Vec<Step<S>>) {
    let mut frames = Frames::new();
    let mut frame = |state: &mut S| {
        let messages = frames.frame(view(state), &[]);
        messages.into_iter().for_each(|m| update(state, m));
    };
    frame(state);
    for step in steps {
        step(state);
        frame(state);
        frame(state);
    }
}
