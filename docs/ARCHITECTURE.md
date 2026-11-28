# Architecture

How the design in [`docs/design`](design/README.md) maps onto this repository. The design docs say what the app does. This file says where the code lives and which rules every library follows.

## Layout

One CMake project under `cpp/`, built with Ninja, C++23 and doctest. `cpp/CMakeLists.txt` discovers every `cpp/libs/*`, `cpp/ui/*` and `cpp/tests/*` directory that has a `CMakeLists.txt` (in sorted order) and adds `cpp/app` last.

| Directory | Holds |
| --- | --- |
| `cpp/libs/<name>` | One library: `STATIC` target `loa_<name>` (alias `loa::<name>`), namespace `loa::<name>`, public headers in `include/loa/<name>/`, sources in `src/`, doctest tests in `tests/` built as `loa_<name>_tests` |
| `cpp/ui/<name>` | One UI module: target `loa_ui_<name>`, namespace `loa::ui::<name>`, tests `loa_ui_<name>_tests` |
| `cpp/app` | The `loa-qt` executable and its `loa_app` library: services wiring, areas, windows, smoke screens; tests `loa_app_tests` |
| `cpp/tests/golden` | Golden fixtures for every wire and disk format, with `MANIFEST.md` (what each file is) and `README.md` (provenance); checked by `loa_golden_tests` |
| `cpp/cmake` | `LoaTest.cmake` (`loa_add_test(<name> SOURCES … LINK …)`) |
| `cpp/scripts` | `lint-ui-literals.sh`, the theme lint run by ctest as `loa_ui_lint_literals` |
| `cpp/vcpkg.json` | Dependencies for macOS and Windows; Linux uses the apt packages in the README |

Every library returns `std::expected<T, loa::runtime::Error>` (or its own error type) from fallible calls; exceptions never cross a public API. Async code runs as Asio C++20 coroutines on the `loa::runtime` executor. Abstract classes are the ports; shared implementations are passed as `std::shared_ptr<Port>`.

Heavy third-party headers (Asio, toml++, cpp-httplib, Qt) stay in `.cpp` files or precompiled headers; public headers forward-declare. Each header-only third-party implementation is compiled in exactly one translation unit.

### Foundation and contracts

Everything else builds on these. Their existing public items change only with a schema or interface version bump.

| Library | Holds |
| --- | --- |
| `runtime` | Domain-free foundation: executor and `io_context` threads, `Error`, `log` (level from `LOA_LOG`), `json` (yyjson), the protobuf wire codec `proto`, process spawning and `which`, atomic files, temp files, Myers diff, gitignore matcher, semver, time and text helpers, `Registry`, SSH config parsing |
| `core` | Element schema v1, `Dom`/`Node`/`NodeId`, `Patch`/`Op`/`Draft`, `PatchSink` (single writer), `MemorySink`, XML debug export, DOM diff and rewind plans |
| `contracts` | Harness interface (`Harness`, `HarnessSession`, `Capabilities`, `SessionConfig`, `Availability`, accounts, login channels), host tools and the surface contract (`SurfaceUri`, `SurfaceInfo`, `Frame`, `InputEvent`, `Surface`, `SurfaceServer`) |

### Libraries

