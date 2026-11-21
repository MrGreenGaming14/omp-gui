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

/// Renders `state` frame after frame through one `UserInterface` cache, as the app does, and
/// applies `steps` between frames. Catches widget-state mix-ups a fresh simulator never sees.
pub fn run_frames<S, M>(state: &mut S, view: impl Fn(&S) -> Element<'_, M>, mut update: impl FnMut(&mut S, M), steps: Vec<Step<S>>) {
    use iced::advanced::{clipboard, renderer::Headless};
    let mut renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(Font::DEFAULT, Pixels(16.0), None)).expect("headless renderer");
    let at = Point::new(400.0, 300.0);
    let events = [Event::Mouse(mouse::Event::CursorMoved { position: at }), Event::Window(iced::window::Event::RedrawRequested(std::time::Instant::now()))];
    let mut frame = |state: &mut S, cache: Cache| {
        let mut messages = Vec::new();
        let mut ui = UserInterface::build(view(state), Size::new(900.0, 700.0), cache, &mut renderer);
        let _ = ui.update(&events, mouse::Cursor::Available(at), &mut renderer, &mut clipboard::Null, &mut messages);
        let cache = ui.into_cache();
        messages.into_iter().for_each(|m| update(state, m));
        cache
    };
    let mut cache = frame(state, Cache::default());
    for step in steps {
        step(state);
        cache = frame(state, cache);
        cache = frame(state, cache);
    }
}
