//! Keyboard and paste input encoded as the bytes a terminal program expects
//! (xterm conventions).

use std::borrow::Cow;

use alacritty_terminal::term::TermMode;
use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};

/// What a key press means to the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// Send these bytes to the program.
    Send(Cow<'static, [u8]>),
    /// Copy the selection to the clipboard.
    Copy,
    /// Paste the clipboard into the program.
    Paste,
    /// Scroll the view through the scrollback.
    ScrollPageUp,
    /// Scroll the view through the scrollback.
    ScrollPageDown,
    /// Jump to the oldest scrollback line.
    ScrollTop,
    /// Jump back to the live screen.
    ScrollBottom,
}

/// Maps a key press to what the pane should do. `text` is the text the key
/// produced with modifiers applied, as reported by iced.
pub fn key_action(
    key: &Key,
    modifiers: Modifiers,
    text: Option<&str>,
    mode: TermMode,
) -> Option<KeyAction> {
    if let Some(action) = shortcut(key, modifiers) {
        return Some(action);
    }
    encode_key(key, modifiers, text, mode).map(KeyAction::Send)
}

fn is_char(key: &Key, wanted: &str) -> bool {
    matches!(key, Key::Character(c) if c.eq_ignore_ascii_case(wanted))
}

/// Pane shortcuts, taken before anything reaches the program: Ctrl+Shift+C
/// and Ctrl+Shift+V (Cmd+C and Cmd+V on macOS), Shift+Insert, and Shift with
/// Page Up, Page Down, Home and End for the scrollback.
fn shortcut(key: &Key, modifiers: Modifiers) -> Option<KeyAction> {
    let clipboard_chord = if cfg!(target_os = "macos") {
        modifiers.logo() && !modifiers.control()
    } else {
        modifiers.control() && modifiers.shift() && !modifiers.alt()
    };
    if clipboard_chord && is_char(key, "c") {
        return Some(KeyAction::Copy);
    }
    if clipboard_chord && is_char(key, "v") {
        return Some(KeyAction::Paste);
    }
    let shift_only = modifiers.shift() && !modifiers.control() && !modifiers.alt();
    if !shift_only {
        return None;
    }
    match key {
        Key::Named(Named::Insert) => Some(KeyAction::Paste),
        Key::Named(Named::PageUp) => Some(KeyAction::ScrollPageUp),
        Key::Named(Named::PageDown) => Some(KeyAction::ScrollPageDown),
        Key::Named(Named::Home) => Some(KeyAction::ScrollTop),
        Key::Named(Named::End) => Some(KeyAction::ScrollBottom),
        _ => None,
    }
}

/// xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
fn modifier_param(modifiers: Modifiers) -> u8 {
    1 + u8::from(modifiers.shift())
        + 2 * u8::from(modifiers.alt())
        + 4 * u8::from(modifiers.control())
}

/// Cursor keys: `ESC [ x`, `ESC O x` in application cursor mode, and
/// `ESC [ 1 ; m x` with modifiers.
fn cursor_key(letter: u8, modifiers: Modifiers, mode: TermMode) -> Cow<'static, [u8]> {
    let param = modifier_param(modifiers);
    if param > 1 {
        return Cow::Owned(format!("\x1b[1;{param}{}", letter as char).into_bytes());
    }
    let prefix = if mode.contains(TermMode::APP_CURSOR) { b'O' } else { b'[' };
    Cow::Owned(vec![0x1b, prefix, letter])
}

/// Keys of the form `ESC [ n ~`, with `ESC [ n ; m ~` for modifiers.
fn tilde_key(number: u8, modifiers: Modifiers) -> Cow<'static, [u8]> {
    let param = modifier_param(modifiers);
    let text = if param > 1 {
        format!("\x1b[{number};{param}~")
    } else {
        format!("\x1b[{number}~")
    };
    Cow::Owned(text.into_bytes())
}

