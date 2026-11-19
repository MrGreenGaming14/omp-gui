//! Keyboard shortcuts as data.
//!
//! A [`Keymap`] maps key [`Chord`]s to [`CommandId`]s within a [`Scope`]. The
//! app feeds iced keyboard events to [`Keymap::resolve`] together with the
//! scope that has focus and runs whatever command id comes back.

use std::fmt::{self, Write as _};

use iced::keyboard::{self, Modifiers, key::Named};

use crate::{CommandId, ids};

/// Error of keymap edits.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The chord is already bound to another command in the same scope.
    #[error("{} is already bound to {} in {:?}", .0.chord, .0.existing, .0.scope)]
    Conflict(Conflict),
}

/// Result of keymap edits.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Platform convention for the primary modifier and shortcut labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Cmd is the primary modifier; labels use the macOS glyphs.
    MacOs,
    /// Ctrl is the primary modifier; labels are spelled out.
    Other,
}

impl Platform {
    /// The platform this binary was built for.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }

    /// The modifier app shortcuts use: Cmd on macOS, Ctrl elsewhere.
    pub const fn primary(self) -> Modifiers {
        match self {
            Self::MacOs => Modifiers::LOGO,
            Self::Other => Modifiers::CTRL,
        }
    }
}

/// The non-modifier key of a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordKey {
    /// A character key, stored lowercase and in its latin form.
    Char(char),
    /// A named key such as Escape or Enter.
    Named(Named),
}

/// A key pressed together with an exact set of modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    /// The key.
    pub key: ChordKey,
    /// Exactly the modifiers held; only Ctrl, Alt, Shift and Logo count.
    pub modifiers: Modifiers,
}

const RELEVANT: Modifiers = Modifiers::CTRL
    .union(Modifiers::ALT)
    .union(Modifiers::SHIFT)
    .union(Modifiers::LOGO);

impl Chord {
    /// A chord of a key and modifiers. Characters are lowercased.
    pub fn new(key: ChordKey, modifiers: Modifiers) -> Self {
        let key = match key {
            ChordKey::Char(c) => ChordKey::Char(lowercase(c)),
            named => named,
        };
        Self {
            key,
            modifiers: modifiers.intersection(RELEVANT),
        }
    }

    /// A character key with modifiers.
    pub fn char(c: char, modifiers: Modifiers) -> Self {
        Self::new(ChordKey::Char(c), modifiers)
    }

    /// A named key with modifiers.
    pub fn named(key: Named, modifiers: Modifiers) -> Self {
        Self::new(ChordKey::Named(key), modifiers)
    }

    /// The chord of a key press. Releases, modifier-only presses and
    /// unidentified keys give `None`. Characters typed on a non-latin layout
    /// map to the latin key at the same position, so Ctrl+K works on a
    /// Cyrillic layout too.
    pub fn from_event(event: &keyboard::Event) -> Option<Self> {
        let keyboard::Event::KeyPressed {
            key,
            physical_key,
            modifiers,
            ..
        } = event
        else {
            return None;
        };
        let chord_key = match key {
            keyboard::Key::Named(
                Named::Shift
                | Named::Control
                | Named::Alt
                | Named::AltGraph
                | Named::Super
                | Named::Meta
                | Named::Hyper,
            )
            | keyboard::Key::Unidentified => return None,
            keyboard::Key::Named(named) => ChordKey::Named(*named),
            keyboard::Key::Character(text) => {
                let c = key
                    .to_latin(*physical_key)
                    .or_else(|| text.chars().next())?;
                ChordKey::Char(c)
            }
        };
        Some(Self::new(chord_key, *modifiers))
    }

    /// The shortcut hint shown next to a command, e.g. `Ctrl+K` or `⌘K`.
    pub fn label(&self, platform: Platform) -> String {
        let mut out = String::new();
        match platform {
            Platform::MacOs => {
                for (flag, glyph) in [
                    (Modifiers::CTRL, '⌃'),
                    (Modifiers::ALT, '⌥'),
                    (Modifiers::SHIFT, '⇧'),
                    (Modifiers::LOGO, '⌘'),
                ] {
                    if self.modifiers.contains(flag) {
                        out.push(glyph);
                    }
                }
            }
            Platform::Other => {
                for (flag, name) in [
                    (Modifiers::CTRL, "Ctrl+"),
                    (Modifiers::ALT, "Alt+"),
                    (Modifiers::SHIFT, "Shift+"),
                    (Modifiers::LOGO, "Super+"),
                ] {
                    if self.modifiers.contains(flag) {
                        out.push_str(name);
                    }
                }
            }
        }
        match self.key {
            ChordKey::Char(c) => out.extend(c.to_uppercase()),
            ChordKey::Named(named) => match (platform, named_label(named)) {
                (Platform::MacOs, Some((_, glyph))) => out.push_str(glyph),
                (Platform::Other, Some((word, _))) => out.push_str(word),
                // Keys without a dedicated label use iced's variant name,
                // which is the key's documented name (e.g. `F5`, `PageUp`).
                (_, None) => {
                    let _ = write!(out, "{named:?}");
                }
            },
        }
        out
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label(Platform::Other))
    }
}

