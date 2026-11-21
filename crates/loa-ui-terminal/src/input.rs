//! Key presses and pastes encoded as the bytes a terminal program expects (xterm conventions).

use alacritty_terminal::{grid::Scroll, term::TermMode};
use bytes::Bytes;
use iced::keyboard::{Key, Modifiers, key::Named};

/// What a key press means to the pane.
#[derive(Debug, Clone)]
pub enum KeyAction { Send(Bytes), Copy, Paste, Scroll(Scroll) }

/// A named key's encoding: fixed bytes without and with Ctrl, `ESC [ x` (`ESC O x` in application
/// cursor mode), `ESC O x`, or `ESC [ n ~`; modifiers turn the last three into `ESC [ 1 ; m x` / `ESC [ n ; m ~`.
enum Seq { Text(&'static [u8], &'static [u8]), Cursor(u8), Ss3(u8), Tilde(u8) }

const NAMED: &[(Named, Seq)] = {
    use Named::*;
    use Seq::*;
    &[
        (Enter, Text(b"\r", b"\r")), (Backspace, Text(b"\x7f", b"\x08")), (Tab, Text(b"\t", b"\t")), (Escape, Text(b"\x1b", b"\x1b")), (Space, Text(b" ", b"\0")),
        (ArrowUp, Cursor(b'A')), (ArrowDown, Cursor(b'B')), (ArrowRight, Cursor(b'C')), (ArrowLeft, Cursor(b'D')), (Home, Cursor(b'H')), (End, Cursor(b'F')),
        (F1, Ss3(b'P')), (F2, Ss3(b'Q')), (F3, Ss3(b'R')), (F4, Ss3(b'S')),
        (Insert, Tilde(2)), (Delete, Tilde(3)), (PageUp, Tilde(5)), (PageDown, Tilde(6)), (F5, Tilde(15)), (F6, Tilde(17)), (F7, Tilde(18)), (F8, Tilde(19)),
        (F9, Tilde(20)), (F10, Tilde(21)), (F11, Tilde(23)), (F12, Tilde(24)),
    ]
};

/// Shift plus these keys act on the pane: paste and the scrollback.
const SHIFTED: &[(Named, KeyAction)] = {
    use KeyAction::Scroll as S;
    &[(Named::Insert, KeyAction::Paste), (Named::PageUp, S(Scroll::PageUp)), (Named::PageDown, S(Scroll::PageDown)), (Named::Home, S(Scroll::Top)), (Named::End, S(Scroll::Bottom))]
};

/// Ctrl plus these characters send the byte; Ctrl plus a letter sends its alphabet index.
const CONTROL: &[(&str, u8)] = &[("@ 2", 0), ("[3", 0x1b), ("\\4", 0x1c), ("]5", 0x1d), ("^6", 0x1e), ("_-7", 0x1f), ("?8", 0x7f)];

/// Pane shortcuts first (Ctrl+Shift+C/V or Cmd+C/V, then [`SHIFTED`]), else the bytes for the
/// program. `text` is what the key typed.
pub fn key_action(key: &Key, m: Modifiers, text: Option<&str>, mode: TermMode) -> Option<KeyAction> {
    let clip = if cfg!(target_os = "macos") { m.logo() && !m.control() } else { m.control() && m.shift() && !m.alt() };
    match key {
        Key::Character(c) if clip && c.eq_ignore_ascii_case("c") => return Some(KeyAction::Copy),
        Key::Character(c) if clip && c.eq_ignore_ascii_case("v") => return Some(KeyAction::Paste),
        Key::Named(n) if m.shift() && !m.control() && !m.alt() => {
            if let Some((_, action)) = SHIFTED.iter().find(|(k, _)| k == n) { return Some(action.clone()) }
        }
        _ => {}
    }
    let text = text.filter(|t| !t.is_empty());
    // The xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    let param = 1 + u8::from(m.shift()) + 2 * u8::from(m.alt()) + 4 * u8::from(m.control());
    let letter = |ss3: bool, x: u8| if param == 1 { Bytes::from(vec![0x1b, if ss3 { b'O' } else { b'[' }, x]) } else { Bytes::from(format!("\x1b[1;{param}{}", x as char)) };
    let alt = |b: Bytes| if m.alt() { Bytes::from([&b"\x1b"[..], &b].concat()) } else { b };
    let bytes = match key {
        Key::Named(Named::Tab) if m.shift() => Bytes::from_static(b"\x1b[Z"),
        Key::Named(named) => match NAMED.iter().find(|(k, _)| k == named)?.1 {
            Seq::Text(plain, ctrl) => alt(Bytes::from_static(if m.control() { ctrl } else { plain })),
            Seq::Cursor(x) => letter(mode.contains(TermMode::APP_CURSOR), x),
            Seq::Ss3(x) => letter(true, x),
            Seq::Tilde(n) if param == 1 => Bytes::from(format!("\x1b[{n}~")),
            Seq::Tilde(n) => Bytes::from(format!("\x1b[{n};{param}~")),
        },
        Key::Character(c) => {
            let control = match c.to_ascii_lowercase().as_bytes() {
                [b @ b'a'..=b'z'] => Some(b - b'a' + 1),
                [b] => CONTROL.iter().find(|(keys, _)| keys.as_bytes().contains(b)).map(|&(_, byte)| byte),
                _ => None,
            };
            alt(control.filter(|_| m.control()).map_or_else(|| Bytes::from(text.unwrap_or(c.as_str()).to_owned()), |b| Bytes::from(vec![b])))
        }
        Key::Unidentified => alt(Bytes::from(text?.to_owned())),
    };
    Some(KeyAction::Send(bytes))
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
#[test]
fn keys_encode_per_xterm() {
    let (none, app, no, ctrl, shift) = (TermMode::empty(), TermMode::APP_CURSOR, Modifiers::empty(), Modifiers::CTRL, Modifiers::SHIFT);
    let clip = if cfg!(target_os = "macos") { Modifiers::LOGO } else { Modifiers::CTRL | Modifiers::SHIFT };
    let (ch, named, send) = (|c: &str| Key::Character(c.into()), Key::Named, |b: &[u8]| KeyAction::Send(Bytes::copy_from_slice(b)));
    let cases = [
        (ch("c"), ctrl, None, none, send(&[0x03])),
        (ch("["), ctrl, None, none, send(&[0x1b])),
        (ch("b"), Modifiers::ALT, Some("b"), none, send(b"\x1bb")),
        (ch("é"), no, Some("é"), none, send("é".as_bytes())),
        (named(Named::Backspace), no, None, none, send(b"\x7f")),
        (named(Named::Tab), shift, None, none, send(b"\x1b[Z")),
        (named(Named::ArrowUp), no, None, app, send(b"\x1bOA")),
        (named(Named::ArrowUp), ctrl, None, app, send(b"\x1b[1;5A")),
        (named(Named::F5), shift, None, none, send(b"\x1b[15;2~")),
        (ch("C"), clip, None, none, KeyAction::Copy),
        (named(Named::PageUp), shift, None, none, KeyAction::Scroll(Scroll::PageUp)),
    ];
    for (key, m, text, mode, want) in cases { assert_eq!(format!("{:?}", key_action(&key, m, text, mode)), format!("{:?}", Some(want)), "{key:?} {m:?}") }
}
