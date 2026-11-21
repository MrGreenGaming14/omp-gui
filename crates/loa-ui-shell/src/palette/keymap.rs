//! Keyboard shortcuts as data: chords bound to [`CommandId`]s per [`Scope`], with the defaults
//! in one table.

use super::{CommandId, ids};
use iced::keyboard::{self, Modifiers, key::Named};
use std::fmt::{self, Write as _};

/// Platform convention for the primary modifier and shortcut labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    MacOs,
    Other,
}

impl Platform {
    pub const fn current() -> Self { if cfg!(target_os = "macos") { Self::MacOs } else { Self::Other } }
    /// Cmd on macOS, Ctrl elsewhere.
    pub const fn primary(self) -> Modifiers { if let Self::MacOs = self { Modifiers::LOGO } else { Modifiers::CTRL } }
}

/// The non-modifier key of a chord; characters are lowercase and latin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordKey {
    Char(char),
    Named(Named),
}

/// A key pressed with exactly these modifiers (only Ctrl, Alt, Shift and Logo count).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: ChordKey,
    pub modifiers: Modifiers,
}

const RELEVANT: Modifiers = Modifiers::CTRL.union(Modifiers::ALT).union(Modifiers::SHIFT).union(Modifiers::LOGO);
const MAC_GLYPHS: [(Modifiers, &str); 4] = [(Modifiers::CTRL, "⌃"), (Modifiers::ALT, "⌥"), (Modifiers::SHIFT, "⇧"), (Modifiers::LOGO, "⌘")];
const WORDS: [(Modifiers, &str); 4] = [(Modifiers::CTRL, "Ctrl+"), (Modifiers::ALT, "Alt+"), (Modifiers::SHIFT, "Shift+"), (Modifiers::LOGO, "Super+")];

impl Chord {
    pub fn new(key: ChordKey, modifiers: Modifiers) -> Self {
        let key = match key {
            ChordKey::Char(c) => ChordKey::Char(match (c.to_lowercase().next(), c.to_lowercase().nth(1)) { (Some(l), None) => l, _ => c }),
            named => named,
        };
        Self { key, modifiers: modifiers.intersection(RELEVANT) }
    }

    pub fn char(c: char, modifiers: Modifiers) -> Self { Self::new(ChordKey::Char(c), modifiers) }
    pub fn named(key: Named, modifiers: Modifiers) -> Self { Self::new(ChordKey::Named(key), modifiers) }

    /// The chord of a key press; characters on a non-latin layout map to the latin key at the
    /// same position. Releases, bare modifiers and unidentified keys give `None`.
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
        let mac = platform == Platform::MacOs;
        let mut out: String = (if mac { MAC_GLYPHS } else { WORDS }).iter().filter(|(m, _)| self.modifiers.contains(*m)).map(|(_, s)| *s).collect();
        match self.key {
            ChordKey::Char(c) => out.extend(c.to_uppercase()),
            ChordKey::Named(named) => match named_label(named) {
                Some((word, glyph)) => out.push_str(if mac { glyph } else { word }),
                None => { let _ = write!(out, "{named:?}"); }
            },
        }
        out
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.label(Platform::Other)) }
}

/// The spelled out and macOS labels of named keys; others use iced's name (`F5`, `Insert`).
const NAMED: [(Named, &str, &str); 14] = [
    (Named::Escape, "Esc", "⎋"), (Named::Enter, "Enter", "↩"), (Named::Tab, "Tab", "⇥"), (Named::Space, "Space", "Space"), (Named::Backspace, "Backspace", "⌫"),
    (Named::Delete, "Delete", "⌦"), (Named::ArrowUp, "Up", "↑"), (Named::ArrowDown, "Down", "↓"), (Named::ArrowLeft, "Left", "←"), (Named::ArrowRight, "Right", "→"),
    (Named::PageUp, "PageUp", "⇞"), (Named::PageDown, "PageDown", "⇟"), (Named::Home, "Home", "↖"), (Named::End, "End", "↘"),
];

fn named_label(named: Named) -> Option<(&'static str, &'static str)> { NAMED.iter().find(|(n, ..)| *n == named).map(|&(_, w, g)| (w, g)) }

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Everywhere except modal scopes.
    Global,
    /// While the composer has focus; overrides global bindings there.
    Composer,
    /// While the palette is open; modal, so global bindings do not apply.
    Palette,
}