| Library | Area | Holds |
| --- | --- | --- |
| `bus` | core | Append-only protobuf journal with shared little-endian framing and CRC32, replay, snapshots, fork; the session writer (`PatchSink` impl) and publish/subscribe with backpressure (`subscribe_seq`, `Policy::every_patch`, `Policy::coalescing`, `Lagged`) |
| `settings` | settings | The settings hub: `app` (central config: schema with version, TOML codec reporting unknown keys by dotted path, `ConfigStore`), `models` (model settings, the omp `config.yml`/`models.yml` editors on a comment-preserving YAML editor, agent definitions, workflows with their `RULES.md` sections, the model selector grammar, omp RPC types, sign-in descriptions and the ports the omp side implements), `projects`, `updates` |
| `harness` | adapters | Harness adapters (`omp`, `omp2`, `claude`, `codex`, `opencode`, `acp`), the shared adapter `kit`, account support and login strategies, native session discovery and import (`native`), the factory `registry` and the offline `testing` fakes (`loa::harness_testing`) |
| `session` | sessions | The session domain on any harness: the journal-backed `Library`, fork, rewind and compaction, plan mode, rules and advisor hooks |
| `app_runtime` | sessions | The headless app runtime: `Runtime` owns the session `Library`, live sessions (single-writer feeds), the harness registry, the active project and folder and the approval mode. `Runtime::dispatch(Command)` returns a `Transition` of async effects and runtime events |
| `mcp` | tools | In-house MCP (JSON-RPC 2.0 over stdio and streamable HTTP): client, server registry, config import, the omp tools `bridge` served to other harnesses, host-tool serving |
| `remote` | tools | The `remote` MCP server: `remote_bash`, `remote_file`, PTYs over libssh with `ssh-agent` auth, known hosts, SQLite audit log |
| `projects` | projects | Digests, context ranking, `project_search`, `project_read`, the project settings store, the project service over the bus and journal; summaries through the `Summarizers` port the app backs with omp one-shot calls |
| `models` | models | The model settings service (catalog, sign-in, sign out, settings load and save over the `settings` ports) and its offline `testing` fakes |
| `surfaces` | surfaces | `proto` (`omp/surface/1` framing and datagrams), `video` (H.264 via openh264, still frames), built-in servers `vnc`, `browser` (CDP over WebSocket, xcb virtual display) and `docs` (QtPdf, xlsx/docx), and the `hub`: built-in servers, MCP-declared surfaces, `surface_*` agent tools and `<surface>` drafts |
| `trust` | p2p | One ed25519 node key behind the `KeyStore` port (libsecret/Keychain/Credential Manager, private key file fallback: `0600` on Unix, current-user DACL on Windows); Biscuit grants whose scope kinds come from one table; renewal and narrowing |
| `p2p` | p2p | iroh endpoint on 31415 (`quic`), TCP+TLS fallback with in-house yamux (`tcp`, `yamux`), ALPN `Router`, mDNS and tailnet discovery, presence, peers database, grant issue/renew/revoke over `omp/control/1`, session sharing over `omp/session/1`, shared folders over `omp/folder/1` |
| `sandbox` | sandbox | `sandbox_task`, providers (local, container, VM), runs, sync; the exec stub executable `loa-sandbox-stub` (`stub/main.cpp`), built into `build/app/` beside `loa-qt` |
| `updater` | updater | Release feeds, signature checks, staging, gate checks against the installed omp, swap and rollback; the `Updater` service for the Updates screen |
| `iroh_ffi`, `biscuit_ffi` | p2p | RAII wrappers over the vendored `iroh-c-ffi` and `biscuit-capi` forks in `fork/`. These are the only non-C++ sources: cargo prebuilds them into `${LOA_RUST_FFI_TARGET:-$HOME/.cache/loa/rust-ffi-target}`, shared across checkouts |

### Presentation API

| Library | Holds |
| --- | --- |
| `view` | The stable, versioned presentation API (`loa::view::VERSION`, snapshot in `cpp/libs/view/api.txt`). One header per screen family: `theme`, `settings_{models,projects,peers,updates,agents,workflows}`, `session`, `shell`, `panel`, `terminal`, `surface`. Each holds plain view models (value types with public fields and `operator==`), the screen's `Intent` (what the UI emits to the app) and, under `port`, the abstract classes the UI needs and the app implements (decoders, document renderer, PTY, file system, notifier, surface transport) |

Transcript `*.golden` files use LF on every platform. Recorded inputs under `cpp/libs/harness/tests/fixtures` and everything under `cpp/tests/golden` keep their exact committed bytes; `.gitattributes` prevents checkout conversion.

### UI

