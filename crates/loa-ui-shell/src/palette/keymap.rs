//! Keyboard shortcuts as data: chords bound to [`CommandId`]s per [`Scope`], defaults in one table.

use super::{CommandId, ids};
use iced::keyboard::{self, Modifiers, key::Named};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform { MacOs, Other }

impl Platform {
    pub const fn current() -> Self { if cfg!(target_os = "macos") { Self::MacOs } else { Self::Other } }
    /// Cmd on macOS, Ctrl elsewhere.
    pub const fn primary(self) -> Modifiers { if let Self::MacOs = self { Modifiers::LOGO } else { Modifiers::CTRL } }
}

/// The non-modifier key of a chord; characters are lowercase and latin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordKey { Char(char), Named(Named) }

/// A key pressed with exactly these modifiers (only Ctrl, Alt, Shift and Logo count).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord { pub key: ChordKey, pub modifiers: Modifiers }

/// Modifier labels as `(modifier, word, macOS glyph)`, in display order.
const MODIFIERS: [(Modifiers, &str, &str); 4] = [(Modifiers::CTRL, "Ctrl+", "⌃"), (Modifiers::ALT, "Alt+", "⌥"), (Modifiers::SHIFT, "Shift+", "⇧"), (Modifiers::LOGO, "Super+", "⌘")];

/// Labels of named keys as `(key, word, macOS glyph)`; others use iced's name (`F5`, `Insert`).
const NAMED: [(Named, &str, &str); 14] = [
    (Named::Escape, "Esc", "⎋"), (Named::Enter, "Enter", "↩"), (Named::Tab, "Tab", "⇥"), (Named::Space, "Space", "Space"), (Named::Backspace, "Backspace", "⌫"),
    (Named::Delete, "Delete", "⌦"), (Named::ArrowUp, "Up", "↑"), (Named::ArrowDown, "Down", "↓"), (Named::ArrowLeft, "Left", "←"), (Named::ArrowRight, "Right", "→"),
    (Named::PageUp, "PageUp", "⇞"), (Named::PageDown, "PageDown", "⇟"), (Named::Home, "Home", "↖"), (Named::End, "End", "↘"),
];

impl Chord {
    pub fn new(key: ChordKey, modifiers: Modifiers) -> Self {
        let key = match key {
            ChordKey::Char(c) => ChordKey::Char(match (c.to_lowercase().next(), c.to_lowercase().nth(1)) { (Some(l), None) => l, _ => c }),
            named => named,
        };
        Self { key, modifiers: modifiers.intersection(Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT | Modifiers::LOGO) }
    }
    pub fn char(c: char, modifiers: Modifiers) -> Self { Self::new(ChordKey::Char(c), modifiers) }
    pub fn named(key: Named, modifiers: Modifiers) -> Self { Self::new(ChordKey::Named(key), modifiers) }
    /// The chord of a key press, with non-latin characters mapped to the latin key at the same position.
    pub fn from_event(event: &keyboard::Event) -> Option<Self> {
        let keyboard::Event::KeyPressed { key, physical_key, modifiers, .. } = event else { return None };
        let k = match key {
            keyboard::Key::Named(Named::Shift | Named::Control | Named::Alt | Named::AltGraph | Named::Super | Named::Meta | Named::Hyper) | keyboard::Key::Unidentified => return None,
            keyboard::Key::Named(named) => ChordKey::Named(*named),
            keyboard::Key::Character(text) => ChordKey::Char(key.to_latin(*physical_key).or_else(|| text.chars().next())?),
        };
        Some(Self::new(k, *modifiers))
    }
    /// The hint shown next to a command: `Ctrl+K`, or `⌘K` on macOS.
    pub fn label(&self, platform: Platform) -> String {
        let pick = |word: &'static str, glyph: &'static str| if platform == Platform::MacOs { glyph } else { word };
        let mut out: String = MODIFIERS.iter().filter(|(m, ..)| self.modifiers.contains(*m)).map(|&(_, w, g)| pick(w, g)).collect();
        match self.key {
            ChordKey::Char(c) => out.extend(c.to_uppercase()),
            ChordKey::Named(named) => match NAMED.iter().find(|(n, ..)| *n == named) {
                Some(&(_, w, g)) => out.push_str(pick(w, g)),
                None => out.push_str(&format!("{named:?}")),
            },
        }
        out
    }
}

