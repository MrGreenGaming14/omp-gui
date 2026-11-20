//! Key bindings of the composer editor: Enter submits, Shift+Enter breaks the
//! line, undo and redo, word deletes, popup navigation and Esc to interrupt.

use iced::keyboard::Key;
use iced::keyboard::key::Named;
use iced::widget::text_editor::{Binding, KeyPress, Motion, Status};

use crate::Message;

/// Composer state the bindings depend on, copied into the editor's
/// `key_binding` closure on every view.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct KeyContext {
    /// A `/` or `@` popup shows items.
    pub popup_open: bool,
    /// An input method is composing text; its keys must reach the IME.
    pub composing: bool,
    /// A turn runs or is starting.
    pub busy: bool,
    /// Text is selected; a word delete then deletes just the selection.
    pub selection: bool,
}

pub(crate) fn bind(press: KeyPress, ctx: KeyContext) -> Option<Binding<Message>> {
    if !matches!(press.status, Status::Focused { .. }) {
        return None;
    }
    let named = match press.key.as_ref() {
        Key::Named(named) => Some(named),
        _ => None,
    };
    let ours = matches!(named, Some(Named::Enter | Named::Escape | Named::Tab | Named::ArrowUp | Named::ArrowDown));
    if ctx.composing && ours {
        // The IME uses these to confirm or cancel its candidate; leave them alone.
        return None;
    }

    let modifiers = press.modifiers;
    if modifiers.command() {
        match press.key.to_latin(press.physical_key) {
            Some('z') if modifiers.shift() => return Some(Binding::Custom(Message::Redo)),
            Some('z') => return Some(Binding::Custom(Message::Undo)),
            Some('y') => return Some(Binding::Custom(Message::Redo)),
            // Clipboard and select all stay with the editor; a paste also
            // asks the app whether the clipboard holds an image.
            Some('v') => return Some(Binding::Sequence(vec![Binding::Paste, Binding::Custom(Message::PasteImage)])),
            Some('c' | 'x' | 'a') => {}
            // Any other Ctrl/Cmd+character chord is an app shortcut (new
            // session, palette, switch session…): leave it uncaptured so the
            // app's keymap sees it. On Windows AltGr arrives as Ctrl+Alt and
            // types characters, so those go to the editor.
            Some(_) if !(cfg!(windows) && modifiers.alt()) => return None,
            _ => {}
        }
    }

    let custom = |message| Some(Binding::Custom(message));
    match named {
        Some(Named::Enter) if modifiers.shift() => Some(Binding::Enter),
        Some(Named::Enter) if ctx.popup_open => custom(Message::PopupAccept),
        Some(Named::Enter) => custom(Message::Submit),
        Some(Named::Tab) if ctx.popup_open && !modifiers.shift() => custom(Message::PopupAccept),
        Some(Named::ArrowUp) if ctx.popup_open => custom(Message::PopupPrevious),
        Some(Named::ArrowDown) if ctx.popup_open => custom(Message::PopupNext),
        Some(Named::Escape) if ctx.popup_open => custom(Message::PopupClose),
        Some(Named::Escape) if ctx.busy => custom(Message::Interrupt),
        // Ctrl+Backspace / Ctrl+Delete (Alt on macOS) delete a word; on
        // macOS Cmd+Backspace deletes to the start of the line.
        Some(Named::Backspace | Named::Delete) if ctx.selection && (modifiers.jump() || modifiers.macos_command()) => {
            Some(if named == Some(Named::Backspace) { Binding::Backspace } else { Binding::Delete })
        }
        Some(Named::Backspace) if modifiers.macos_command() => Some(Binding::Sequence(vec![Binding::Select(Motion::Home), Binding::Backspace])),
        Some(Named::Backspace) if modifiers.jump() => Some(Binding::Sequence(vec![Binding::Select(Motion::WordLeft), Binding::Backspace])),
        Some(Named::Delete) if modifiers.jump() => Some(Binding::Sequence(vec![Binding::Select(Motion::WordRight), Binding::Delete])),
        _ => Binding::from_key_press(press),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::Modifiers;
    use iced::keyboard::key::{NativeCode, Physical};

    fn press(key: Key, modifiers: Modifiers) -> KeyPress {
        KeyPress {
            key: key.clone(),
            modified_key: key,
            physical_key: Physical::Unidentified(NativeCode::Unidentified),
            modifiers,
            text: None,
            status: Status::Focused { is_hovered: false },
        }
    }

    fn enter(modifiers: Modifiers, ctx: KeyContext) -> Option<Binding<Message>> {
        bind(press(Key::Named(Named::Enter), modifiers), ctx)
    }

    #[test]
    fn enter_submits_and_shift_enter_breaks_the_line() {
        let ctx = KeyContext::default();
        assert!(matches!(enter(Modifiers::empty(), ctx), Some(Binding::Custom(Message::Submit))));
        assert_eq!(enter(Modifiers::SHIFT, ctx), Some(Binding::Enter));
    }

    #[test]
    fn enter_while_composing_goes_to_the_input_method() {
        let ctx = KeyContext { composing: true, ..KeyContext::default() };
        assert_eq!(enter(Modifiers::empty(), ctx), None);
        let esc = bind(press(Key::Named(Named::Escape), Modifiers::empty()), KeyContext { busy: true, ..ctx });
        assert_eq!(esc, None);
    }

    #[test]
    fn undo_and_redo_shortcuts() {
        let ctx = KeyContext::default();
        let z = || Key::Character("z".into());
        assert!(matches!(bind(press(z(), Modifiers::COMMAND), ctx), Some(Binding::Custom(Message::Undo))));
        let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
        assert!(matches!(bind(press(z(), shift_cmd), ctx), Some(Binding::Custom(Message::Redo))));
        let y = Key::Character("y".into());
        assert!(matches!(bind(press(y, Modifiers::COMMAND), ctx), Some(Binding::Custom(Message::Redo))));
    }

    #[test]
    fn popup_takes_navigation_keys_and_esc_interrupts_a_turn() {
        let popup = KeyContext { popup_open: true, busy: true, ..KeyContext::default() };
        let key = |named| bind(press(Key::Named(named), Modifiers::empty()), popup);
        assert!(matches!(key(Named::ArrowDown), Some(Binding::Custom(Message::PopupNext))));
        assert!(matches!(key(Named::ArrowUp), Some(Binding::Custom(Message::PopupPrevious))));
        assert!(matches!(key(Named::Tab), Some(Binding::Custom(Message::PopupAccept))));
        assert!(matches!(key(Named::Enter), Some(Binding::Custom(Message::PopupAccept))));
        assert!(matches!(key(Named::Escape), Some(Binding::Custom(Message::PopupClose))));

        let running = KeyContext { busy: true, ..KeyContext::default() };
        let esc = bind(press(Key::Named(Named::Escape), Modifiers::empty()), running);
        assert!(matches!(esc, Some(Binding::Custom(Message::Interrupt))));
        let idle = bind(press(Key::Named(Named::Escape), Modifiers::empty()), KeyContext::default());
        assert_eq!(idle, Some(Binding::Unfocus));
    }

    #[test]
    fn app_shortcuts_pass_through_and_clipboard_stays() {
        let ctx = KeyContext::default();
        for c in ["n", "k", "l", "1", "/"] {
            assert_eq!(bind(press(Key::Character(c.into()), Modifiers::COMMAND), ctx), None, "Ctrl/Cmd+{c}");
        }
        let copy = bind(press(Key::Character("c".into()), Modifiers::COMMAND), ctx);
        assert_eq!(copy, Some(Binding::Copy));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn ctrl_edit_keys_delete_and_move_by_word() {
        let ctx = KeyContext::default();
        let ctrl = |named| bind(press(Key::Named(named), Modifiers::CTRL), ctx);
        assert_eq!(ctrl(Named::Backspace), Some(Binding::Sequence(vec![Binding::Select(Motion::WordLeft), Binding::Backspace])));
        assert_eq!(ctrl(Named::Delete), Some(Binding::Sequence(vec![Binding::Select(Motion::WordRight), Binding::Delete])));
        assert_eq!(ctrl(Named::ArrowLeft), Some(Binding::Move(Motion::WordLeft)));
        assert_eq!(ctrl(Named::ArrowRight), Some(Binding::Move(Motion::WordRight)));
        assert_eq!(ctrl(Named::Home), Some(Binding::Move(Motion::DocumentStart)));
        assert_eq!(ctrl(Named::End), Some(Binding::Move(Motion::DocumentEnd)));
        let select = bind(press(Key::Named(Named::ArrowLeft), Modifiers::CTRL | Modifiers::SHIFT), ctx);
        assert_eq!(select, Some(Binding::Select(Motion::WordLeft)));
        // With a selection, the word delete takes just the selection.
        let selected = KeyContext { selection: true, ..ctx };
        assert_eq!(bind(press(Key::Named(Named::Backspace), Modifiers::CTRL), selected), Some(Binding::Backspace));
    }
}