fn named_label(named: Named) -> Option<(&'static str, &'static str)> {
    Some(match named {
        Named::Escape => ("Esc", "⎋"),
        Named::Enter => ("Enter", "↩"),
        Named::Tab => ("Tab", "⇥"),
        Named::Space => ("Space", "Space"),
        Named::Backspace => ("Backspace", "⌫"),
        Named::Delete => ("Delete", "⌦"),
        Named::ArrowUp => ("Up", "↑"),
        Named::ArrowDown => ("Down", "↓"),
        Named::ArrowLeft => ("Left", "←"),
        Named::ArrowRight => ("Right", "→"),
        Named::PageUp => ("PageUp", "⇞"),
        Named::PageDown => ("PageDown", "⇟"),
        Named::Home => ("Home", "↖"),
        Named::End => ("End", "↘"),
        _ => return None,
    })
}

fn lowercase(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Everywhere except modal scopes.
    Global,
    /// Only while the composer has focus. Overrides global bindings there.
    Composer,
    /// Only while the palette is open. Modal: global bindings do not apply,
    /// so Esc closes the palette instead of interrupting the session.
    Palette,
}

impl Scope {
    /// Whether global bindings are suspended while this scope has focus.
    pub const fn is_modal(self) -> bool {
        matches!(self, Self::Palette)
    }
}

/// One chord bound to one command in one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The chord.
    pub chord: Chord,
    /// Where it applies.
    pub scope: Scope,
    /// The command it runs.
    pub command: CommandId,
}

impl Binding {
    /// A binding of `chord` to `command` in `scope`.
    pub fn new(chord: Chord, scope: Scope, command: CommandId) -> Self {
        Self {
            chord,
            scope,
            command,
        }
    }
}

/// How two bindings collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// Same chord, same scope, different commands: one of them can never run.
    Duplicate,
    /// A non-modal scope rebinds a global chord: the global command cannot be
    /// reached by that chord while the scope has focus.
    Shadows,
}

/// Two bindings that collide on a chord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// How they collide.
    pub kind: ConflictKind,
    /// The chord both use.
    pub chord: Chord,
    /// Scope of the binding that wins (for `Shadows`, the narrower scope).
    pub scope: Scope,
    /// The command bound first (for `Shadows`, the global one).
    pub existing: CommandId,
    /// The command bound second (for `Shadows`, the scoped one).
    pub new: CommandId,
}

/// Every conflict in a list of bindings, e.g. a user's keymap file. Bindings
/// of the same chord to the same command are not conflicts.
pub fn conflicts(bindings: &[Binding]) -> Vec<Conflict> {
    let mut found = Vec::new();
    for (i, a) in bindings.iter().enumerate() {
        for b in &bindings[i + 1..] {
            if a.chord != b.chord || a.command == b.command {
                continue;
            }
            if a.scope == b.scope {
                found.push(Conflict {
                    kind: ConflictKind::Duplicate,
                    chord: a.chord,
                    scope: a.scope,
                    existing: a.command.clone(),
                    new: b.command.clone(),
                });
                continue;
            }
            let (global, scoped) = match (a.scope, b.scope) {
                (Scope::Global, s) if !s.is_modal() => (a, b),
                (s, Scope::Global) if !s.is_modal() => (b, a),
                _ => continue,
            };
            found.push(Conflict {
                kind: ConflictKind::Shadows,
                chord: a.chord,
                scope: scoped.scope,
                existing: global.command.clone(),
                new: scoped.command.clone(),
            });
        }
    }
    found
}

/// The app's shortcuts.
#[derive(Debug, Clone)]
pub struct Keymap {
    platform: Platform,
    bindings: Vec<Binding>,
}