/// Where a binding applies: everywhere, in the composer (over global bindings), or in the modal palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope { Global, Composer, Palette }

/// Stands for the platform's primary modifier in [`DEFAULTS`].
const P: Modifiers = Modifiers::LOGO;
const NONE: Modifiers = Modifiers::empty();
const SHIFT: Modifiers = Modifiers::SHIFT;
const CTRL: Modifiers = Modifiers::CTRL;
use ChordKey::{Char as C, Named as N};
use Scope::{Composer, Global, Palette};

/// The default bindings; Ctrl/Cmd+1…9 and Ctrl/Cmd+Alt+1…9 are added in a loop.
const DEFAULTS: &[(ChordKey, Modifiers, Scope, CommandId)] = &[
    (N(Named::Escape), NONE, Global, ids::SESSION_INTERRUPT), (C('k'), P, Global, ids::PALETTE_TOGGLE), (C('k'), P, Palette, ids::PALETTE_TOGGLE),
    (C('/'), P, Global, ids::KEYMAP_SHOW), (C('b'), P, Global, ids::PANEL_TOGGLE), (C('a'), Modifiers::ALT, Global, ids::PANEL_AGENTS),
    (C('n'), P, Global, ids::SESSION_NEW),
    (N(Named::Tab), CTRL, Global, ids::SESSION_NEXT), (N(Named::Tab), CTRL.union(SHIFT), Global, ids::SESSION_PREVIOUS),
    (N(Named::PageDown), P, Global, ids::PROJECT_TAB_NEXT), (N(Named::PageUp), P, Global, ids::PROJECT_TAB_PREVIOUS),
    (C('l'), P, Global, ids::COMPOSER_FOCUS), (C('f'), P.union(SHIFT), Global, ids::SEARCH_FOCUS),
    (N(Named::Tab), NONE, Global, ids::FOCUS_NEXT), (N(Named::Tab), SHIFT, Global, ids::FOCUS_PREVIOUS),
    (N(Named::Enter), NONE, Composer, ids::COMPOSER_SUBMIT), (N(Named::Enter), SHIFT, Composer, ids::COMPOSER_NEWLINE),
];

#[derive(Debug, Clone)]
pub struct Keymap { platform: Platform, bindings: Vec<(Chord, Scope, CommandId)> }

