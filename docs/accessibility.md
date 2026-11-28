# Accessibility

How screen reader and assistive technology support stands in this Qt Widgets app, and the rules UI modules follow. Tracks issue #84.

## Current state

Qt 6 Widgets ship accessibility through `QAccessible`: every standard widget exposes a role, name, state and text to the platform bridge (AT-SPI on Linux, UI Automation on Windows, NSAccessibility on macOS). Nothing extra is linked or enabled; what a screen reader sees depends on which widgets we use and how we label them.

Gaps today:

- The terminal pane draws libvterm's screen on a custom canvas and exposes nothing.
- Pixel surfaces (surface view frames) expose nothing.
- Custom-painted widgets expose only what they set with `setAccessibleName`/`setAccessibleDescription`; they have no custom `QAccessibleInterface` yet.

## Rules for UI modules

1. Standard Qt widgets first. A button is a `QPushButton`/`QToolButton`, a list is a `QListView`/`QListWidget` (or the kit's virtual list), a field is a `QLineEdit`/`QPlainTextEdit`. Standard widgets are accessible for free.
2. Custom widgets (own `paintEvent`, anything that draws text itself) call `setAccessibleName` and, where the name is not enough, `setAccessibleDescription` (as the RECIPE Qt UI rules require).
3. Icon-only buttons get an accessible name and a tooltip next to the code that builds them, never only an image.
4. Every action has a keyboard path: a shortcut in the shell `Keymap`, an entry in the command palette, or focus traversal. No hover-only or drag-only actions.
5. State is never shown by color alone. Pair it with text or an icon that has a label (e.g. the waiting badge says "Waiting").
6. Colors come from the theme palette (already an ARCHITECTURE.md rule), so a high contrast theme is one theme away.
7. Widgets that are focus or scroll targets get a stable `objectName`, which tests and focus code key off.
8. Text is shown with Qt text widgets (`QLabel`, `QTextBrowser`, `QPlainTextEdit`) wherever possible, not rasterized into images or painted by hand, so it stays exposed.

## Hand built widgets in this app

From the design ([gui.md](design/gui.md)) and [ARCHITECTURE.md](ARCHITECTURE.md). Owners keep this table current.

| Widget | Module | Built from | Accessibility notes |
| --- | --- | --- | --- |
| Command palette | `cpp/ui/shell` | Standard widgets: search field and list | Role: dialog with a search field (`loa-palette-search`) and a list (`loa-palette-list`) of items named by command title; keymap reference rows are plain text. Fully keyboard driven (Ctrl/Cmd+K, Up, Down, Enter, Esc). Highlighting uses weight, not color |
| Diff view | `cpp/ui/panel` | Custom rows | Needs per-line accessible names with an added/removed label, not color alone |
| Tab bar (right panel) | `cpp/ui/shell` | Buttons named by tab title | The selected state is drawn only (underline); every tab has Ctrl/Cmd+Alt+n and an "Open X tab" palette command |
| `/` and `@` popups | `cpp/ui/session` (composer) | Hand built | Role: listbox attached to the composer field |
| Working indicator | `cpp/ui/session` (composer) | Spinner icon and labels | Accessible name "<label>, <elapsed>" (e.g. "Reading the config…, 12s"); the spinner is decorative. State is in the words, not the color |
| Virtualized transcript list | `cpp/ui/session` (transcript) | Kit virtual list (`cpp/ui/kit`) | Off-screen items must still be reachable by keyboard |
| Terminal pane | `cpp/ui/terminal` | libvterm screen on a custom canvas | Exposes nothing yet |
| Surface view | `cpp/ui/surface` | Pixel frames | Pixel surfaces expose nothing yet; document mode surfaces should show text with Qt text widgets |

## Keyboard map and reachability

Issues #78, #82, #83. The keymap lives in `loa::ui::shell::Keymap::defaults` (`cpp/ui/shell/include/loa/ui/shell/keymap.hpp`); the app (`cpp/app/src/app/keys.cpp`) runs it and lists it in the palette. Ctrl/Cmd+/ opens the palette on the keymap only; the normal palette shows each command's chord and a "Keyboard shortcuts" section for keys that aren't commands.
| Keys | Does |
| --- | --- |
| Ctrl/Cmd+K | Command palette (every app and area command) |
| Ctrl/Cmd+/ | Palette on the keyboard shortcuts |
| Ctrl/Cmd+N | New session |
| Ctrl+Tab, Ctrl+Shift+Tab | Next, previous session in the sidebar's order |
| Ctrl/Cmd+1…9 | n-th session in the sidebar |
| Ctrl/Cmd+PageDown, Ctrl/Cmd+PageUp | Next, previous project tab (with the project tabs setting on) |
| Ctrl/Cmd+Alt+1…9 | n-th right panel tab (opens the panel) |
| Ctrl/Cmd+B | Toggle the right panel |
| Ctrl/Cmd+L | Focus the composer (closes a screen first) |
| Ctrl/Cmd+Shift+F | Focus the session search |
| Tab, Shift+Tab | Next, previous text field |
| Esc | Close the palette, dismiss the focused prompt card, close a screen, else interrupt the turn |
| 1…9, Enter | Pick and confirm on the focused prompt card |
| Enter, Shift+Enter, Ctrl/Cmd+Z / Shift+Z | Composer: send, new line, undo, redo |

The composer editor leaves Ctrl/Cmd+letter chords other than copy, cut, paste, select all, undo and redo uncaptured, so the shortcuts work while typing. The palette is modal: while it shows, only its own keys apply.

Controls without a key of their own are palette commands: sidebar Tools and Peers ("Show tools", "Show peers"), the hover-only Pin button ("Pin session" / "Unpin session"), each sidebar session ("Go to session: …"), the composer's mode, effort and model pickers ("Mode: …", "Effort: …", "Model: …"), and every area's own commands.

Icon-only buttons carry their label as a tooltip next to the code: the panel toggle (top bar), send and stop, remove queued message (composer), the session tree's expand/collapse chevron.

### Known gaps

- The terminal (libvterm canvas) and pixel surfaces take keys only when focused and expose nothing to assistive technology.
