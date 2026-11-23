# Architecture

How the design in [`docs/design`](design/README.md) maps onto this repository. The design docs say what the app does. This file says where the code lives and which rules every crate follows.

## Workspace

One Cargo workspace, one crate per directory under `crates/`. Every crate is named `loa-*` (local omp app).

### Contracts

Everything else builds on these three. Their existing public items change only with a schema or interface version bump.

| Crate | Holds |
| --- | --- |
| `loa-core` | Element schema v1, `Dom`/`Node`/`NodeId`, `Patch`/`Op`/`Draft`, `PatchSink` (single writer), `MemorySink` |
| `loa-adapter` | `Adapter` (incl. `availability`), `HarnessSession` (incl. `models`, `set_effort`), `Capabilities`, `SessionConfig`, `Registry`, `Availability`, `HarnessModel` |
| `loa-surface` | `SurfaceUri`, `SurfaceInfo`, `Frame`, `InputEvent`, `Surface`, `SurfaceServer` |

### Libraries

| Crate | Area | Holds |
| --- | --- | --- |
| `loa-xml`, `loa-rewind` | core | XML debug export, DOM diff and rewind plans |
| `loa-journal` | core | Append only prost journal, replay, snapshots, fork |
| `loa-bus` | core | Session writer (`PatchSink` impl), publish subscribe with backpressure |
| `loa-settings` | settings | The settings hub: every app and harness setting in one place. `app` is the central config (schema with version, `ConfigStore`, theme reference); `models` holds the model settings and the omp `config.yml`/`models.yml` editors on a comment-preserving YAML editor, the omp agent definition files (`models::agents`: read, validate, write, enable/disable and delete), the workflows (`models::workflows`: `modelRoles` plus their `RULES.md` sections, marker-delimited or hand-written, saved only against the snapshot they were loaded from), the model selector grammar, the omp RPC types, the shared settings error type and the ports the omp side implements: the omp RPC (`rpc::OmpRpc`), provider logins (`login::OmpProviderAccount`), the agent directory (`files::OmpFilesResolver`) and validation (`error::SettingsValidator`); `projects` holds the project settings, digest and overview types; `updates` holds the updater's release, gate, hold and peer status types. Depends only on `loa-util`, `loa-core` and `loa-contracts`. Claude JSON edits normalize whitespace while preserving key order; omp YAML edits retain untouched comments |
| `loa-util` | core | Domain-free helpers: poison-tolerant locks, atomic files, executables (`write_executable`, `retry_busy`, `which` with `PATHEXT` matching on Windows, `resolve`, `probe_cli`, captured `output`), the real home directory and `canonicalize` without Windows verbatim prefixes, time, text, `Registry`, `str_enum!` |
| `loa-harness` | adapters | Harness adapters and account implementations, factory registry, CLI and omp OAuth login strategies, shared configuration editors for Claude and Codex, and the omp implementations of the `loa-settings` ports (`OmpService` RPC, `OmpAccount` provider logins, `OmpLaunch` files resolver, `OmpSettingsValidator`) |
| `loa-session` | sessions | The session domain on any harness: the journal-backed `Library`, `SessionStore`, fork, rewind and compaction, plan mode (`PlanGate`), rules and advisor hooks, transcript helpers. Native session discovery stays in `loa-harness::native` |
| `loa-runtime` | sessions | The headless app runtime: `Runtime` owns the session `Library`, the live `Sessions` (single-writer feeds, `SessionEvent`s), the harness registry (`Harnesses`), the active project and folder and the approval mode; `feed::Source` reads channel and writer feeds. `lifecycle` is the headless lifecycle facade: `Runtime::dispatch(Command)` (start, open, send, spawn, interrupt, close, approval mode, completions) returns a `Transition` of async `Effect`s (harness calls resolving to a `Completion`) and `RuntimeEvent`s; new sessions get their base config and wait for a `Prepared` config before the harness starts. Feed frames use `begin_feed` / `advance_feed`: consumers react to each transition against its post-patch DOM before advancing, with turn reactions once at the end of the frame. No `iced`, UI crates or concrete adapters |
| `loa-mcp` | tools | rmcp client, server registry, config import |
| `loa-remote` | tools | `remote` MCP server: `remote_bash`, `remote_file` over russh |
| `loa-omp-bridge` | tools | omp tools served to other harnesses as an MCP server |
| `loa-surface-proto` | surfaces | `omp/surface/1` framing and datagrams, live H.264/HEVC video (app feature `remote-video`, off by default) |
| `loa-surface-browser`, `-docs`, `-rfb` | surfaces | Built in surface servers |
| `loa-surface-hub` | surfaces | The app's surface service: built-in servers, MCP declared surfaces, `surface_*` agent tools as host tools and a loopback MCP endpoint, `<surface>` drafts |
| `loa-models` | models | The model settings service (`service::Backend`: catalog, sign-in, settings load and save over the `loa-settings` ports) and its offline `testing` fakes. `Backend` is built only from injected `Parts`; `loa-app` composes the real ones from `loa-harness` |
| `loa-projects` | projects | Digests, context ranking, `project_search`, `project_read`, the project settings store (one JSON file per project, and its ideas notes as a Markdown file of the same name beside it, saved over the `loa_util::fs::Snapshot` the page's edit started from so an outside edit is never overwritten), `ProjectService` over the bus and journal; the data types live in `loa_settings::projects`; summaries through the `Summarizers` port, which `loa-app` backs with omp one-shot calls |
| `loa-trust` | p2p | One ed25519 node key (`PublicKey` converts to iroh, Biscuit and bus keys) behind the `KeyStore` port (OS keychain, with a private key file fallback: `0600` on Unix, a protected current-user DACL on Windows); Biscuit grants whose scope kinds (sessions, prompts, projects, future sessions, SSH hosts, surfaces, sandbox, folders) come from one `KINDS` table; renewal, narrowing |
| `loa-net` | p2p | iroh endpoint on 31415, ALPNs, TCP fallback, relay, discovery |
| `loa-share` | p2p | Session replication, view and drive, fork to own node |
| `loa-node` | p2p | The app's peer service: identity, endpoint, mDNS and tailnet discovery, presence, grant issue/renew/revoke over `omp/control/1`, session sharing over `omp/session/1`, shared folders over `omp/folder/1` (list, read, and with a write grant write, confined to the granted folder), protocols other areas add (e.g. `loa-sandbox/1`, served only to peers proving a covering grant, registered through the app's `NodeSlot`), the Peers panel snapshot and its events |
| `loa-sandbox`, `loa-sandbox-stub` | sandbox | `sandbox_task`, providers, exec stub |
| `loa-updater` | updater | Release feeds, staging, gate checks against the installed omp, swap and rollback; `Updater` service for the Updates screen. Its status types live in `loa_settings::updates` |