impl Keymap {
    pub fn defaults(platform: Platform) -> Self {
        let primary = platform.primary();
        let bind = |key, m: Modifiers, scope, command| (Chord::new(key, if m.contains(P) { m.difference(P) | primary } else { m }), scope, command);
        let mut bindings: Vec<_> = DEFAULTS.iter().map(|(k, m, s, id)| bind(*k, *m, *s, id.clone())).collect();
        for n in 1..=9u8 {
            bindings.push(bind(C(char::from(b'0' + n)), P, Global, ids::session_goto(n.into())));
            bindings.push(bind(C(char::from(b'0' + n)), P | Modifiers::ALT, Global, ids::panel_tab(n.into())));
        }
        Self { platform, bindings }
    }
    pub fn platform(&self) -> Platform { self.platform }
    /// The command a key event runs in `scope`: its own binding, else (for the composer) the global one. Repeats run nothing.
    pub fn resolve(&self, event: &keyboard::Event, scope: Scope) -> Option<&CommandId> {
        if let keyboard::Event::KeyPressed { repeat: true, .. } = event { return None }
        let chord = Chord::from_event(event)?;
        let find = |s: Scope| self.bindings.iter().find(|(c, scope, _)| *scope == s && *c == chord).map(|(.., id)| id);
        find(scope).or_else(|| (scope == Composer).then(|| find(Global)).flatten())
    }
    /// The hint for `command`: its global chord, else its first scoped one.
    pub fn chord_for(&self, command: &CommandId) -> Option<Chord> {
        let mut bound = self.bindings.iter().filter(|(.., id)| id == command);
        let first = bound.clone().next()?;
        Some(bound.find(|(_, s, _)| *s == Global).unwrap_or(first).0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::Key;
    use iced::keyboard::key::{Code, Physical};

    #[test]
    fn defaults_resolve_per_platform_and_scope() {
        let press = |key: Key, code, modifiers, repeat| keyboard::Event::KeyPressed {
            modified_key: key.clone(), key, physical_key: Physical::Code(code), location: keyboard::Location::Standard, modifiers, text: None, repeat,
        };
        let ch = |c: &str, code, m| press(Key::Character(c.into()), code, m, false);
        let named = |n, code, m| press(Key::Named(n), code, m, false);
        let (mac, linux) = (Keymap::defaults(Platform::MacOs), Keymap::defaults(Platform::Other));
        let cases: [(&Keymap, keyboard::Event, Scope, Option<CommandId>); 18] = [
            (&mac, ch("k", Code::KeyK, Modifiers::LOGO), Global, Some(ids::PALETTE_TOGGLE)),
            (&mac, ch("k", Code::KeyK, CTRL), Global, None),
            (&linux, ch("k", Code::KeyK, CTRL), Global, Some(ids::PALETTE_TOGGLE)),
            (&linux, ch("b", Code::KeyB, CTRL), Composer, Some(ids::PANEL_TOGGLE)),
            (&linux, ch("a", Code::KeyA, Modifiers::ALT), Global, Some(ids::PANEL_AGENTS)),
            (&linux, ch("a", Code::KeyA, Modifiers::ALT), Composer, Some(ids::PANEL_AGENTS)),
            (&mac, ch("a", Code::KeyA, Modifiers::ALT), Global, Some(ids::PANEL_AGENTS)),
            (&linux, ch("b", Code::KeyB, CTRL), Palette, None),
            (&linux, ch("k", Code::KeyK, CTRL), Palette, Some(ids::PALETTE_TOGGLE)),
            (&linux, ch("K", Code::KeyK, CTRL | SHIFT), Global, None),
            (&linux, ch("л", Code::KeyK, CTRL), Global, Some(ids::PALETTE_TOGGLE)),
            (&linux, named(Named::Enter, Code::Enter, NONE), Global, None),
            (&linux, named(Named::Enter, Code::Enter, NONE), Composer, Some(ids::COMPOSER_SUBMIT)),
            (&linux, named(Named::Enter, Code::Enter, SHIFT), Composer, Some(ids::COMPOSER_NEWLINE)),
            (&linux, named(Named::Escape, Code::Escape, NONE), Composer, Some(ids::SESSION_INTERRUPT)),
            (&mac, named(Named::PageDown, Code::PageDown, Modifiers::LOGO), Composer, Some(ids::PROJECT_TAB_NEXT)),
            (&linux, named(Named::Control, Code::ControlLeft, CTRL), Global, None),
            (&linux, press(Key::Character("k".into()), Code::KeyK, CTRL, true), Global, None),
        ];
        for (i, (map, event, scope, want)) in cases.into_iter().enumerate() {
            assert_eq!(map.resolve(&event, scope), want.as_ref(), "case {i}");
        }
    }

    #[test]
    fn defaults_bind_each_chord_once_and_label_per_platform() {
        let (mac, linux) = (Keymap::defaults(Platform::MacOs), Keymap::defaults(Platform::Other));
        for b in [&mac.bindings, &linux.bindings] {
            assert!(b.iter().enumerate().all(|(i, x)| !b[i + 1..].iter().any(|y| (x.0, x.1) == (y.0, y.1))));
        }
        let hint = |map: &Keymap, id: &CommandId| map.chord_for(id).unwrap().label(map.platform());
        let cases = [
            (hint(&mac, &ids::PALETTE_TOGGLE), "⌘K"),
            (hint(&linux, &ids::PALETTE_TOGGLE), "Ctrl+K"),
            (hint(&linux, &ids::SESSION_INTERRUPT), "Esc"),
            (hint(&mac, &ids::COMPOSER_NEWLINE), "⇧↩"),
            (hint(&linux, &ids::COMPOSER_NEWLINE), "Shift+Enter"),
            (hint(&linux, &ids::PROJECT_TAB_NEXT), "Ctrl+PageDown"),
            (Chord::char('p', CTRL | SHIFT | Modifiers::LOGO).label(Platform::MacOs), "⌃⇧⌘P"),
            (Chord::named(Named::F5, Modifiers::ALT).label(Platform::Other), "Alt+F5"),
        ];
        for (got, want) in cases { assert_eq!(got, want) }
    }
}
