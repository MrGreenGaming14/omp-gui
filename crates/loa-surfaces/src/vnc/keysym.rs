//! W3C `KeyboardEvent.key` values to X11 keysyms, as RFB KeyEvents carry them.

pub(crate) const SHIFT_L: u32 = 0xffe1;
pub(crate) const CONTROL_L: u32 = 0xffe3;
pub(crate) const ALT_L: u32 = 0xffe9;
pub(crate) const SUPER_L: u32 = 0xffeb;
pub(crate) const RETURN: u32 = 0xff0d;
pub(crate) const TAB: u32 = 0xff09;
pub(crate) const BACKSPACE: u32 = 0xff08;

/// Named keys with no left or right variant.
const NAMED: &[(&str, u32)] = &[
    ("Backspace", BACKSPACE),
    ("Tab", TAB),
    ("Clear", 0xff0b),
    ("Enter", RETURN),
    ("Pause", 0xff13),
    ("ScrollLock", 0xff14),
    ("Escape", 0xff1b),
    ("Home", 0xff50),
    ("ArrowLeft", 0xff51),
    ("ArrowUp", 0xff52),
    ("ArrowRight", 0xff53),
    ("ArrowDown", 0xff54),
    ("PageUp", 0xff55),
    ("PageDown", 0xff56),
    ("End", 0xff57),
    ("Select", 0xff60),
    ("PrintScreen", 0xff61),
    ("Execute", 0xff62),
    ("Insert", 0xff63),
    ("Undo", 0xff65),
    ("Redo", 0xff66),
    ("ContextMenu", 0xff67),
    ("Find", 0xff68),
    ("Cancel", 0xff69),
    ("Help", 0xff6a),
    ("NumLock", 0xff7f),
    ("CapsLock", 0xffe5),
    ("AltGraph", 0xfe03),
    ("Delete", 0xffff),
];

/// Modifier keys: W3C key, left keysym, right keysym.
const MODIFIERS: &[(&str, u32, u32)] = &[
    ("Shift", SHIFT_L, 0xffe2),
    ("Control", CONTROL_L, 0xffe4),
    ("Alt", ALT_L, 0xffea),
    ("Meta", SUPER_L, 0xffec),
];

/// Keypad keys by their W3C `key` value, used when `code` names a numpad key.
const KEYPAD: &[(&str, u32)] = &[
    ("Enter", 0xff8d),
    ("*", 0xffaa),
    ("+", 0xffab),
    (",", 0xffac),
    ("-", 0xffad),
    (".", 0xffae),
    ("/", 0xffaf),
    ("=", 0xffbd),
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
    fn printable_characters() {
        assert_eq!(keysym("a", "KeyA"), Some(0x61));
        assert_eq!(keysym("A", "KeyA"), Some(0x41));
        assert_eq!(keysym("1", "Digit1"), Some(0x31));
        assert_eq!(keysym("!", "Digit1"), Some(0x21));
        assert_eq!(keysym(" ", "Space"), Some(0x20));
        assert_eq!(keysym("~", "Backquote"), Some(0x7e));
        assert_eq!(keysym("é", "KeyE"), Some(0xe9));
        assert_eq!(keysym("€", "KeyE"), Some(0x0100_20ac));
        assert_eq!(keysym("ж", "KeyX"), Some(0x0100_0436));
    }

    #[test]
    fn modifiers_follow_the_side_in_code() {
        assert_eq!(keysym("Shift", "ShiftLeft"), Some(0xffe1));
        assert_eq!(keysym("Shift", "ShiftRight"), Some(0xffe2));
        assert_eq!(keysym("Control", "ControlLeft"), Some(0xffe3));
        assert_eq!(keysym("Control", "ControlRight"), Some(0xffe4));
        assert_eq!(keysym("Alt", "AltLeft"), Some(0xffe9));
        assert_eq!(keysym("Alt", "AltRight"), Some(0xffea));
        assert_eq!(keysym("Meta", "MetaLeft"), Some(0xffeb));
        assert_eq!(keysym("Meta", "MetaRight"), Some(0xffec));
        assert_eq!(keysym("Shift", ""), Some(0xffe1));
        assert_eq!(keysym("CapsLock", "CapsLock"), Some(0xffe5));
        assert_eq!(keysym("AltGraph", "AltRight"), Some(0xfe03));
    }

    #[test]
    fn editing_and_navigation_keys() {
        let cases = [
            ("Backspace", 0xff08),
            ("Tab", 0xff09),
            ("Enter", 0xff0d),
            ("Escape", 0xff1b),
            ("Delete", 0xffff),
            ("Insert", 0xff63),
            ("Home", 0xff50),
            ("End", 0xff57),
            ("PageUp", 0xff55),
            ("PageDown", 0xff56),
            ("ArrowLeft", 0xff51),
            ("ArrowUp", 0xff52),
            ("ArrowRight", 0xff53),
            ("ArrowDown", 0xff54),
            ("PrintScreen", 0xff61),
            ("ContextMenu", 0xff67),
        ];
        for (key, sym) in cases {
            assert_eq!(keysym(key, ""), Some(sym), "{key}");
        }
    }

    #[test]
    fn function_keys() {
        assert_eq!(keysym("F1", "F1"), Some(0xffbe));
        assert_eq!(keysym("F5", "F5"), Some(0xffc2));
        assert_eq!(keysym("F12", "F12"), Some(0xffc9));
        assert_eq!(keysym("F24", "F24"), Some(0xffd5));
        assert_eq!(keysym("F35", "F35"), Some(0xffe0));
        assert_eq!(keysym("F36", "F36"), None);
        assert_eq!(keysym("F0", "F0"), None);
        assert_eq!(keysym("Fn", "Fn"), None);
    }

    #[test]
    fn numpad_keys_use_keypad_keysyms() {
        assert_eq!(keysym("7", "Numpad7"), Some(0xffb7));
        assert_eq!(keysym("Enter", "NumpadEnter"), Some(0xff8d));
        assert_eq!(keysym("+", "NumpadAdd"), Some(0xffab));
        assert_eq!(keysym(".", "NumpadDecimal"), Some(0xffae));
        // Num Lock off: the numpad key reports a navigation key.
        assert_eq!(keysym("Home", "Numpad7"), Some(0xff50));
    }

    #[test]
    fn keys_without_keysym() {
        assert_eq!(keysym("Unidentified", ""), None);
        assert_eq!(keysym("Dead", "Quote"), None);
        assert_eq!(keysym("", ""), None);
    }

    #[test]
    fn text_control_characters() {
        assert_eq!(char_keysym('\n'), Some(0xff0d));
        assert_eq!(char_keysym('\t'), Some(0xff09));
        assert_eq!(char_keysym('\u{7}'), None);
    }
}
