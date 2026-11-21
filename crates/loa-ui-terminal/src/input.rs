//! Key presses and pastes encoded as the bytes a terminal program expects (xterm conventions).

use alacritty_terminal::term::TermMode;
use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};
use std::borrow::Cow;

/// What a key press means to the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    Send(Cow<'static, [u8]>),
    Copy,
    Paste,
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
}

/// Pane shortcuts first (Ctrl+Shift+C/V or Cmd+C/V, Shift+Insert, Shift+PageUp/PageDown/
/// Home/End for the scrollback), else the bytes for the program. `text` is what the key typed.
pub fn key_action(key: &Key, modifiers: Modifiers, text: Option<&str>, mode: TermMode) -> Option<KeyAction> {
    let clip = if cfg!(target_os = "macos") { modifiers.logo() && !modifiers.control() } else { modifiers.control() && modifiers.shift() && !modifiers.alt() };
    let is = |c: &str| matches!(key, Key::Character(k) if k.eq_ignore_ascii_case(c));
    if clip && is("c") { return Some(KeyAction::Copy) }
    if clip && is("v") { return Some(KeyAction::Paste) }
    if modifiers.shift() && !modifiers.control() && !modifiers.alt() {
        let action = match key {
            Key::Named(Named::Insert) => Some(KeyAction::Paste),
            Key::Named(Named::PageUp) => Some(KeyAction::ScrollPageUp),
            Key::Named(Named::PageDown) => Some(KeyAction::ScrollPageDown),
            Key::Named(Named::Home) => Some(KeyAction::ScrollTop),
            Key::Named(Named::End) => Some(KeyAction::ScrollBottom),
            _ => None,
        };
        if action.is_some() { return action }
    }
    encode_key(key, modifiers, text, mode).map(KeyAction::Send)
}

/// The xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
fn param(m: Modifiers) -> u8 { 1 + u8::from(m.shift()) + 2 * u8::from(m.alt()) + 4 * u8::from(m.control()) }

/// `ESC [ x` (or `ESC O x` with `ss3`), `ESC [ 1 ; m x` with modifiers.
fn letter_key(letter: u8, m: Modifiers, ss3: bool) -> Cow<'static, [u8]> {
    match param(m) {
        1 => Cow::Owned(vec![0x1b, if ss3 { b'O' } else { b'[' }, letter]),
        p => Cow::Owned(format!("\x1b[1;{p}{}", letter as char).into_bytes()),
    }
}

/// `ESC [ n ~`, `ESC [ n ; m ~` with modifiers.
fn tilde_key(n: u8, m: Modifiers) -> Cow<'static, [u8]> {
    Cow::Owned(match param(m) { 1 => format!("\x1b[{n}~"), p => format!("\x1b[{n};{p}~") }.into_bytes())
}