### Presentation API

| Crate | Holds |
| --- | --- |
| `loa-view` | The stable, versioned presentation API (`loa_view::VERSION`, snapshot in `crates/loa-view/api.txt`). One module per screen family: `theme` (`Appearance`, `ThemeRef`, `Icon`), `settings::{models, projects, peers, updates, agents, workflows}`, `session`, `shell`, `panel`, `terminal`, `surface`. Each holds plain view models (`Clone + Debug + PartialEq`, public fields), the screen's `Intent` (what the UI emits to the app) and, under `port`, the traits the UI needs and the app implements (decoders, document renderer, PTY, file system, notifier, surface transport). Foundation values the UI needs as they are (`Prompt`, `Answer`, `SessionSummary`, surface protocol values, …) are re-exported from the matching module. Depends only on `loa-util`, `loa-core` and `loa-contracts` |

Transcript `*.golden` files and `loa-view/api.txt` use LF on every platform. Recorded inputs and sidecars under `loa-harness/fixtures` retain their exact committed bytes: native import/export preserves raw JSONL and file contents. `.gitattributes` prevents Windows checkout conversion from changing these fixtures.

### UI

| Crate | Holds |
| --- | --- |
| `loa-ui-kit` | Theme token layer (`ThemeRegistry`, `TokenSet`, roles), style recipes, Lucide line icons as theme-tinted SVG widgets for `loa_view::theme::Icon`, shared widgets and the `Component`/`Screen`/`PanelPane` conventions, text helpers |
| `loa-ui-shell` | Sidebar, top bar, right panel frame, themes, command palette (`palette`) |
| `loa-ui-session` | `transcript` (streaming markdown, tool rows, virtual list), `composer` (editor, pickers, `/` and `@` popups, stop and queue), `decisions` (approval, question, plan and form cards) |
| `loa-ui-panel` | Diff view, file tree, session tree, agents panel, tools pane (`tools`) |
| `loa-ui-terminal` | Terminal tab, local and SSH |
| `loa-ui-surface` | Surfaces tab: tab shell over `PixelMode`/`DocumentMode` strategies, a `FrameDecoder` chain and a `DocumentRenderer` it receives through `loa_view::surface::port` (the app supplies the still/H.264 decoders and document rendering from `loa-surfaces`), launcher for browser, documents, VNC and MCP surfaces |
| `loa-ui-settings` | Settings-style screens on kit `Screen` (`Msg<Local, Intent>` + guard): `models` (sign in reducer over `LoginUpdate`, picker, model settings via kit `form_view`/`list_editor` over `OVERRIDE_FIELDS`), `projects` (project screen), `peers` (peers, grants with a typed folder, shared sessions and folders), `updates` (renders `loa_view::settings::updates` status with a `STATUS_RULES` table, `HoldChoice` choices), `agents` (omp agents: the form with templates, model and thinking pickers, tool and spawn checkboxes, prompt body and field errors), `workflows` (the workflow list and editor: role, models, agent access and notes for app-made sections, the raw markdown of hand-written ones, delete confirmation and the reload offered after an outside change). Depends only on `loa-view`, `loa-ui-kit` and `loa-util`; the session model chip is derived from the DOM in `loa-app` (`areas/models/session.rs`) |
| `loa-app` | The binary: wires every crate together |