impl Keymap {
    /// A keymap with no bindings.
    pub fn empty(platform: Platform) -> Self {
        Self {
            platform,
            bindings: Vec::new(),
        }
    }

    /// The default shortcuts the design names:
    ///
    /// - Esc interrupts streaming (global)
    /// - Ctrl/Cmd+K toggles the palette (global, and inside the palette)
    /// - Ctrl/Cmd+B toggles the right panel (global)
    /// - Enter submits and Shift+Enter inserts a line break, in the composer only
    pub fn defaults(platform: Platform) -> Self {
        let primary = platform.primary();
        let none = Modifiers::empty();
        Self {
            platform,
            bindings: vec![
                Binding::new(
                    Chord::named(Named::Escape, none),
                    Scope::Global,
                    ids::SESSION_INTERRUPT,
                ),
                Binding::new(Chord::char('k', primary), Scope::Global, ids::PALETTE_TOGGLE),
                Binding::new(Chord::char('k', primary), Scope::Palette, ids::PALETTE_TOGGLE),
                Binding::new(Chord::char('b', primary), Scope::Global, ids::PANEL_TOGGLE),
                Binding::new(
                    Chord::named(Named::Enter, none),
                    Scope::Composer,
                    ids::COMPOSER_SUBMIT,
                ),
                Binding::new(
                    Chord::named(Named::Enter, Modifiers::SHIFT),
                    Scope::Composer,
                    ids::COMPOSER_NEWLINE,
                ),
            ],
        }
    }

    /// The platform the keymap labels and defaults follow.
    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// All bindings in the order they were added.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Adds a binding. Fails when the chord already runs a different command in
    /// the same scope; rebinding the same command is a no-op.
    pub fn bind(&mut self, binding: Binding) -> Result<()> {
        if let Some(existing) = self
            .bindings
            .iter()
            .find(|b| b.chord == binding.chord && b.scope == binding.scope)
        {
            if existing.command == binding.command {
                return Ok(());
            }
            return Err(Error::Conflict(Conflict {
                kind: ConflictKind::Duplicate,
                chord: binding.chord,
                scope: binding.scope,
                existing: existing.command.clone(),
                new: binding.command,
            }));
        }
        self.bindings.push(binding);
        Ok(())
    }

    /// Removes the binding of `chord` in `scope`, returning it.
    pub fn unbind(&mut self, chord: Chord, scope: Scope) -> Option<Binding> {
        let index = self
            .bindings
            .iter()
            .position(|b| b.chord == chord && b.scope == scope)?;
        Some(self.bindings.remove(index))
    }

    /// The command `chord` runs while `scope` has focus. A scope's own
    /// bindings win over global ones; modal scopes see only their own.
    pub fn resolve_chord(&self, chord: &Chord, scope: Scope) -> Option<&CommandId> {
        let in_scope = |s: Scope| {
            self.bindings
                .iter()
                .find(|b| b.scope == s && b.chord == *chord)
                .map(|b| &b.command)
        };
        in_scope(scope).or_else(|| {
            if scope.is_modal() || scope == Scope::Global {
                None
            } else {
                in_scope(Scope::Global)
            }
        })
    }

    /// The command an iced keyboard event runs while `scope` has focus.
    /// Key repeats resolve to nothing, so holding a toggle does not flicker.
    pub fn resolve(&self, event: &keyboard::Event, scope: Scope) -> Option<&CommandId> {
        if let keyboard::Event::KeyPressed { repeat: true, .. } = event {
            return None;
        }
        self.resolve_chord(&Chord::from_event(event)?, scope)
    }

    /// The chord to show as the hint for `command`: its global binding if any,
    /// otherwise its first scoped one.
    pub fn chord_for(&self, command: &CommandId) -> Option<Chord> {
        let mut bound = self.bindings.iter().filter(|b| b.command == *command);
        let first = bound.clone().next()?;
        Some(
            bound
                .find(|b| b.scope == Scope::Global)
                .unwrap_or(first)
                .chord,
        )
    }

