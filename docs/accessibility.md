# Accessibility

How screen reader and assistive technology support stands in iced, what it would take for this app to get it, and the rules UI crates follow meanwhile so the switch stays cheap. Tracks issue #84.

Last checked: 2026-11-19, against upstream `iced-rs/iced` master (`0.15.0-dev`) and `pop-os/iced` master (commit `115018bfb`, 2026-11-11).

## Current state

### Upstream iced

No accessibility support has shipped. iced 0.14.0 (released [2025-12-07](https://github.com/iced-rs/iced/releases/tag/0.14.0)) exposes no accessibility tree, and neither does master as of 2026-11-16. The workspace `Cargo.toml` on master has no `accesskit` dependency.

| Item | State | Dates |
| --- | --- | --- |
| [Issue #552 "Implement accessibility support"](https://github.com/iced-rs/iced/issues/552) | Open, labels `feature`, `help wanted`, `accessibility`, no milestone | Opened 2020-10-05, last activity 2026-02-04 |
| [PR #1849 "WIP: Iced accessibility"](https://github.com/iced-rs/iced/pull/1849) (Ashley Wulber, System76) | Open draft, 56 files. The origin of the pop-os fork's work | Opened 2023-05-11 |
| [PR #3111 "draft: Accesskit integration"](https://github.com/iced-rs/iced/pull/3111) | Open draft, feasibility study. Only `Button` and `Text` expose nodes; the counter example works with VoiceOver on macOS | Opened 2025-11-11 |
| [PR #3281 "Accessibility support"](https://github.com/iced-rs/iced/pull/3281) | Closed unmerged. The maintainer (hecrj) answered "Thanks! But I'll work on this myself." The author keeps the work in a vendored fork | Opened and closed 2026-03-14 |
| [ROADMAP.md](https://github.com/iced-rs/iced/blob/master/ROADMAP.md) | Points to an external graphical roadmap; no dated accessibility milestone | |

Reading: the maintainer intends to build accessibility himself, on his own schedule, and none of the open PRs is on a merge path. Expect it no earlier than a release after 0.15, with an API of its own rather than the pop-os one.

### System76 `pop-os/iced`

The [pop-os/iced](https://github.com/pop-os/iced) fork, used by libcosmic and the COSMIC desktop, has working AccessKit support.

- Crate `iced_accessibility` (in `accessibility/`) wraps AccessKit and its platform adapters (AT-SPI on Linux, UIA on Windows, NSAccessibility on macOS, via `accesskit_winit`).
- Behind the `a11y` cargo feature, which is in the fork's default features together with `wayland`, `x11`, `tiny-skia` and `tokio`.
- `Widget` gains `fn a11y_nodes(&self, layout, state, cursor) -> A11yTree`, plus `id`/`set_id`. Default: no nodes.
- Widgets that implement `a11y_nodes` on master: `button`, `checkbox`, `column`, `container`, `image`, `mouse_area`, `row`, `scrollable`, `slider`, `svg`, `toggler`, `text` (core), and `lazy::component`.
- Widgets without nodes: `text_input`, `text_editor`, `rich_text`, `pick_list`, `combo_box`, `radio`, `tooltip`, `pane_grid`, `canvas`, `shader`, `markdown` and the rest. A screen reader does not see the text fields of this app (composer, palette search) even on the fork.
- Labelling API, only with the feature on: `button(..).name("…")`, `.description("…")`, `.description_widget(&w)`, `.label(&w)`; similar on `checkbox` and `image`.
- Divergence: master is iced `0.14.0` plus 286 commits and 0 behind the `0.14.0` tag, but 746 commits behind upstream master (compare checked 2026-11-19). It pins `winit` to `pop-os/winit` (git rev) and AccessKit to `wash2/accesskit` (git rev), neither from crates.io. Branch `iced-accessibility` holds older work.
- Public API otherwise matches 0.14 for what this app uses: `widget::Id::{new, unique}`, `text_input(..).id(..)`, `scrollable(..).id(..)`, `widget::operation::{focus, scroll_to}`, `event::listen_with`.

## What switching would cost this app

1. A `[patch.crates-io]` block in the root `Cargo.toml` for every `iced*` crate pointing at one pop-os git revision, so `iced_term` and our crates compile against the same fork. Plus the transitive git pins (pop-os `winit`, wash2 `accesskit`).
2. Turning off the fork's default features (`wayland`, `x11` through sctk) and picking ours, then re-checking that the build still needs no system `-dev` packages on our toolchain (AccessKit's Linux adapter talks D-Bus through `zbus`, pure Rust: [INFERENCE] not built here).
3. Upgrades become gated on System76 rebasing the fork. Today it is a full release behind upstream master; an iced 0.15 upgrade would wait for their rebase.
4. Labelling work in our crates: adding `.name(..)` to icon-only buttons and hand built widgets, behind a `cfg(feature = "a11y")` of our own. Code written against upstream 0.14 compiles unchanged on the fork, so this can be incremental.
5. Real gaps remain after switching: text fields, rich text, the terminal and pixel surfaces expose nothing. For a coding app whose core interaction is a text field, that means implementing `a11y_nodes` for `text_input`/`text_editor` in the fork (or upstreaming it there) before switching pays off.

Verdict for now: stay on upstream 0.14 and keep the code ready. Switch only if the fork gains text input nodes and stays within one release of upstream, or adopt upstream's own API when it lands.

## Rules for UI crates

1. Standard iced widgets first. A button is a `button`, a list is a `column` in a `scrollable`, a field is a `text_input` or `text_editor`. Every standard widget gets accessibility when iced (or the fork) adds it, for free.
2. Hand built widgets (custom `Widget` impls, `canvas`, `shader`, anything that draws text itself) carry a doc comment line in this form, so the label and role can be wired in one pass later:

   ```rust
   /// Accessibility: role `list`, label "Changed files"; items are `button`s labelled by path.
   ```

3. Icon-only buttons keep their label next to the code that builds them (a `const` or the tooltip text), never only an image.
4. Every action has a keyboard path: a shortcut in `loa_ui_palette::Keymap`, an entry in the command palette, or focus traversal. No hover-only or drag-only actions.
5. State is never shown by color alone. Pair it with text or an icon that has a label (e.g. the waiting badge says "Waiting").
6. Colors come from the theme's extended palette (already an ARCHITECTURE.md rule), so a high contrast theme is one theme away.
7. Widgets that are focus or scroll targets get a stable `widget::Id`. Both AccessKit node ids and our focus operations key off these.
8. Text is laid out with iced's text widgets wherever possible, not rasterized into images or drawn on a `canvas`, so it can be exposed later.

## Hand built widgets in this app

From the design ([gui.md](design/gui.md), "Notes from the iced mockup") and [ARCHITECTURE.md](ARCHITECTURE.md). Owners keep this table current.

| Widget | Crate | Built from | Accessibility notes |
| --- | --- | --- | --- |
| Command palette | `loa-ui-palette` | Standard widgets only: `text_input`, `scrollable`, `button`, `rich_text`, `mouse_area`, `opaque` | Role: dialog with a search field and a list of buttons labelled by command title. Fully keyboard driven (Ctrl/Cmd+K, Up, Down, Enter, Esc). Highlighting uses weight and underline, not color |
| Diff view | `loa-ui-panel` | Hand built | Needs per-line role and an added/removed label, not color alone |
| Tab bar (right panel) | `loa-ui-shell` / `loa-ui-panel` | Hand built | Role `tablist`; tabs need labels and a selected state |
| `/` and `@` popups | `loa-ui-composer` | Hand built | Role: listbox attached to the composer field |
| Virtualized transcript list | `loa-ui-chat` | Hand built | Role `list`; off-screen items must still be reachable by keyboard |
| Terminal pane | `loa-ui-terminal` | `iced_term` (third party, canvas) | Exposes nothing; depends on `iced_term` upstream |
| Surface view | `loa-ui-surface` | Pixel frames (image/shader) | Pixel surfaces expose nothing; document mode surfaces should render text with iced text widgets |

## Re-check on each iced release

- Release notes and `CHANGELOG.md` of the new upstream version: any `accesskit` dependency, an `a11y`/`accessibility` feature, or new `Widget` trait methods for accessibility.
- Issue #552 and any PR labelled `accessibility`: state, and whether hecrj's own implementation landed.
- `pop-os/iced`: which upstream tag master is rebased on (compare `iced-rs:iced:<tag>...pop-os:iced:master`), whether `text_input`/`text_editor` now implement `a11y_nodes`, and whether `winit`/`accesskit` are still git pins.
- Whether `iced_term` has a release for the new iced version (it blocks any upgrade, fork or not).
- Update "Last checked", the tables above, and the verdict.