impl Scope {
    pub const fn is_modal(self) -> bool { matches!(self, Self::Palette) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub chord: Chord,
    pub scope: Scope,
    pub command: CommandId,
}

const NONE: Modifiers = Modifiers::empty();
const SHIFT: Modifiers = Modifiers::SHIFT;
const CTRL: Modifiers = Modifiers::CTRL;
const CTRL_SHIFT: Modifiers = Modifiers::CTRL.union(Modifiers::SHIFT);
use ChordKey::{Char as C, Named as N};
use Scope::{Composer, Global, Palette};

/// The default bindings: key, whether the platform's primary modifier is held, further
/// modifiers, scope and command. Ctrl/Cmd+1…9 and Ctrl/Cmd+Alt+1…9 are added in a loop.
const DEFAULTS: &[(ChordKey, bool, Modifiers, Scope, CommandId)] = &[
    (N(Named::Escape), false, NONE, Global, ids::SESSION_INTERRUPT),
    (C('k'), true, NONE, Global, ids::PALETTE_TOGGLE),
    (C('k'), true, NONE, Palette, ids::PALETTE_TOGGLE),
    (C('/'), true, NONE, Global, ids::KEYMAP_SHOW),
    (C('b'), true, NONE, Global, ids::PANEL_TOGGLE),
    (C('n'), true, NONE, Global, ids::SESSION_NEW),
    (N(Named::Tab), false, CTRL, Global, ids::SESSION_NEXT),
    (N(Named::Tab), false, CTRL_SHIFT, Global, ids::SESSION_PREVIOUS),
    (N(Named::PageDown), true, NONE, Global, ids::PROJECT_TAB_NEXT),
    (N(Named::PageUp), true, NONE, Global, ids::PROJECT_TAB_PREVIOUS),
    (C('l'), true, NONE, Global, ids::COMPOSER_FOCUS),
    (C('f'), true, SHIFT, Global, ids::SEARCH_FOCUS),
    (N(Named::Tab), false, NONE, Global, ids::FOCUS_NEXT),
    (N(Named::Tab), false, SHIFT, Global, ids::FOCUS_PREVIOUS),
    (N(Named::Enter), false, NONE, Composer, ids::COMPOSER_SUBMIT),
    (N(Named::Enter), false, SHIFT, Composer, ids::COMPOSER_NEWLINE),
];

/// The app's shortcuts.
#[derive(Debug, Clone)]
pub struct Keymap {
    platform: Platform,
    bindings: Vec<Binding>,
}

impl Keymap {
    pub fn defaults(platform: Platform) -> Self {
        let primary = platform.primary();
        let bind = |key, modifiers, scope, command| Binding { chord: Chord::new(key, modifiers), scope, command };
        let mut bindings: Vec<_> = DEFAULTS.iter().map(|(k, p, m, s, id)| bind(*k, if *p { primary | *m } else { *m }, *s, id.clone())).collect();
        for n in 1..=9u8 {
            let digit = C(char::from(b'0' + n));
            bindings.push(bind(digit, primary, Global, ids::session_goto(n.into())));
            bindings.push(bind(digit, primary | Modifiers::ALT, Global, ids::panel_tab(n.into())));
        }
        Self { platform, bindings }
    }

    pub fn platform(&self) -> Platform { self.platform }
    pub fn bindings(&self) -> &[Binding] { &self.bindings }

    /// The command `chord` runs while `scope` has focus: the scope's own binding, else (for
    /// non-modal scopes) the global one.
    pub fn resolve_chord(&self, chord: &Chord, scope: Scope) -> Option<&CommandId> {
        let find = |s: Scope| self.bindings.iter().find(|b| b.scope == s && b.chord == *chord).map(|b| &b.command);
        find(scope).or_else(|| (!scope.is_modal() && scope != Global).then(|| find(Global)).flatten())
    }

    /// The command a key event runs; repeats resolve to nothing so held toggles don't flicker.
    pub fn resolve(&self, event: &keyboard::Event, scope: Scope) -> Option<&CommandId> {
        if let keyboard::Event::KeyPressed { repeat: true, .. } = event { return None }
        self.resolve_chord(&Chord::from_event(event)?, scope)
    }

