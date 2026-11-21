//! Translating iced keys, buttons and wheel movements into the surface contract's W3C terms.

use iced::keyboard::{self, Key, key};
use iced::mouse;
use loa_view::surface::{Input, Modifiers, MouseButton};

/// Pixels one wheel line scrolls.
pub const LINE_PX: f32 = 40.0;

/// A key press or release, already in W3C terms.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyInput {
    /// `KeyboardEvent.key`.
    pub key: String,
    /// `KeyboardEvent.code`.
    pub code: String,
    pub pressed: bool,
    pub modifiers: Modifiers,
    /// Text the press produced, if any.
    pub text: Option<String>,
}

impl KeyInput {
    /// The key input of an iced keyboard event; `None` for modifier changes.
    pub fn from_event(event: &keyboard::Event) -> Option<Self> {
        let (modified_key, physical_key, modifiers, text, pressed) = match event {
            keyboard::Event::KeyPressed { modified_key, physical_key, modifiers, text, .. } => {
                (modified_key, physical_key, modifiers, text.as_ref().map(|t| t.to_string()), true)
            }
            keyboard::Event::KeyReleased { modified_key, physical_key, modifiers, .. } => (modified_key, physical_key, modifiers, None, false),
            keyboard::Event::ModifiersChanged(_) => return None,
        };
        Some(Self { key: key_value(modified_key), code: code_value(physical_key), pressed, modifiers: convert_modifiers(*modifiers), text })
    }

    /// What to send for this key. `held` holds the codes whose press went out as [`Input::Key`].
    ///
    /// Typing goes as committed [`Input::Text`] and its release is dropped, so the server sees
    /// each character once. Keys that type nothing (arrows, Enter, chords with Ctrl, Alt or Meta)
    /// go as [`Input::Key`], and their releases follow even when the modifier was let go first.
    pub fn to_input(&self, held: &mut Vec<String>) -> Option<Input> {
        if self.pressed {
            let chord = self.modifiers.ctrl || self.modifiers.alt || self.modifiers.meta;
            if let Some(text) = self.text.as_ref().filter(|t| !chord && !t.is_empty() && !t.chars().any(char::is_control)) {
                return Some(Input::Text { text: text.clone() });
            }
            if !held.contains(&self.code) {
                held.push(self.code.clone());
            }
        } else {
            let i = held.iter().position(|code| *code == self.code)?;
            held.swap_remove(i);
        }
        Some(Input::Key { key: self.key.clone(), code: self.code.clone(), pressed: self.pressed, modifiers: self.modifiers })
    }
}

/// The W3C `KeyboardEvent.key` value; iced's named keys carry the W3C names but these.
pub fn key_value(key: &Key) -> String {
    match key {
        Key::Character(c) => c.to_string(),
        Key::Named(key::Named::Space) => " ".to_owned(),
        Key::Named(key::Named::Super) => "Meta".to_owned(),
        Key::Named(named) => format!("{named:?}"),
        Key::Unidentified => "Unidentified".to_owned(),
    }
}

/// The W3C `KeyboardEvent.code` value; iced's codes carry the W3C names but these.
pub fn code_value(physical: &key::Physical) -> String {
    match physical {
        key::Physical::Code(key::Code::SuperLeft) => "MetaLeft".to_owned(),
        key::Physical::Code(key::Code::SuperRight) => "MetaRight".to_owned(),
        key::Physical::Code(code) => format!("{code:?}"),
        key::Physical::Unidentified(_) => "Unidentified".to_owned(),
    }
}

pub fn convert_modifiers(m: keyboard::Modifiers) -> Modifiers {
    Modifiers { shift: m.shift(), ctrl: m.control(), alt: m.alt(), meta: m.logo() }
}

pub fn convert_button(button: mouse::Button) -> Option<MouseButton> {
    match button {
        mouse::Button::Left => Some(MouseButton::Left),
        mouse::Button::Right => Some(MouseButton::Right),
        mouse::Button::Middle => Some(MouseButton::Middle),
        mouse::Button::Back => Some(MouseButton::Back),
        mouse::Button::Forward => Some(MouseButton::Forward),
        mouse::Button::Other(_) => None,
    }
}

/// A wheel movement in panel pixels, positive `dy` scrolling the content down as the contract
/// wants. iced reports wheel up as positive.
pub fn scroll_pixels(delta: mouse::ScrollDelta) -> (f32, f32) {
    match delta {
        mouse::ScrollDelta::Lines { x, y } => (-x * LINE_PX, -y * LINE_PX),
        mouse::ScrollDelta::Pixels { x, y } => (-x, -y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyboard::Modifiers as M;
    use key::{Code, Named};

    fn event(key: Key, code: Code, text: Option<&str>, modifiers: M, pressed: bool) -> KeyInput {
        let (modified_key, physical_key, location) = (key.clone(), key::Physical::Code(code), keyboard::Location::Standard);
        KeyInput::from_event(&if pressed {
            keyboard::Event::KeyPressed { key, modified_key, physical_key, location, modifiers, text: text.map(Into::into), repeat: false }
        } else {
            keyboard::Event::KeyReleased { key, modified_key, physical_key, location, modifiers }
        })
        .unwrap()
    }

    #[test]
    fn keys_map_to_text_or_w3c_keys() {
        let ch = |c: &str| Key::Character(c.into());
        let text = |t: &str| Some(Input::Text { text: t.into() });
        let w3c = |key: &str, code: &str, pressed, ctrl| {
            Some(Input::Key { key: key.into(), code: code.into(), pressed, modifiers: Modifiers { ctrl, ..Modifiers::default() } })
        };
        let mut held = Vec::new();
        // (key, code, text, modifiers, pressed, sent), applied in order to one held set.
        let cases = [
            (ch("A"), Code::KeyA, Some("A"), M::SHIFT, true, Some(Input::Text { text: "A".into() })),
            (ch("A"), Code::KeyA, None, M::empty(), false, None),
            (Key::Named(Named::Space), Code::Space, Some(" "), M::empty(), true, text(" ")),
            (Key::Named(Named::Enter), Code::Enter, Some("\r"), M::empty(), true, w3c("Enter", "Enter", true, false)),
            (ch("c"), Code::KeyC, Some("c"), M::CTRL, true, w3c("c", "KeyC", true, true)),
            // Ctrl is up by the time C is released: the release still goes out.
            (ch("c"), Code::KeyC, None, M::empty(), false, w3c("c", "KeyC", false, false)),
            // A release whose press never went out (focus came later) is dropped.
            (Key::Named(Named::ArrowLeft), Code::ArrowLeft, None, M::empty(), false, None),
        ];
        for (key, code, typed, modifiers, pressed, sent) in cases {
            assert_eq!(event(key.clone(), code, typed, modifiers, pressed).to_input(&mut held), sent, "{key:?} pressed {pressed}");
        }
        let meta = event(Key::Named(Named::Super), Code::SuperLeft, None, M::LOGO, true);
        assert_eq!((meta.key.as_str(), meta.code.as_str()), ("Meta", "MetaLeft"));
    }

    #[test]
    fn wheel_up_scrolls_content_up() {
        assert_eq!(scroll_pixels(mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 }), (-0.0, -LINE_PX));
        assert_eq!(scroll_pixels(mouse::ScrollDelta::Pixels { x: 3.0, y: -12.0 }), (-3.0, 12.0));
    }
}