| Module | Holds |
| --- | --- |
| `kit` | Theme token layer (`loa::ui::theme`, mapping the extended palette roles 1:1), style recipes, Lucide line icons as theme-tinted SVG, shared widgets, text helpers, the Qt bridge (`qt.hpp`) and a testkit |
| `shell` | Sidebar, layout (top bar, right panel frame), themes, command palette, keymap |
| `session` | `transcript` (md4c streaming markdown, tool rows, virtual list), `composer` (editor, pickers, `/` and `@` popups, stop and queue, working indicator), `decisions` (approval, question, plan and form cards, libnotify notifications through a port) |
| `panel` | Diff view, file tree, session tree, agents panel, tools pane |
| `terminal` | Terminal tab on libvterm: local `forkpty` (ConPTY on Windows) and SSH channels through the `view` PTY port |
| `surface` | Surfaces tab: pixel and document modes, frame decoders and document renderer received through `loa::view::surface::port`, launcher for browser, documents, VNC and MCP surfaces |
| `settings` | Settings-style screens, one subdirectory each: `models`, `projects`, `peers`, `updates`, `agents`, `workflows` |

## Dependency rules

- Contracts depend on nothing except `runtime` and `core`
- A library depends on contracts. When it needs something another library owns, it declares an abstract class for it (a port) and `cpp/app` passes the implementation in. Example: `projects` declares the access check it needs and the app backs it with `trust`
- A UI module takes `loa::view` view models in and gives `loa::view` intents out. It never spawns a harness, opens a socket or reads the journal. Its direct dependencies are limited to:

  | Target | Allowed direct `loa::*` dependencies |
  | --- | --- |
  | `loa::view` | `loa::runtime`, `loa::core`, `loa::contracts` |
  | `loa::ui_kit` | `loa::view`, `loa::runtime` |
  | `loa::ui_shell`, `loa::ui_settings`, `loa::ui_terminal`, `loa::ui_surface` | `loa::view`, `loa::ui_kit`, `loa::runtime` (and `loa::ui_shell` for its `Task`) |
  | `loa::ui_panel` | the above and `loa::core` (the agents pane and session tree read the DOM) |
  | `loa::ui_session` | the above and `loa::core` and `loa::ui_panel` |

  No UI module links `loa::settings`, `loa::surfaces` or any domain, adapter or runtime library; it imports foundation values only through `loa::view`. Check the `target_link_libraries` lines in `cpp/ui/<name>/CMakeLists.txt`
- Only `cpp/app`, `session` and `p2p` depend on several libraries
- `settings` depends only on `runtime`, `core` and `contracts`; it never depends on `models`, `projects`, `updater`, `harness` or a UI module

### Presentation API and its version

`cpp/app/src/view/` projects domain state into `loa::view` types (`to_view(const Domain&) -> View`, and `from_view(View) -> Domain` for anything the UI edits) and maps each screen's `Intent` back to domain calls. Every editable type round-trips losslessly, with one app-side test per type asserting `from_view(to_view(x)) == x`.

`cpp/libs/view/api.txt` lists every public item of `cpp/libs/view/include/loa/view`; its first line is `version = <loa::view::VERSION>`. The `loa_view_api` ctest renders the listing from the headers and compares:

- The listing changed but `VERSION` was not bumped: the test fails asking to bump `loa::view::VERSION`, with the diff
- Removing or renaming an item or field, or changing a type or signature, is a major bump; additions alone are a minor bump. The test rejects a minor bump when a line was removed or changed
- After bumping, `UPDATE_VIEW_API=1 ./build/libs/view/loa_view_api cpp/libs/view/include/loa/view cpp/libs/view/api.txt` rewrites `api.txt`; it refuses to while `VERSION` still equals the header

### Settings hub

`settings` is the central place for app and harness settings: the central config, the model and provider settings with the omp file editors, the project settings and the updater status. The services that change them (`models`, `projects`, `updater`) import their types from the hub; the settings UI (`cpp/ui/settings`) sees only their `loa::view` settings projections. Harness-specific work goes through the hub's ports (omp RPC, provider accounts, files resolver, settings validator), which `harness` implements and the app wires in.

`settings::models` workflows write app-managed rules between workflow markers. Description, agent routing notes, ordered steps and named Markdown blocks have explicit field boundaries so they can be read back without mistaking headings in user text for fields. A marked section that cannot be read back losslessly opens as raw Markdown with an outside-edit notice. Copying creates a new draft: managed fields stay structured, raw or hand-written Markdown goes into Notes, and the source stays untouched.