fn with_alt(bytes: Cow<'static, [u8]>, m: Modifiers) -> Cow<'static, [u8]> {
    if m.alt() { Cow::Owned([&[0x1b][..], &bytes].concat()) } else { bytes }
}

fn control_byte(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// The bytes of a key press, or `None` for keys that send nothing.
pub fn encode_key(key: &Key, m: Modifiers, text: Option<&str>, mode: TermMode) -> Option<Cow<'static, [u8]>> {
    let fixed = |b: &'static [u8]| Some(with_alt(Cow::Borrowed(b), m));
    let app = mode.contains(TermMode::APP_CURSOR);
    let text = text.filter(|t| !t.is_empty());
    match key {
        Key::Named(named) => match named {
            Named::Enter => fixed(b"\r"),
            Named::Backspace => fixed(if m.control() { b"\x08" } else { b"\x7f" }),
            Named::Tab if m.shift() => Some(Cow::Borrowed(b"\x1b[Z")),
            Named::Tab => fixed(b"\t"),
            Named::Escape => fixed(b"\x1b"),
            Named::Space => fixed(if m.control() { b"\0" } else { b" " }),
            Named::ArrowUp => Some(letter_key(b'A', m, app)),
            Named::ArrowDown => Some(letter_key(b'B', m, app)),
            Named::ArrowRight => Some(letter_key(b'C', m, app)),
            Named::ArrowLeft => Some(letter_key(b'D', m, app)),
            Named::Home => Some(letter_key(b'H', m, app)),
            Named::End => Some(letter_key(b'F', m, app)),
            Named::F1 => Some(letter_key(b'P', m, true)),
            Named::F2 => Some(letter_key(b'Q', m, true)),
            Named::F3 => Some(letter_key(b'R', m, true)),
            Named::F4 => Some(letter_key(b'S', m, true)),
            n => {
                let codes = [(Named::Insert, 2), (Named::Delete, 3), (Named::PageUp, 5), (Named::PageDown, 6), (Named::F5, 15), (Named::F6, 17), (Named::F7, 18), (Named::F8, 19), (Named::F9, 20), (Named::F10, 21), (Named::F11, 23), (Named::F12, 24)];
                codes.iter().find(|(k, _)| k == n).map(|&(_, code)| tilde_key(code, m))
            }
        },
        Key::Character(c) => {
            let mut chars = c.chars();
            if let (true, Some(first), None) = (m.control(), chars.next(), chars.next())
                && let Some(byte) = control_byte(first)
            {
                return Some(with_alt(Cow::Owned(vec![byte]), m));
            }
            Some(with_alt(Cow::Owned(text.unwrap_or(c.as_str()).as_bytes().to_vec()), m))
        }
        Key::Unidentified => text.map(|t| with_alt(Cow::Owned(t.as_bytes().to_vec()), m)),
    }
}

/// Pasted text: inside `ESC [ 200 ~`/`ESC [ 201 ~` with escape bytes removed when bracketed
/// paste is on, else with line feeds turned into the carriage returns Enter sends.
pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        return [b"\x1b[200~".to_vec(), text.bytes().filter(|&b| b != 0x1b).collect(), b"\x1b[201~".to_vec()].concat();
    }
    text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_encode_per_xterm() {
        let (none, app) = (TermMode::empty(), TermMode::APP_CURSOR);
        let (ctrl, shift, alt, no) = (Modifiers::CTRL, Modifiers::SHIFT, Modifiers::ALT, Modifiers::empty());
        let ch = |c: &str| Key::Character(c.into());
        let named = Key::Named;
        let clip = if cfg!(target_os = "macos") { Modifiers::LOGO } else { Modifiers::CTRL | Modifiers::SHIFT };
        let send = |b: &[u8]| Some(KeyAction::Send(Cow::Owned(b.to_vec())));
        let cases = [
            (ch("a"), no, Some("a"), none, send(b"a")),
            (ch("a"), shift, Some("A"), none, send(b"A")),
            (ch("c"), ctrl, None, none, send(&[0x03])),
            (ch("["), ctrl, None, none, send(&[0x1b])),
            (ch("b"), alt, Some("b"), none, send(b"\x1bb")),
            (ch("é"), no, Some("é"), none, send("é".as_bytes())),
            (named(Named::Enter), no, None, none, send(b"\r")),
            (named(Named::Backspace), no, None, none, send(b"\x7f")),
            (named(Named::Tab), shift, None, none, send(b"\x1b[Z")),
            (named(Named::ArrowUp), no, None, none, send(b"\x1b[A")),
            (named(Named::ArrowUp), no, None, app, send(b"\x1bOA")),
            (named(Named::ArrowUp), ctrl, None, app, send(b"\x1b[1;5A")),
            (named(Named::Delete), no, None, none, send(b"\x1b[3~")),
            (named(Named::F5), shift, None, none, send(b"\x1b[15;2~")),
            (ch("C"), clip, None, none, Some(KeyAction::Copy)),
            (ch("V"), clip, None, none, Some(KeyAction::Paste)),
            (named(Named::PageUp), shift, None, none, Some(KeyAction::ScrollPageUp)),
        ];
        for (key, m, text, mode, want) in cases { assert_eq!(key_action(&key, m, text, mode), want, "{key:?} {m:?}") }
        assert_eq!(encode_paste("a\nb\r\nc", none), b"a\rb\rc");
        assert_eq!(encode_paste("rm x\n\x1b[201~", TermMode::BRACKETED_PASTE), b"\x1b[200~rm x\n[201~\x1b[201~");
    }
}