    /// The hint for `command`: its global chord, else its first scoped one.
    pub fn chord_for(&self, command: &CommandId) -> Option<Chord> {
        let mut bound = self.bindings.iter().filter(|b| b.command == *command);
        let first = bound.clone().next()?;
        Some(bound.find(|b| b.scope == Global).unwrap_or(first).chord)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::Key;
    use iced::keyboard::key::{Code, Physical};

    fn press(key: Key, code: Code, modifiers: Modifiers, repeat: bool) -> keyboard::Event {
        let location = keyboard::Location::Standard;
        keyboard::Event::KeyPressed { modified_key: key.clone(), key, physical_key: Physical::Code(code), location, modifiers, text: None, repeat }
    }

    #[test]
    fn defaults_resolve_per_platform_and_scope() {
        let ch = |c: &str, code, m| press(Key::Character(c.into()), code, m, false);
        let named = |n, code, m| press(Key::Named(n), code, m, false);
        let (mac, linux) = (Keymap::defaults(Platform::MacOs), Keymap::defaults(Platform::Other));
        let cases: [(&Keymap, keyboard::Event, Scope, Option<CommandId>); 18] = [
            (&mac, ch("k", Code::KeyK, Modifiers::LOGO), Global, Some(ids::PALETTE_TOGGLE)),
            (&mac, ch("k", Code::KeyK, CTRL), Global, None),
            (&linux, ch("k", Code::KeyK, CTRL), Global, Some(ids::PALETTE_TOGGLE)),
            (&linux, ch("k", Code::KeyK, Modifiers::LOGO), Global, None),
            (&linux, ch("b", Code::KeyB, CTRL), Composer, Some(ids::PANEL_TOGGLE)),
            (&linux, ch("b", Code::KeyB, CTRL), Palette, None),
            (&linux, ch("k", Code::KeyK, CTRL), Palette, Some(ids::PALETTE_TOGGLE)),
            (&linux, ch("K", Code::KeyK, CTRL_SHIFT), Global, None),
            (&linux, ch("k", Code::KeyK, NONE), Global, None),
            (&linux, ch("л", Code::KeyK, CTRL), Global, Some(ids::PALETTE_TOGGLE)),
            (&linux, named(Named::Enter, Code::Enter, NONE), Global, None),
            (&linux, named(Named::Enter, Code::Enter, NONE), Composer, Some(ids::COMPOSER_SUBMIT)),
            (&linux, named(Named::Enter, Code::Enter, SHIFT), Composer, Some(ids::COMPOSER_NEWLINE)),
            (&linux, named(Named::Escape, Code::Escape, NONE), Composer, Some(ids::SESSION_INTERRUPT)),
            (&linux, named(Named::Escape, Code::Escape, NONE), Palette, None),
            (&mac, named(Named::PageDown, Code::PageDown, Modifiers::LOGO), Composer, Some(ids::PROJECT_TAB_NEXT)),
            (&linux, named(Named::PageDown, Code::PageDown, NONE), Global, None),
            (&linux, press(Key::Character("k".into()), Code::KeyK, CTRL, true), Global, None),
        ];
        for (i, (map, event, scope, want)) in cases.into_iter().enumerate() {
            assert_eq!(map.resolve(&event, scope), want.as_ref(), "case {i}");
        }
        assert_eq!(Chord::from_event(&named(Named::Control, Code::ControlLeft, CTRL)), None);
    }

    #[test]
    fn defaults_bind_each_chord_once_and_label_per_platform() {
        for map in [Keymap::defaults(Platform::MacOs), Keymap::defaults(Platform::Other)] {
            let b = map.bindings();
            assert!(b.iter().enumerate().all(|(i, x)| !b[i + 1..].iter().any(|y| x.chord == y.chord && x.scope == y.scope)));
        }
        let (mac, linux) = (Keymap::defaults(Platform::MacOs), Keymap::defaults(Platform::Other));
        let hint = |map: &Keymap, id: &CommandId| map.chord_for(id).unwrap().label(map.platform());
        let cases = [
            (hint(&mac, &ids::PALETTE_TOGGLE), "⌘K"),
            (hint(&linux, &ids::PALETTE_TOGGLE), "Ctrl+K"),
            (hint(&linux, &ids::SESSION_INTERRUPT), "Esc"),
            (hint(&mac, &ids::COMPOSER_NEWLINE), "⇧↩"),
            (hint(&linux, &ids::COMPOSER_NEWLINE), "Shift+Enter"),
            (hint(&linux, &ids::PROJECT_TAB_NEXT), "Ctrl+PageDown"),
            (Chord::char('p', CTRL_SHIFT | Modifiers::LOGO).label(Platform::MacOs), "⌃⇧⌘P"),
            (Chord::named(Named::F5, Modifiers::ALT).label(Platform::Other), "Alt+F5"),
        ];
        for (got, want) in cases { assert_eq!(got, want) }
    }
}
