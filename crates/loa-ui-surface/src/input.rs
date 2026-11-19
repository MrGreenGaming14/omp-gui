//! Translating iced input into the surface contract's terms.

use iced::keyboard::{self, Key, key};
use iced::mouse;
use loa_surface::{Input, Modifiers, MouseButton};

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
    /// Builds a key input from an iced keyboard event; `None` for modifier
    /// changes.
    pub fn from_event(event: &keyboard::Event) -> Option<Self> {
        match event {
            keyboard::Event::KeyPressed {
                modified_key,
                physical_key,
                modifiers,
                text,
                ..
            } => Some(Self {
                key: key_value(modified_key),
                code: code_value(physical_key),
                pressed: true,
                modifiers: convert_modifiers(*modifiers),
                text: text.as_ref().map(|t| t.to_string()),
            }),
            keyboard::Event::KeyReleased {
                modified_key,
                physical_key,
                modifiers,
                ..
            } => Some(Self {
                key: key_value(modified_key),
                code: code_value(physical_key),
                pressed: false,
                modifiers: convert_modifiers(*modifiers),
                text: None,
            }),
            keyboard::Event::ModifiersChanged(_) => None,
        }
    }

    /// What to send for this key. `held` holds the codes whose press went out
    /// as [`Input::Key`].
    ///
    /// Typing goes as committed [`Input::Text`] and its release is dropped, so
    /// the server sees each character once. Keys that type nothing (arrows,
    /// Enter, shortcuts with Ctrl, Alt or Meta) go as [`Input::Key`] presses,
    /// and their releases follow even when the modifier was let go first.
    pub fn to_input(&self, held: &mut Vec<String>) -> Option<Input> {
        if self.pressed {
            let chord = self.modifiers.ctrl || self.modifiers.alt || self.modifiers.meta;
            if !chord
                && let Some(text) = &self.text
                && is_typed(text)
            {
                return Some(Input::Text { text: text.clone() });
            }
            if !held.contains(&self.code) {
                held.push(self.code.clone());
            }
        } else {
            let i = held.iter().position(|code| *code == self.code)?;
            held.swap_remove(i);
        }
        Some(Input::Key {
            key: self.key.clone(),
            code: self.code.clone(),
            pressed: self.pressed,
            modifiers: self.modifiers,
        })
    }
}

fn is_typed(text: &str) -> bool {
    !text.is_empty() && !text.chars().any(char::is_control)
}

/// The W3C `KeyboardEvent.key` value. iced's named keys carry the W3C names,
/// except those listed here.
pub fn key_value(key: &Key) -> String {
    match key {
        Key::Character(c) => c.to_string(),
        Key::Named(key::Named::Space) => " ".to_owned(),
        Key::Named(key::Named::Super) => "Meta".to_owned(),
        Key::Named(named) => format!("{named:?}"),
        Key::Unidentified => "Unidentified".to_owned(),
    }
}

/// The W3C `KeyboardEvent.code` value. iced's codes carry the W3C names,
/// except those listed here.
pub fn code_value(physical: &key::Physical) -> String {
    match physical {
        key::Physical::Code(key::Code::SuperLeft) => "MetaLeft".to_owned(),
        key::Physical::Code(key::Code::SuperRight) => "MetaRight".to_owned(),
        key::Physical::Code(code) => format!("{code:?}"),
        key::Physical::Unidentified(_) => "Unidentified".to_owned(),
    }
}