## Dependency rules

- Contracts depend on nothing in the workspace except `loa-core`
- A library depends on contracts. When it needs something another library owns, it declares a trait for it (a port) and `loa-app` passes the implementation in. Example: `loa-projects` declares the access check it needs and `loa-app` backs it with `loa-trust`
- A UI crate takes `loa_view` view models in and gives `loa_view` intents out. It never spawns a harness, opens a socket or reads the journal. Its direct `loa-*` dependencies are limited to:

  | Crate | Allowed direct `loa-*` dependencies |
  | --- | --- |
  | `loa-view` | `loa-util`, `loa-core`, `loa-contracts` |
  | `loa-ui-kit` | `loa-view` (theme types), `loa-util` |
  | `loa-ui-settings`, `loa-ui-shell`, `loa-ui-terminal`, `loa-ui-surface` | `loa-view`, `loa-ui-kit`, `loa-util` |
  | `loa-ui-panel` | the above and `loa-core` (the agents pane and session tree read the DOM) |
  | `loa-ui-session` | the above and `loa-core` (DOM transcript and decision rendering) and `loa-ui-panel` |

  No UI crate depends directly on `loa-settings`, `loa-contracts`, `loa-surfaces` or any domain, adapter or runtime crate; it imports foundation values only through `loa_view::…`. Check with `cargo tree -e normal --depth 1 -p <crate>`