    /// Every conflict among this keymap's bindings.
    pub fn conflicts(&self) -> Vec<Conflict> {
        conflicts(&self.bindings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::key::{Code, Physical};
    use iced::keyboard::{Key, Location};

    fn press(key: Key, physical: Code, modifiers: Modifiers) -> keyboard::Event {
        keyboard::Event::KeyPressed {
            modified_key: key.clone(),
            key,
            physical_key: Physical::Code(physical),
            location: Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        }
    }

    fn ch(c: &str, code: Code, modifiers: Modifiers) -> keyboard::Event {
        press(Key::Character(c.into()), code, modifiers)
    }

    #[test]
    fn primary_modifier_follows_platform() {
        let mac = Keymap::defaults(Platform::MacOs);
        let linux = Keymap::defaults(Platform::Other);

        let cmd_k = ch("k", Code::KeyK, Modifiers::LOGO);
        let ctrl_k = ch("k", Code::KeyK, Modifiers::CTRL);

        assert_eq!(mac.resolve(&cmd_k, Scope::Global), Some(&ids::PALETTE_TOGGLE));
        assert_eq!(mac.resolve(&ctrl_k, Scope::Global), None);
        assert_eq!(linux.resolve(&ctrl_k, Scope::Global), Some(&ids::PALETTE_TOGGLE));
        assert_eq!(linux.resolve(&cmd_k, Scope::Global), None);

        let ctrl_b = ch("b", Code::KeyB, Modifiers::CTRL);
        assert_eq!(linux.resolve(&ctrl_b, Scope::Composer), Some(&ids::PANEL_TOGGLE));
    }

    #[test]
    fn modifiers_must_match_exactly() {
        let map = Keymap::defaults(Platform::Other);
        let ctrl_shift_k = ch("K", Code::KeyK, Modifiers::CTRL | Modifiers::SHIFT);
        assert_eq!(map.resolve(&ctrl_shift_k, Scope::Global), None);
        let plain_k = ch("k", Code::KeyK, Modifiers::empty());
        assert_eq!(map.resolve(&plain_k, Scope::Global), None);
    }

    #[test]
    fn non_latin_layout_resolves_by_key_position() {
        let map = Keymap::defaults(Platform::Other);
        // Cyrillic "л" sits on the K key.
        let ctrl_k = ch("л", Code::KeyK, Modifiers::CTRL);
        assert_eq!(map.resolve(&ctrl_k, Scope::Global), Some(&ids::PALETTE_TOGGLE));
    }

    #[test]
    fn enter_is_bound_only_in_the_composer() {
        let map = Keymap::defaults(Platform::Other);
        let enter = press(Key::Named(Named::Enter), Code::Enter, Modifiers::empty());
        let shift_enter = press(Key::Named(Named::Enter), Code::Enter, Modifiers::SHIFT);

        assert_eq!(map.resolve(&enter, Scope::Global), None);
        assert_eq!(map.resolve(&enter, Scope::Composer), Some(&ids::COMPOSER_SUBMIT));
        assert_eq!(
            map.resolve(&shift_enter, Scope::Composer),
            Some(&ids::COMPOSER_NEWLINE)
        );
    }

    #[test]
    fn escape_interrupts_except_while_palette_is_open() {
        let map = Keymap::defaults(Platform::Other);
        let esc = press(Key::Named(Named::Escape), Code::Escape, Modifiers::empty());
        assert_eq!(map.resolve(&esc, Scope::Global), Some(&ids::SESSION_INTERRUPT));
        assert_eq!(map.resolve(&esc, Scope::Composer), Some(&ids::SESSION_INTERRUPT));
        assert_eq!(map.resolve(&esc, Scope::Palette), None);

        let ctrl_k = ch("k", Code::KeyK, Modifiers::CTRL);
        assert_eq!(map.resolve(&ctrl_k, Scope::Palette), Some(&ids::PALETTE_TOGGLE));
        let ctrl_b = ch("b", Code::KeyB, Modifiers::CTRL);
        assert_eq!(map.resolve(&ctrl_b, Scope::Palette), None);
    }

    #[test]
    fn key_repeat_and_releases_resolve_to_nothing() {
        let map = Keymap::defaults(Platform::Other);
        let keyboard::Event::KeyPressed {
            key,
            modified_key,
            physical_key,
            location,
            modifiers,
            text,
            ..
        } = ch("k", Code::KeyK, Modifiers::CTRL)
        else {
            unreachable!("ch builds a key press");
        };
        let repeat = keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: modified_key.clone(),
            physical_key,
            location,
            modifiers,
            text,
            repeat: true,
        };
        assert_eq!(map.resolve(&repeat, Scope::Global), None);
        let release = keyboard::Event::KeyReleased {
            key,
            modified_key,
            physical_key,
            location,
            modifiers,
        };
        assert_eq!(map.resolve(&release, Scope::Global), None);
        let ctrl_alone = press(
            Key::Named(Named::Control),
            Code::ControlLeft,
            Modifiers::CTRL,
        );
        assert_eq!(Chord::from_event(&ctrl_alone), None);
    }

    #[test]
    fn bind_rejects_duplicate_chord_in_same_scope() {
        let mut map = Keymap::defaults(Platform::Other);
        let ctrl_k = Chord::char('K', Modifiers::CTRL);
        let err = map
            .bind(Binding::new(
                ctrl_k,
                Scope::Global,
                CommandId::from_static("search.open"),
            ))
            .unwrap_err();
        let Error::Conflict(conflict) = err;
        assert_eq!(conflict.kind, ConflictKind::Duplicate);
        assert_eq!(conflict.existing, ids::PALETTE_TOGGLE);
        assert_eq!(conflict.new.as_str(), "search.open");

        // Same command again is fine, and the palette still owns the chord.
        map.bind(Binding::new(ctrl_k, Scope::Global, ids::PALETTE_TOGGLE))
            .unwrap();
        assert_eq!(
            map.resolve_chord(&ctrl_k, Scope::Global),
            Some(&ids::PALETTE_TOGGLE)
        );

        // After unbinding, the new command can take the chord.
        map.unbind(ctrl_k, Scope::Global).unwrap();
        map.bind(Binding::new(
            ctrl_k,
            Scope::Global,
            CommandId::from_static("search.open"),
        ))
        .unwrap();
        assert_eq!(
            map.resolve_chord(&ctrl_k, Scope::Global).map(CommandId::as_str),
            Some("search.open")
        );
    }

    #[test]
    fn defaults_have_no_conflicts_and_shadowing_is_reported() {
        assert!(Keymap::defaults(Platform::Other).conflicts().is_empty());
        assert!(Keymap::defaults(Platform::MacOs).conflicts().is_empty());

        let ctrl_b = Chord::char('b', Modifiers::CTRL);
        let bold = CommandId::from_static("composer.bold");
        let list = [
            Binding::new(ctrl_b, Scope::Global, ids::PANEL_TOGGLE),
            Binding::new(ctrl_b, Scope::Composer, bold.clone()),
            Binding::new(ctrl_b, Scope::Composer, ids::COMPOSER_SUBMIT),
            Binding::new(ctrl_b, Scope::Palette, ids::PALETTE_TOGGLE),
        ];
        let found = conflicts(&list);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found.iter().any(|c| c.kind == ConflictKind::Shadows
            && c.existing == ids::PANEL_TOGGLE
            && c.new == bold
            && c.scope == Scope::Composer));
        assert!(found.iter().any(|c| c.kind == ConflictKind::Duplicate
            && c.existing == bold
            && c.new == ids::COMPOSER_SUBMIT));
    }

