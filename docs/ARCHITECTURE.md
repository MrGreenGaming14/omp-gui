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
| `loa-config` | core | Central config schema with version and migrations |
| `loa-adapter-testkit` | adapters | Contract test suite, golden fixtures, round trip diff |
| `loa-omp-rpc`, `loa-adapter-omp` | adapters | `omp --mode rpc` client, omp adapter |
| `loa-adapter-claude`, `-codex`, `-opencode`, `-acp`, `-omp2` | adapters | One crate per harness |
| `loa-features` | adapters | omp features provided on top of harnesses that lack them |
| `loa-mcp` | tools | rmcp client, server registry, config import |
| `loa-remote` | tools | `remote` MCP server: `remote_bash`, `remote_file` over russh |
| `loa-omp-bridge` | tools | omp tools served to other harnesses as an MCP server |
| `loa-surface-proto` | surfaces | `omp/surface/1` framing, datagrams, MCP declaration |
| `loa-surface-browser`, `-docs`, `-rfb` | surfaces | Built in surface servers |
| `loa-surface-bench` | surfaces | Input to pixels benchmark, 50 ms gate, video encode |
| `loa-surface-hub` | surfaces | The app's surface service: built-in servers, MCP declared surfaces, `surface_*` agent tools as host tools and a loopback MCP endpoint, `<surface>` drafts |
| `loa-models` | models | Sign in, model listing and settings through omp and other harnesses |
| `loa-projects` | projects | Digests, context ranking, `project_search`, `project_read`, project settings, `ProjectService` over the bus and journal |
| `loa-identity` | p2p | Node ed25519 key in the OS keychain, a `0600` key file in the data directory when no keychain is usable |
| `loa-grants` | p2p | Biscuit grants, scopes, renewal, narrowing |
| `loa-net` | p2p | iroh endpoint on 31415, ALPNs, TCP fallback, relay, discovery |
| `loa-share` | p2p | Session replication, view and drive, fork to own node |
| `loa-node` | p2p | The app's peer service: identity, endpoint, mDNS and tailnet discovery, presence, grant issue/renew/revoke over `omp/control/1`, session sharing over `omp/session/1`, protocols other areas add (e.g. `loa-sandbox/1`, served only to peers proving a covering grant, registered through the app's `NodeSlot`), the Peers panel snapshot and its events |
| `loa-sandbox`, `loa-sandbox-stub` | sandbox | `sandbox_task`, providers, exec stub |
| `loa-updater` | updater | Release feeds, staging, gate checks against the installed omp, swap and rollback; `Updater` service for the Updates screen |

### UI

| Crate | Holds |
| --- | --- |
| `loa-ui-shell` | Sidebar, top bar, right panel frame, themes |
| `loa-ui-chat` | Transcript: streaming markdown, tool rows, virtual list |
| `loa-ui-composer` | Editor, pickers, `/` and `@` popups, stop and queue |
| `loa-ui-decisions` | Approval, question, plan and form cards |
| `loa-ui-panel` | Diff view, file tree, session tree, agents panel |
| `loa-ui-terminal` | Terminal tab, local and SSH |
| `loa-ui-palette` | Command palette |
| `loa-ui-tools` | Tools panel |
| `loa-ui-projects` | Project screen: shared instructions, pinned files, digest model, sessions with digests and exclude toggle |
| `loa-ui-surface` | Surface renderer, Surfaces tab (open tabs, launcher for browser, documents, VNC and MCP surfaces), document pages through `loa-surface-docs` |
| `loa-ui-models` | Sign in, model picker, model settings |
| `loa-ui-peers` | Peers list, grant management |
| `loa-ui-updates` | Updates screen: feeds, staged versions, gate reports with update anyway/pin/wait, swap, rollback, signatures, peers behind |
| `loa-ui-icons` | Lucide line icons as theme-tinted SVG widgets, shared by every UI crate |
| `loa-app` | The binary: wires every crate together |

## Dependency rules

- Contracts depend on nothing in the workspace except `loa-core`
- A library depends on contracts. When it needs something another library owns, it declares a trait for it (a port) and `loa-app` passes the implementation in. Example: `loa-projects` declares the access check it needs and `loa-app` backs it with `loa-grants`
- A UI crate depends on contracts, `iced` and `loa-ui-icons`. It takes plain data in and gives events out. It never spawns a harness, opens a socket or reads the journal
- Only `loa-app`, `loa-features`, `loa-share` and `loa-node` depend on several libraries

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

- Session data arrives as `&loa_core::Dom` or as patches. Other data arrives as plain structs the UI crate defines
- Colors come from the `iced::Theme` extended palette, never from literals, so the dark and light themes apply everywhere
- Standard iced widgets first. A hand built widget carries a label (accessibility readiness, see `docs/accessibility.md`)
- Logic is tested through `update` and through `iced_test` headless runs. Tests never open a window

## Protocols and versions

| Name | Value | Owner |
| --- | --- | --- |
| Element schema | `loa_core::SCHEMA_VERSION` (1) | `loa-core` |
| Adapter interface | `loa_adapter::INTERFACE_VERSION` | `loa-adapter` |
| Control ALPN | `omp/control/1` | `loa-net` |
| Session ALPN | `omp/session/1` | `loa-net`, `loa-share` |
| Surface ALPN | `omp/surface/1` | `loa-surface` |
| Port | 31415, UDP (iroh) and TCP fallback | `loa-net` |

Wire and disk formats are protobuf through `prost` derive macros, with no `protoc` step.

## Local data

SQLite through `rusqlite` with the bundled library. Each crate owns its tables and creates them itself. No crate reads another crate's tables.

## Secrets

- The node's private key lives in the OS keychain
- SSH private keys are never read. Authentication goes through `ssh-agent`
- Provider credentials stay in each harness's own store. The app never sees them