## Sessions

- A session changes only through patches. Whoever wants a change hands a `Draft` to the session's `PatchSink`, which assigns the sequence number, timestamp and (for inserts) the node id, applies it, journals it and publishes it
- Sequence numbers start at 1 with no gaps. A fork's journal continues the numbering of its source
- Node ids only go up. An id is never used twice in a session, even after its node is removed
- The root `<session>` is inserted under `NodeId::DOCUMENT`
- Data with no place in the schema goes into an `x-` attribute or an `<ext>` child, never into a new attribute name. Promoting it to the schema is a schema version bump

### DOM conventions for adapters

Every adapter writes the same shapes, so the UI, projects and peers never look at the harness.

| Event | Patches |
| --- | --- |
| Session starts | Insert `<session id harness node created schema cwd project model>` |
| Prompt sent | Insert `<turn status="running" started>` under the session, then `<message role="user" status="done" by>` with the text |
| Assistant text starts | Insert `<message role="assistant" status="streaming" model>` under the turn |
| Token | `append` to the message |
| Thinking | Same as text with `kind="thinking"` |
| Assistant text ends | `set status="done"` |
| Tool call starts | Insert `<call tool call_id status="running" title started>` under the turn with an `<input>` child holding the JSON arguments. Arguments that stream are appended |
| Tool call ends | Insert `<result is_error>` with the output, `<usage>` when known, `set status` to `done` or `error`, `set ended` |
| Harness needs input | Insert `<decision kind status="waiting" request_id …>` under the call it is about (or the turn), `set` the turn `status="waiting"` |
| Decision answered | `set answer`, `by`, `reason`, `status="answered"`, `ended` on the decision, `set` the turn `status="running"` |
| Subagent or background shell | Insert `<job kind status="running" …>`. Output is appended. A subagent's own conversation is a child session linked by `session` |
| Plan items | `<todo state>` under the session. State changes are `set` patches |
| Model or mode changes | `set model`, `model_role`, `effort`, `mode` on the session |
| Harness compacts | Insert `<compaction covers>` under the session with the summary. Covered turns stay in the DOM |
| Turn ends | `set` the turn `status` to `done`, `error` or `cancelled`, `set ended`, insert `<usage>` when known |
| Interrupt | Streaming messages, running calls and waiting decisions become `cancelled`, then the turn does |
| Anything else | `<ext harness type>` with the raw data as text |

OMP progress updates use the subagent's lifecycle ID to append to its job, not a standalone transcript row. Unmatched updates are ignored; legacy OMP `subagent_progress` extension nodes stay in the journal but are not rendered.

Decision answers use the constants in `loa::core::answer` (`allow_once`, `allow_session`, `deny`, `approve`, `edit`, `reject`). Approval decisions that accept session grants advertise `allow_session` in `options` (e.g. `['allow_once', 'allow_session', 'deny']`); approval cards show "Allow for this session" only when `options` includes it, and unattended policies or decision defaults of `allow_session` fall back to `allow_once` when it is absent. Question answers are the picked option labels or the typed text. Form answers are one JSON object.

Imports produce the same shapes with the original timestamps. Native ids go into `id`, `call_id`, `request_id` and `native_id`. Whatever the mapping doesn't cover is kept in `<ext>` so the export reproduces the native file.

## UI components

Each UI module exposes components in the same form, so the app composes them the same way. Per screen, `state.hpp/.cpp` is plain C++ with no Qt:

```cpp
struct State {
    using Message = std::variant</* internal */>;
    using Event = std::variant</* what the app must act on */>;
    std::pair<Task, std::optional<Event>> update(Message message);
};
```

and `view.hpp/.cpp` is a `QWidget` subclass that renders `const State&`, emits Messages through one `message(Message)` signal and never mutates `State`.