    #[test]
    fn scoped_binding_overrides_global_in_its_scope() {
        let mut map = Keymap::defaults(Platform::Other);
        let ctrl_b = Chord::char('b', Modifiers::CTRL);
        let bold = CommandId::from_static("composer.bold");
        map.bind(Binding::new(ctrl_b, Scope::Composer, bold.clone()))
            .unwrap();
        assert_eq!(map.resolve_chord(&ctrl_b, Scope::Composer), Some(&bold));
        assert_eq!(
            map.resolve_chord(&ctrl_b, Scope::Global),
            Some(&ids::PANEL_TOGGLE)
        );
    }

    #[test]
    fn labels_follow_platform_conventions() {
        let mac = Keymap::defaults(Platform::MacOs);
        let linux = Keymap::defaults(Platform::Other);
        fn hint(map: &Keymap, id: &CommandId) -> String {
            map.chord_for(id).unwrap().label(map.platform())
        }

        assert_eq!(hint(&mac, &ids::PALETTE_TOGGLE), "⌘K");
        assert_eq!(hint(&linux, &ids::PALETTE_TOGGLE), "Ctrl+K");
        assert_eq!(hint(&linux, &ids::SESSION_INTERRUPT), "Esc");
        assert_eq!(hint(&mac, &ids::COMPOSER_NEWLINE), "⇧↩");
        assert_eq!(hint(&linux, &ids::COMPOSER_NEWLINE), "Shift+Enter");
        assert_eq!(
            Chord::char('p', Modifiers::CTRL | Modifiers::SHIFT | Modifiers::LOGO)
                .label(Platform::MacOs),
            "⌃⇧⌘P"
        );
        assert_eq!(
            Chord::named(Named::F5, Modifiers::ALT).label(Platform::Other),
            "Alt+F5"
        );
    }
}