- Only `loa-app`, `loa-session`, `loa-share` and `loa-node` depend on several libraries
- `loa-settings` depends only on `loa-util`, `loa-core` and `loa-contracts`; it never depends on `loa-models`, `loa-projects`, `loa-updater`, `loa-harness` or a UI crate

### Presentation API and its version

`loa-app` projects domain state into `loa_view` types in `src/view/<screen>.rs` (`to_view(&Domain) -> View`, and `from_view(View) -> Domain` for anything the UI edits) and maps each screen's `Intent` back to domain calls. Every editable type round-trips losslessly, with one app-side test per type asserting `from_view(to_view(x)) == x`.

`crates/loa-view/api.txt` lists every public item of `loa-view` (fields, variants, signatures and re-exported paths); its first line is `version = <loa_view::VERSION>`. `cargo test -p loa-view --test api` renders the listing from `src/` with `syn` and compares:

- The listing changed but `VERSION` was not bumped: the test fails with "bump loa_view::VERSION" and the diff
- Removing or renaming an item or field, or changing a type or signature, is a major bump; additions alone are a minor bump. The test rejects a minor bump when a line was removed or changed
- After bumping, `UPDATE_VIEW_API=1 cargo test -p loa-view --test api` rewrites `api.txt`; it refuses to while `VERSION` still equals the header

### Settings hub

`loa-settings` is the central place for app and harness settings: the central config, the model and provider settings with the omp file editors, the project settings and the updater status. The services that change them (`loa-models`, `loa-projects`, `loa-updater`) import their types from the hub; the settings UI (`loa-ui-settings`) sees only their `loa_view::settings` projections. Harness-specific work goes through the hub's ports (`OmpRpc`, `OmpProviderAccount`, `OmpFilesResolver`, `SettingsValidator`), which the adapters in `loa-harness` implement and `loa-app` wires in

`models::workflows` writes app-managed rules between workflow markers. Description, agent routing notes, ordered steps and named Markdown blocks have explicit field boundaries so they can be read back without mistaking headings in user text for fields. A marked section that cannot be read back losslessly opens as raw Markdown with an outside-edit notice. Copying creates a new draft: managed fields stay structured, raw or hand-written Markdown goes into Notes, and the source stays untouched.

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

Decision answers use the constants in `loa_core::answer` (`allow_once`, `allow_session`, `deny`, `approve`, `edit`, `reject`). Question answers are the picked option labels or the typed text. Form answers are one JSON object.

Imports produce the same shapes with `commit_at` and the original timestamps. Native ids go into `id`, `call_id`, `request_id` and `native_id`. Whatever the mapping doesn't cover is kept in `<ext>` so the export reproduces the native file.

## UI components

Each UI crate exposes components in the same form, so `loa-app` composes them the same way.

```rust
pub struct State { /* … */ }
pub enum Message { /* internal */ }
pub enum Event { /* what the app must act on */ }

impl State {
    pub fn update(&mut self, message: Message) -> (iced::Task<Message>, Option<Event>);
    pub fn view(&self) -> iced::Element<'_, Message>;
    pub fn subscription(&self) -> iced::Subscription<Message>; // only when needed
}
```

- Session data arrives as `&loa_core::Dom` or as patches. Other data arrives as `loa_view` view models, and the screen's output is its `loa_view::<screen>::Intent`; internal `Message`/`Local` types stay private to the UI crate
- Colors, text sizes, spacing and layout widths come from the `loa-ui-kit` theme tokens (`ColorRole`, `TextRole`, `SpaceRole`, `LayoutRole`), never from literals, so runtime themes apply everywhere. `scripts/lint-ui-literals.sh` checks the UI crates and `loa-app`
- Standard iced widgets first. A hand built widget carries a label (accessibility readiness, see `docs/accessibility.md`)
- Logic is tested through `update` and through `iced_test` headless runs. Tests never open a window

## The app

`loa-app` holds the `App`, a `Ctx` every area reads and changes, and the area `Registry` built from one `AREAS` table.