pub fn convert_modifiers(m: keyboard::Modifiers) -> Modifiers {
    Modifiers {
        shift: m.shift(),
        ctrl: m.control(),
        alt: m.alt(),
        meta: m.logo(),
    }
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

/// A wheel movement in panel pixels, with positive `dy` scrolling the
/// content down as the contract wants. iced reports wheel up as positive.
pub fn scroll_pixels(delta: mouse::ScrollDelta) -> (f32, f32) {
    match delta {
        mouse::ScrollDelta::Lines { x, y } => (-x * LINE_PX, -y * LINE_PX),
        mouse::ScrollDelta::Pixels { x, y } => (-x, -y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(
        key: Key,
        code: key::Code,
        text: Option<&str>,
        modifiers: keyboard::Modifiers,
    ) -> KeyInput {
        KeyInput::from_event(&keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: key::Physical::Code(code),
            location: keyboard::Location::Standard,
            modifiers,
            text: text.map(Into::into),
            repeat: false,
        })
        .unwrap()
    }

    fn release(key: Key, code: key::Code) -> KeyInput {
        KeyInput::from_event(&keyboard::Event::KeyReleased {
            key: key.clone(),
            modified_key: key,
            physical_key: key::Physical::Code(code),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
        })
        .unwrap()
    }

    #[test]
    fn typing_a_letter_sends_the_text_once() {
        let mut held = Vec::new();
        let a = Key::Character("A".into());
        let down = press(
            a.clone(),
            key::Code::KeyA,
            Some("A"),
            keyboard::Modifiers::SHIFT,
        );
        assert_eq!(
            down.to_input(&mut held),
            Some(Input::Text { text: "A".into() })
        );
        assert_eq!(release(a, key::Code::KeyA).to_input(&mut held), None);
    }

    #[test]
    fn space_is_typed_as_a_space() {
        let mut held = Vec::new();
        let space = Key::Named(key::Named::Space);
        assert_eq!(
            press(
                space.clone(),
                key::Code::Space,
                Some(" "),
                keyboard::Modifiers::empty()
            )
            .to_input(&mut held),
            Some(Input::Text { text: " ".into() })
        );
        assert_eq!(release(space, key::Code::Space).to_input(&mut held), None);
    }

    #[test]
    fn named_keys_and_shortcuts_go_as_w3c_keys() {
        let mut held = Vec::new();
        let enter = press(
            Key::Named(key::Named::Enter),
            key::Code::Enter,
            Some("\r"),
            keyboard::Modifiers::empty(),
        );
        assert_eq!(
            enter.to_input(&mut held),
            Some(Input::Key {
                key: "Enter".into(),
                code: "Enter".into(),
                pressed: true,
                modifiers: Modifiers::default()
            })
        );
        let meta = press(
            Key::Named(key::Named::Super),
            key::Code::SuperLeft,
            None,
            keyboard::Modifiers::LOGO,
        );
        assert!(
            matches!(meta.to_input(&mut held), Some(Input::Key { ref key, ref code, .. }) if key == "Meta" && code == "MetaLeft")
        );
    }

    #[test]
    fn shortcut_release_follows_after_the_modifier_is_let_go() {
        let mut held = Vec::new();
        let c = Key::Character("c".into());
        let ctrl_c = press(
            c.clone(),
            key::Code::KeyC,
            Some("c"),
            keyboard::Modifiers::CTRL,
        );
        let Some(Input::Key {
            key,
            code,
            pressed: true,
            modifiers,
        }) = ctrl_c.to_input(&mut held)
        else {
            panic!("ctrl+c must go as a key");
        };
        assert_eq!(
            (key.as_str(), code.as_str(), modifiers.ctrl),
            ("c", "KeyC", true)
        );
        // Ctrl is up by the time C is released: the release still goes out.
        let up = release(c, key::Code::KeyC).to_input(&mut held);
        assert!(matches!(up, Some(Input::Key { pressed: false, ref code, .. }) if code == "KeyC"));
        // A release whose press never went out (focus came later) is dropped.
        assert_eq!(
            release(Key::Named(key::Named::ArrowLeft), key::Code::ArrowLeft).to_input(&mut held),
            None
        );
    }

    #[test]
    fn wheel_up_scrolls_content_up() {
        assert_eq!(
            scroll_pixels(mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 }),
            (-0.0, -LINE_PX)
        );
        assert_eq!(
            scroll_pixels(mouse::ScrollDelta::Pixels { x: 3.0, y: -12.0 }),
            (-3.0, 12.0)
        );
    }
}
