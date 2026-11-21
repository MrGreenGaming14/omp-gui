//! W3C `KeyboardEvent.key` values to X11 keysyms, as RFB KeyEvents carry them.

pub(crate) const SHIFT_L: u32 = 0xffe1;
pub(crate) const CONTROL_L: u32 = 0xffe3;
pub(crate) const ALT_L: u32 = 0xffe9;
pub(crate) const SUPER_L: u32 = 0xffeb;
pub(crate) const RETURN: u32 = 0xff0d;
pub(crate) const TAB: u32 = 0xff09;
pub(crate) const BACKSPACE: u32 = 0xff08;

/// Named keys with no left or right variant.
#[rustfmt::skip]
const NAMED: &[(&str, u32)] = &[
    ("Backspace", BACKSPACE), ("Tab", TAB), ("Clear", 0xff0b), ("Enter", RETURN), ("Pause", 0xff13),
    ("ScrollLock", 0xff14), ("Escape", 0xff1b), ("Home", 0xff50), ("ArrowLeft", 0xff51), ("ArrowUp", 0xff52),
    ("ArrowRight", 0xff53), ("ArrowDown", 0xff54), ("PageUp", 0xff55), ("PageDown", 0xff56), ("End", 0xff57),
    ("Select", 0xff60), ("PrintScreen", 0xff61), ("Execute", 0xff62), ("Insert", 0xff63), ("Undo", 0xff65),
    ("Redo", 0xff66), ("ContextMenu", 0xff67), ("Find", 0xff68), ("Cancel", 0xff69), ("Help", 0xff6a),
    ("NumLock", 0xff7f), ("CapsLock", 0xffe5), ("AltGraph", 0xfe03), ("Delete", 0xffff),
];

/// Modifier keys: W3C key, left keysym, right keysym.
const MODIFIERS: &[(&str, u32, u32)] = &[
    ("Shift", SHIFT_L, 0xffe2),
    ("Control", CONTROL_L, 0xffe4),
    ("Alt", ALT_L, 0xffea),
    ("Meta", SUPER_L, 0xffec),
];

/// Keypad keys by their W3C `key` value, used when `code` names a numpad key.
#[rustfmt::skip]
const KEYPAD: &[(&str, u32)] = &[
    ("Enter", 0xff8d), ("*", 0xffaa), ("+", 0xffab), (",", 0xffac), ("-", 0xffad), (".", 0xffae), ("/", 0xffaf), ("=", 0xffbd),
];

/// Keysym of the first function key, F1. F2 to F35 follow it.
const F1: u32 = 0xffbe;

/// The keysym for a key event, or `None` when the key has no keysym
/// (`Unidentified`, `Dead`, unknown names).
pub(crate) fn keysym(key: &str, code: &str) -> Option<u32> {
    if code.starts_with("Numpad") {
        if let Some(&(_, sym)) = KEYPAD.iter().find(|(k, _)| *k == key) {
            return Some(sym);
        }
        if let [digit @ b'0'..=b'9'] = key.as_bytes() {
            return Some(0xffb0 + u32::from(digit - b'0'));
        }
    }
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return char_keysym(c);
    }
    if let Some(&(_, sym)) = NAMED.iter().find(|(k, _)| *k == key) {
        return Some(sym);
    }
    if let Some(&(_, left, right)) = MODIFIERS.iter().find(|(k, _, _)| *k == key) {
        return Some(if code.ends_with("Right") { right } else { left });
    }
    let n: u32 = key.strip_prefix('F')?.parse().ok()?;
    (1..=35).contains(&n).then(|| F1 + n - 1)
}

/// The keysym typing `c` produces. Latin-1 characters map to themselves,
/// other characters to the Unicode keysym range, and the control characters
/// a text field can hold to their editing keys.
pub(crate) fn char_keysym(c: char) -> Option<u32> {
    let cp = u32::from(c);
    match c {
        '\n' | '\r' => Some(RETURN),
        '\t' => Some(TAB),
        '\u{8}' => Some(BACKSPACE),
        ' '..='~' | '\u{a0}'..='\u{ff}' => Some(cp),
        _ if c.is_control() => None,
        _ => Some(0x0100_0000 + cp),
    }
}

/// Whether `key` is one of the four modifiers [`crate::vnc::Modifiers`] tracks.
pub(crate) fn is_modifier(key: &str) -> bool {
    MODIFIERS.iter().any(|(k, _, _)| *k == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysym_table() {
        let cases: &[(&str, &str, Option<u32>)] = &[
            ("a", "KeyA", Some(0x61)), ("A", "KeyA", Some(0x41)), ("!", "Digit1", Some(0x21)), (" ", "Space", Some(0x20)),
            ("~", "Backquote", Some(0x7e)), ("é", "KeyE", Some(0xe9)), ("€", "KeyE", Some(0x0100_20ac)), ("ж", "KeyX", Some(0x0100_0436)),
            ("Shift", "ShiftLeft", Some(0xffe1)), ("Shift", "ShiftRight", Some(0xffe2)), ("Control", "ControlRight", Some(0xffe4)),
            ("Alt", "AltLeft", Some(0xffe9)), ("Meta", "MetaRight", Some(0xffec)), ("Shift", "", Some(0xffe1)),
            ("CapsLock", "CapsLock", Some(0xffe5)), ("AltGraph", "AltRight", Some(0xfe03)),
            ("Backspace", "", Some(0xff08)), ("Tab", "", Some(0xff09)), ("Enter", "", Some(0xff0d)), ("Escape", "", Some(0xff1b)),
            ("Delete", "", Some(0xffff)), ("Insert", "", Some(0xff63)), ("Home", "", Some(0xff50)), ("End", "", Some(0xff57)),
            ("PageUp", "", Some(0xff55)), ("PageDown", "", Some(0xff56)), ("ArrowLeft", "", Some(0xff51)), ("ArrowDown", "", Some(0xff54)),
            ("PrintScreen", "", Some(0xff61)), ("ContextMenu", "", Some(0xff67)),
            ("F1", "F1", Some(0xffbe)), ("F12", "F12", Some(0xffc9)), ("F35", "F35", Some(0xffe0)), ("F36", "F36", None), ("F0", "F0", None), ("Fn", "Fn", None),
            ("7", "Numpad7", Some(0xffb7)), ("Enter", "NumpadEnter", Some(0xff8d)), ("+", "NumpadAdd", Some(0xffab)), (".", "NumpadDecimal", Some(0xffae)),
            // Num Lock off: the numpad key reports a navigation key.
            ("Home", "Numpad7", Some(0xff50)),
            ("Unidentified", "", None), ("Dead", "Quote", None), ("", "", None),
        ];
        for &(key, code, sym) in cases {
            assert_eq!(keysym(key, code), sym, "{key} {code}");
        }
        assert_eq!([char_keysym('\n'), char_keysym('\t'), char_keysym('\u{7}')], [Some(0xff0d), Some(0xff09), None]);
    }
}