fn with_alt(bytes: Cow<'static, [u8]>, modifiers: Modifiers) -> Cow<'static, [u8]> {
    if !modifiers.alt() {
        return bytes;
    }
    let mut prefixed = Vec::with_capacity(bytes.len() + 1);
    prefixed.push(0x1b);
    prefixed.extend_from_slice(&bytes);
    Cow::Owned(prefixed)
}

/// The control byte for Ctrl plus a character, if there is one.
fn control_byte(c: char) -> Option<u8> {
    match c.to_ascii_lowercase() {
        c @ 'a'..='z' => Some(c as u8 - b'a' + 1),
        '@' | ' ' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// Encodes a key press for the program, or `None` for keys that produce
/// nothing (bare modifiers, unknown keys).
pub fn encode_key(
    key: &Key,
    modifiers: Modifiers,
    text: Option<&str>,
    mode: TermMode,
) -> Option<Cow<'static, [u8]>> {
    let fixed = |bytes: &'static [u8]| Some(with_alt(Cow::Borrowed(bytes), modifiers));
    match key {
        Key::Named(named) => match named {
            Named::Enter => fixed(b"\r"),
            Named::Backspace if modifiers.control() => fixed(b"\x08"),
            Named::Backspace => fixed(b"\x7f"),
            Named::Tab if modifiers.shift() => Some(Cow::Borrowed(b"\x1b[Z")),
            Named::Tab => fixed(b"\t"),
            Named::Escape => fixed(b"\x1b"),
            Named::Space if modifiers.control() => fixed(b"\0"),
            Named::Space => fixed(b" "),
            Named::ArrowUp => Some(cursor_key(b'A', modifiers, mode)),
            Named::ArrowDown => Some(cursor_key(b'B', modifiers, mode)),
            Named::ArrowRight => Some(cursor_key(b'C', modifiers, mode)),
            Named::ArrowLeft => Some(cursor_key(b'D', modifiers, mode)),
            Named::Home => Some(cursor_key(b'H', modifiers, mode)),
            Named::End => Some(cursor_key(b'F', modifiers, mode)),
            Named::Insert => Some(tilde_key(2, modifiers)),
            Named::Delete => Some(tilde_key(3, modifiers)),
            Named::PageUp => Some(tilde_key(5, modifiers)),
            Named::PageDown => Some(tilde_key(6, modifiers)),
            Named::F1 => Some(cursor_key_ss3(b'P', modifiers)),
            Named::F2 => Some(cursor_key_ss3(b'Q', modifiers)),
            Named::F3 => Some(cursor_key_ss3(b'R', modifiers)),
            Named::F4 => Some(cursor_key_ss3(b'S', modifiers)),
            Named::F5 => Some(tilde_key(15, modifiers)),
            Named::F6 => Some(tilde_key(17, modifiers)),
            Named::F7 => Some(tilde_key(18, modifiers)),
            Named::F8 => Some(tilde_key(19, modifiers)),
            Named::F9 => Some(tilde_key(20, modifiers)),
            Named::F10 => Some(tilde_key(21, modifiers)),
            Named::F11 => Some(tilde_key(23, modifiers)),
            Named::F12 => Some(tilde_key(24, modifiers)),
            _ => None,
        },
        Key::Character(c) => {
            if modifiers.control() {
                let mut chars = c.chars();
                if let (Some(first), None) = (chars.next(), chars.next())
                    && let Some(byte) = control_byte(first)
                {
                    return Some(with_alt(Cow::Owned(vec![byte]), modifiers));
                }
            }
            let text = text.filter(|text| !text.is_empty()).unwrap_or(c.as_str());
            Some(with_alt(Cow::Owned(text.as_bytes().to_vec()), modifiers))
        }
        Key::Unidentified => text
            .filter(|text| !text.is_empty())
            .map(|text| with_alt(Cow::Owned(text.as_bytes().to_vec()), modifiers)),
    }
}

/// F1 to F4: `ESC O x`, `ESC [ 1 ; m x` with modifiers.
fn cursor_key_ss3(letter: u8, modifiers: Modifiers) -> Cow<'static, [u8]> {
    let param = modifier_param(modifiers);
    if param > 1 {
        return Cow::Owned(format!("\x1b[1;{param}{}", letter as char).into_bytes());
    }
    Cow::Owned(vec![0x1b, b'O', letter])
}

/// Encodes pasted text. With bracketed paste on, the text is wrapped in
/// `ESC [ 200 ~` / `ESC [ 201 ~` and any escape bytes are removed so the
/// paste cannot end the bracket early. Without it, line feeds become carriage
/// returns, which is what Enter sends.
pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let mut out = Vec::with_capacity(text.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend(text.bytes().filter(|&byte| byte != 0x1b));
        out.extend_from_slice(b"\x1b[201~");
        return out;
    }
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            b'\r' if bytes.peek() == Some(&b'\n') => {
                bytes.next();
                out.push(b'\r');
            }
            b'\n' => out.push(b'\r'),
            byte => out.push(byte),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    fn send(key: Key, modifiers: Modifiers, text: Option<&str>, mode: TermMode) -> Vec<u8> {
        match key_action(&key, modifiers, text, mode) {
            Some(KeyAction::Send(bytes)) => bytes.into_owned(),
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    #[test]
    fn typing_and_control_characters() {
        let none = TermMode::empty();
        assert_eq!(send(ch("a"), Modifiers::empty(), Some("a"), none), b"a");
        assert_eq!(send(ch("a"), Modifiers::SHIFT, Some("A"), none), b"A");
        assert_eq!(send(ch("c"), Modifiers::CTRL, None, none), [0x03]);
        assert_eq!(send(ch("d"), Modifiers::CTRL, None, none), [0x04]);
        assert_eq!(send(ch("["), Modifiers::CTRL, None, none), [0x1b]);
        assert_eq!(send(ch("b"), Modifiers::ALT, Some("b"), none), b"\x1bb");
        assert_eq!(send(ch("é"), Modifiers::empty(), Some("é"), none), "é".as_bytes());
        assert_eq!(send(Key::Named(Named::Enter), Modifiers::empty(), None, none), b"\r");
        assert_eq!(send(Key::Named(Named::Backspace), Modifiers::empty(), None, none), b"\x7f");
        assert_eq!(send(Key::Named(Named::Tab), Modifiers::SHIFT, None, none), b"\x1b[Z");
    }

    #[test]
    fn arrows_follow_application_cursor_mode() {
        let up = Key::Named(Named::ArrowUp);
        assert_eq!(send(up.clone(), Modifiers::empty(), None, TermMode::empty()), b"\x1b[A");
        assert_eq!(send(up.clone(), Modifiers::empty(), None, TermMode::APP_CURSOR), b"\x1bOA");
        assert_eq!(send(up, Modifiers::CTRL, None, TermMode::APP_CURSOR), b"\x1b[1;5A");
        assert_eq!(
            send(Key::Named(Named::Delete), Modifiers::empty(), None, TermMode::empty()),
            b"\x1b[3~"
        );
        assert_eq!(
            send(Key::Named(Named::F5), Modifiers::SHIFT, None, TermMode::empty()),
            b"\x1b[15;2~"
        );
    }

    #[test]
    fn clipboard_and_scrollback_shortcuts_do_not_reach_the_program() {
        let none = TermMode::empty();
        let chord = if cfg!(target_os = "macos") {
            Modifiers::LOGO
        } else {
            Modifiers::CTRL | Modifiers::SHIFT
        };
        assert_eq!(key_action(&ch("C"), chord, None, none), Some(KeyAction::Copy));
        assert_eq!(key_action(&ch("V"), chord, None, none), Some(KeyAction::Paste));
        assert_eq!(
            key_action(&Key::Named(Named::PageUp), Modifiers::SHIFT, None, none),
            Some(KeyAction::ScrollPageUp)
        );
        // Plain Page Up still goes to the program (less, vim).
        assert_eq!(
            send(Key::Named(Named::PageUp), Modifiers::empty(), None, none),
            b"\x1b[5~"
        );
    }

    #[test]
    fn paste_converts_newlines_or_brackets() {
        assert_eq!(encode_paste("a\nb\r\nc", TermMode::empty()), b"a\rb\rc");
        assert_eq!(
            encode_paste("rm -rf x\n\x1b[201~", TermMode::BRACKETED_PASTE),
            b"\x1b[200~rm -rf x\n[201~\x1b[201~"
        );
    }
}