- Session data arrives as `const loa::core::Dom&`, as patches or as `loa::view` session view models. Other data arrives as `loa::view` view models, and the screen's output is its `loa::view` `Intent`; internal `Message` types stay private to the module
- Colors, text sizes, spacing and layout widths come from the `loa::ui::theme` tokens, never from literals, so runtime themes apply everywhere. `cpp/scripts/lint-ui-literals.sh` checks `cpp/ui` and `cpp/app` (ctest `loa_ui_lint_literals`)
- Standard Qt widgets first. A custom widget calls `setAccessibleName` (see `docs/accessibility.md`)
- Logic is tested through `State::update` with doctest. Widget tests run under `QT_QPA_PLATFORM=offscreen`; tests never open a visible window

## The app

`cpp/app` holds the `App`, a context every area reads and changes, and the area registry built from one table (`src/areas.cpp`, `src/registry.cpp`).

- `src/application.cpp` and `src/asio_bridge.cpp`: the `QApplication`, and the bridge that runs the `loa::runtime` executor alongside the Qt event loop and posts results back to the UI thread
- `src/services.cpp`: node services (identity, grants, MCP servers), storage (directories, config) and host ports to outside programs (git probe, folder picker, usage source) with command-line adapters; tests pass fakes. `src/adapters/` backs the libraries' ports (models, notifier, peer sessions, sandbox, SSH terminal, summarizer, surfaces, update gates)
- `src/lifecycle.cpp`: the session controller hands lifecycle commands to `Runtime::dispatch`, runs each transition's effects and maps its events to the chat, detached transcripts, sidebar, status and every area; areas contribute session config and host tools before a session starts and run `before_send` hooks before a prompt goes out
- `src/workspace.cpp`, `src/app_workspace.cpp`: the folder and project the app works in, recent folders and project tabs
- `src/view/`: the projections between domain types and `loa::view` view models, one file per screen family
- `src/windows.cpp`, `src/app_windows.cpp`: Configs, Settings and detached OS windows. `src/app/usage.cpp`: the Usage window. `src/app/keys.cpp`: the keymap. `src/app/panes.cpp`: the right panel panes
- `src/areas/<area>/`: `sessions` (branching for fork, rewind and compaction, plan, hooks, transfer, tree, subagents), `projects`, `agents`, `workflows`, `models`, `harness`, `decisions`, `tools`, `sandbox`, `surfaces`, `peers`, `updater`
- `src/smoke.cpp` and `*_smoke.cpp`: `loa-qt --smoke <screen>` builds one screen offscreen and exits; ctest runs every registered screen as `loa_qt_smoke_<screen>`
- `tests/support/test_app.cpp`: runs the app headless on a scripted harness with no model, browser, git, dialog or `omp` calls

## Protocols and versions

| Name | Value | Owner |
| --- | --- | --- |
| Element schema | `loa::core::SCHEMA_VERSION` (1) | `core` |
| Harness interface | `loa::contracts::INTERFACE_VERSION` | `contracts` |
| Presentation API | `loa::view::VERSION` (4.0.0), listed in `cpp/libs/view/api.txt` | `view` |
| Control ALPN | `omp/control/1` | `p2p` |
| Session ALPN | `omp/session/1` | `p2p` |
| Folder ALPN | `omp/folder/1` | `p2p` |
| Surface ALPN | `omp/surface/1` | `surfaces` |
| Port | 31415, UDP (iroh) and TCP fallback | `p2p` |

Wire and disk formats are protobuf, encoded and decoded by hand with `loa::runtime::proto`; there is no `protoc` step. They stay byte-identical with earlier releases of the app, including peers still running them: every format has a golden fixture in `cpp/tests/golden`.

## Local data

SQLite through the system `sqlite3`. Each library owns its tables and creates them itself. No library reads another library's tables.

## Secrets

- The node's private key lives in the OS keychain when usable; the private file fallback is under the app data directory
- SSH private keys are never read. Authentication goes through `ssh-agent` (the OpenSSH agent pipe by default on Windows)
- Provider credentials stay in each harness's own store. The app never sees them

## Platforms

Platform-specific code sits behind `#if` blocks for Linux, macOS and Windows. Only Linux (Ubuntu 24.04) is built and verified in required CI; the macOS and Windows CI jobs build from `cpp/vcpkg.json` and do not gate merges.