- `Ctx.runtime` is the `loa_runtime::Runtime`, built at startup from `RuntimeParts` (library, harnesses, node, folder, project, approval mode). `services/harness.rs` fills its `Harnesses` from the `loa-harness` factories and builds the composer's picker options
- `Services` splits into `NodeServices` (identity, grants, MCP servers), `Storage` (directories, config) and `Host`. `Host` holds the ports to outside programs: `GitProbe`, `FolderPicker` and `UsageSource`, with command line adapters (git, zenity or kdialog, `omp usage`). Tests pass fakes
- `app/lifecycle.rs`: the `SessionController` hands lifecycle `Command`s to `Runtime::dispatch`, runs the transition's effects as `Task::perform` (completions go back as `Command::Completed`) and maps its events: `PrepareSession` runs every area's `contribute` and `host_tools` (first one kept on duplicates) and dispatches `Prepared`; `Session` events update the chat, detached transcripts, sidebar and status, then every area; `SendRequested` and the app's own sends run the areas' `before_send` first. What the app must do with its own parts (present a folder, reread the diff, run a queued prompt, continue in a new session) is queued as `After` items. `Ctx::close_session` and `discard_session` dispatch `Command::Close` and queue the transition; `App::settle` applies queued transitions before area requests, until both are empty
- `app/workspace.rs`: the folder and project the app works in, recent folders and project tabs
- `view/`: the projections between domain types and `loa_view` view models, one file per screen family, with the round-trip tests
- Shell and composer events go through route tables (`route`, `compose`) to one `Intent` handler
- `app/windows.rs`: Configs, Settings (kit `side_nav`) and detached OS windows. `app/usage.rs`: the Usage window
- `areas/sessions`: `branch` (one `spawn_branch` strategy for fork, rewind and compaction), `plan`, `hooks` (rules and advisor), `transfer` (import and export) and `tree`; model role switches (`/role`), which hold the session's queued prompts until the switch ends and announce the new role in its next prompt
- `areas/projects/history.rs`: the project page discovers saved OMP conversations by the session header's exact working folder; only opening a row imports it into Loa.
- `areas/workflows`: the Workflows page of the Models screen, the composer's workflow chip and the role disclosure every omp session starts with (`SessionConfig::instructions`)
- `testing` (tests only): `TestApp` runs the app headless with its real subscriptions on a scripted harness, with no model, browser, git, dialog or `omp` calls: it hands `App::build` offline `AppParts` (runtime parts, models `Parts`, summarizer, home and URL opener)
- Sandbox integration tests build the real `loa-sandbox-stub` and obtain its executable path from Cargo's JSON artifact output, independent of the compiler's test-artifact directory layout.

## Protocols and versions

| Name | Value | Owner |
| --- | --- | --- |
| Element schema | `loa_core::SCHEMA_VERSION` (1) | `loa-core` |
| Adapter interface | `loa_adapter::INTERFACE_VERSION` | `loa-adapter` |
| Presentation API | `loa_view::VERSION` (3.2.0), listed in `crates/loa-view/api.txt` | `loa-view` |
| Control ALPN | `omp/control/1` | `loa-net` |
| Session ALPN | `omp/session/1` | `loa-net`, `loa-share` |
| Surface ALPN | `omp/surface/1` | `loa-surface` |
| Port | 31415, UDP (iroh) and TCP fallback | `loa-net` |

Wire and disk formats are protobuf through `prost` derive macros, with no `protoc` step.

## Local data

SQLite through `rusqlite` with the bundled library. Each crate owns its tables and creates them itself. No crate reads another crate's tables.

## Secrets

- The node's private key lives in the OS keychain when usable; the private file fallback is under the app data directory
- SSH private keys are never read. Authentication goes through `ssh-agent` (the OpenSSH agent pipe by default on Windows)
- Provider credentials stay in each harness's own store. The app never sees them
