
# Refactor plan for omp-gui: target architecture and execution plan

## Summary

I measured the workspace with `find crates -name '*.rs' | xargs cat | wc -l`. It is **162,183 lines across 52 crates**, and every count in this plan uses that same method.

The target is **22 crates and about 47.9k lines including tests**. Most of the reduction comes from five sources:

1. **Code with no production caller is deleted.** That covers:
   - whole crates: `loa-adapter-omp2`, `loa-adapter-testkit`, the `loa-surface-proto` network protocol, the network-video path in `loa-surface-bench`
   - modules and functions: config migrations and `native.rs`, bus firehose, the journal's hand-written `wire` encoder, models `catalog.rs`, projects `deliver`, XLSX writing, browser bench
   - test files: every `#[ignore]` live test (about 4.5k lines).
2. **Duplicated subsystems are merged onto one shared abstraction.** Six JSON-RPC transports become one `StdioRpc<F: Framing>`. Six turn mappers become one `TurnLedger` plus `DomWriter`. Four tool surfaces become one `Tool`/`ToolSet`. Four key types become one. There is one pixel buffer, one theme token layer instead of 13 `style.rs` files, two settings-type stacks become one, and three login state machines become one.
3. **One shared util crate.** `loa-util` replaces about 98 poison-lock copies, about 8 `write_atomic`, 5 `which`, 5 `ago`, 3 hand-written ISO-calendar modules, plus the hand-rolled base64, DES and SSH-config parsers.
4. **Hand-written codecs become derives.** The sandbox protocol moves to postcard, the omp events to tagged serde enums, the journal to prost only, and the enum string tables to `str_enum!`/strum.
5. **Tests are capped at about 9k lines total.** They become table-driven and use one testkit per layer, with no live or timing tests.

### Global rules for every work package

- Each package ends with `cargo check --workspace --all-targets`, `cargo test --workspace` and `cargo clippy --workspace --all-targets -D warnings` passing.
- Each package ends with `scripts/budget.sh` run and its output recorded.
- Doc comments shrink to one line per public item. Module essays and per-variant docs on error enums are deleted.
- No crate outside `loa-ui-*` or `loa-app` may depend on `iced`.
- No library crate may depend on a `loa-ui-*` crate.

### Conflicts between the cluster plans and how I resolved them

| Conflict | Decision |
| --- | --- |
| Poison-tolerant locks: `parking_lot` or a helper | **`loa_util::sync::LockExt`** (`.locked()`, `.read_ok()`, `.write_ok()`). No new dependency, no Condvar API change, and the edit is mechanical. |
| Where shared helpers live: `loa-exec`, `loa-harness::kit`, or a util crate | **`loa-util`** holds everything that does not depend on the domain. Protocol machinery (rpc framing, ledger, residual) goes in `loa-harness::kit`. `DomWriter` goes in `loa-core`, because sandbox, hub, projects and harness all write drafts. |
| Two harness abstractions: `loa-adapter::Adapter` and the models-side `HarnessAdapter` | Both become traits in **`loa-contracts`**: `Adapter` (sessions) and `HarnessAccount` (sign-in and model config). `loa-harness` implements both per harness and exposes them through `HarnessFactory`. `loa-models` only consumes the trait, so there is no dependency cycle. |
| Three summarizer ports (projects, features, updater) | One `Summarizer` port in `loa-contracts`, implemented once by `loa_harness::omp::OneShot`. |
| Tool layer: in loa-mcp or shared | The `Tool`/`ToolSet`/`ToolOutput` model goes in **`loa-contracts::tools`** with no rmcp. The rmcp `ServerHandler` adapter goes in `loa-mcp::toolkit`. Remote, sandbox, surfaces, projects and the omp bridge register `Tool` implementations. |
| SSH config resolution: remote's parser versus the sandbox scan | `loa_util::ssh` holds the `ssh -G` resolver and the alias scan. Both crates use it, so neither depends on the other. |
| Grants token schema change | **Keep the per-kind Datalog predicates on the wire.** Only the Rust side becomes table-driven (the `ResourceKind` table generates the rules). Stored grants and peer interop are unaffected. |
| Journal on-disk format | **Byte-identical.** The `Framed` codec reproduces the current header layouts. A test fixture of current files is checked in before the refactor. |
| ALPN negotiation | Multi-version negotiation is deleted. The fixed `omp/<name>/1` labels stay, so the wire stays compatible with existing peers. |
| loa-sandbox-stub | Stays a separate binary crate so the static build stays small. Its tests move into loa-sandbox. |
| Firecracker, QEMU and ovnfs | **Kept behind a `vm` feature that is off by default** rather than deleted. They are UI-wired, so deleting them is a product decision; this keeps the feature available. |

### Product decisions to confirm with the user before the flagged packages run

1. **P15 (blocking):** delete `omp/surface/1` networking, network video, the H.264 encode/decode stack and the "Live video" toggle. None of these has a production caller.
2. **P03:** delete the omp2 harness entry. It is never Ready because it targets an unreleased binary.
3. **P13:** replace the hand-written SSH config parser with `ssh -G`. This needs an OpenSSH client on the machine, and `Match` blocks start to work.
4. **P21:** delete `jsonedit`. `~/.claude/settings.json` whitespace gets normalized; key order is kept.
5. **P19:** possible bug, confirm before changing: `Relay::Default` maps to `RelayConfig::Off` at `loa-app/src/areas/peers/mod.rs:110`.

If the user declines item 1, `loa-surfaces` grows by about 2.1k lines behind a `remote-video` feature. The total then lands at about 50k, so other budgets must shrink by the same amount.

## Final crate map

| # | Crate | Replaces | Budget (lines incl. tests) | Test lines (included) |
| --- | --- | --- | ---: | ---: |
| 1 | `loa-util` | loa-exec, helper duplicates everywhere | 650 | 120 |
| 2 | `loa-core` | loa-core, loa-xml, loa-rewind (+ `testkit` feature) | 1,300 | 250 |
| 3 | `loa-contracts` | loa-adapter, loa-surface (+ tools, account, summarizer ports) | 700 | 60 |
| 4 | `loa-bus` | loa-bus, loa-journal | 1,300 | 400 |
| 5 | `loa-config` | loa-config | 550 | 180 |
| 6 | `loa-harness` | adapter-claude/-codex/-opencode/-acp/-omp, loa-omp-rpc, loa-features (omp2 and testkit deleted) | 8,000 | 1,100 |
| 7 | `loa-mcp` | loa-mcp, loa-omp-bridge | 1,500 | 300 |
| 8 | `loa-remote` | loa-remote | 1,800 | 450 |
| 9 | `loa-sandbox` | loa-sandbox | 1,300 | 250 |
| 10 | `loa-sandbox-stub` | loa-sandbox-stub (binary only) | 250 | 0 |
| 11 | `loa-surfaces` | surface-browser, -docs, -rfb, -hub, proto `mcp` module (proto network and bench deleted) | 4,800 | 700 |
| 12 | `loa-trust` | loa-identity, loa-grants | 950 | 150 |
| 13 | `loa-p2p` | loa-net, loa-share, loa-node | 2,200 | 350 |
| 14 | `loa-updater` | loa-updater (omp gate adapters move to the app) | 1,450 | 350 |
| 15 | `loa-models` | loa-models + ui-models backend.rs/bridge.rs/settings types | 2,300 | 400 |
| 16 | `loa-projects` | loa-projects | 1,500 | 250 |
| 17 | `loa-ui-kit` | loa-ui-icons, shell theme.rs, all `style.rs` files, shared widgets | 1,400 | 150 |
| 18 | `loa-ui-shell` | loa-ui-shell, loa-ui-palette | 1,400 | 200 |
| 19 | `loa-ui-session` | loa-ui-chat, -composer, -decisions | 2,700 | 450 |
| 20 | `loa-ui-panel` | loa-ui-panel, loa-ui-tools | 2,300 | 350 |
| 21 | `loa-ui-terminal` | loa-ui-terminal | 1,000 | 200 |
| 22 | `loa-ui-surface` | loa-ui-surface | 1,500 | 250 |
| 23 | `loa-ui-settings` | loa-ui-models (view part), -projects, -peers, -updates | 2,000 | 300 |
| 24 | `loa-app` | loa-app | 5,000 | 700 |
| | **Total** | 52 → 24 directories (22 libraries + app + stub) | **47,850** | **~7,610** |

### Dependency layers

Each layer may only depend on layers below it.

- **L0:** `loa-util`
- **L1:** `loa-core`
- **L2:** `loa-contracts`, `loa-config`
- **L3:** `loa-bus`, `loa-trust`
- **L4:** `loa-harness`, `loa-mcp`, `loa-p2p`
- **L5:** `loa-remote`, `loa-sandbox`, `loa-surfaces`, `loa-updater`, `loa-models`, `loa-projects`
- **L6:** `loa-ui-kit`. Depends on L0 and L2 only, plus `iced`.
- **L7:** the other `loa-ui-*` crates. They depend on kit and on domain types only, and never spawn processes.
- **L8:** `loa-app`

## Core abstractions (Rust signatures)

### `loa-util` (no domain types, no iced)

```rust
pub mod sync {
    pub trait LockExt<T: ?Sized> { fn locked(&self) -> MutexGuard<'_, T>; }
    pub trait RwLockExt<T: ?Sized> {
        fn read_ok(&self) -> RwLockReadGuard<'_, T>;
        fn write_ok(&self) -> RwLockWriteGuard<'_, T>;
    }
}
pub mod fs {
    pub fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()>; // temp+fsync+rename+sync_dir, symlink-aware
    pub fn write_json_atomic<T: Serialize>(path: &Path, v: &T) -> io::Result<()>;
    pub fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>>;
    pub fn sync_dir(dir: &Path) -> io::Result<()>;
    pub struct Snapshot { /* path + observed bytes */ }
    impl Snapshot { pub fn load(p: &Path) -> io::Result<Self>; pub fn save(&mut self, bytes: &[u8]) -> io::Result<bool>; }
    pub fn confine(root: &Path, rel: &Path) -> io::Result<PathBuf>; // symlink-aware containment
    pub fn home() -> Option<PathBuf>; pub fn data_dir(app: &str) -> Option<PathBuf>;
}
pub mod exec {
    pub fn write_executable(p: &Path, b: &[u8]) -> io::Result<()>;
    pub fn retry_busy<T>(f: impl FnMut() -> io::Result<T>) -> io::Result<T>;
    pub fn which(name: &str) -> Option<PathBuf>;
    pub fn is_executable(p: &Path) -> bool;
    pub fn parse_version(text: &str) -> Option<semver::Version>;
    pub async fn probe_cli(exe: &Path, args: &[&str], timeout: Duration) -> io::Result<String>;
    pub async fn run_checked(cmd: tokio::process::Command) -> Result<String, String>;
    pub struct StderrTail { /* bounded ring */ }
    impl StderrTail { pub fn spawn(r: impl AsyncRead + Unpin + Send + 'static, lines: usize) -> Self; pub fn joined(&self) -> String; }
    pub fn shell_quote(s: &str) -> String; pub fn kvm_available() -> bool;
}
pub mod time { pub fn now_ms() -> i64; pub fn now_us() -> u64; pub fn ago(now_ms: i64, then_ms: i64) -> String;
               pub fn until(ms: i64) -> String; pub fn parse_iso_ms(s: &str) -> Option<i64>; pub fn format_iso_ms(ms: i64) -> String; } // backed by `time`
pub mod text { pub fn clip(s: &str, max: usize) -> &str; pub fn first_line(s: &str, max: usize) -> String;
               pub fn floor_boundary(s: &str, i: usize) -> usize; pub fn ceil_boundary(s: &str, i: usize) -> usize;
               pub fn percent_encode(s: &str) -> String; pub fn random_hex(bytes: usize) -> String; pub fn mime_of(p: &Path) -> &'static str; }
pub mod ssh { pub fn aliases(config: &Path) -> Vec<String>; pub fn resolve(alias: &str) -> io::Result<ResolvedHost>; /* `ssh -G` */ }
pub mod registry {
    pub struct Registry<A: ?Sized> { by_id: BTreeMap<String, Arc<A>> }
    impl<A: ?Sized> Registry<A> { pub fn register(&mut self, id: impl Into<String>, a: Arc<A>); pub fn get(&self, id: &str) -> Option<Arc<A>>; pub fn iter(&self) -> impl Iterator<Item=(&str,&Arc<A>)>; }
}
#[macro_export] macro_rules! str_enum { /* Name { Variant = "wire", .. } => ALL, COUNT, as_str, FromStr, Display, serde */ }
/// Semantic state for theming; domain status enums implement `fn semantic(&self) -> Semantic`.
pub enum Semantic { Ok, Warn, Error, Muted, Busy, Accent, Neutral }
#[cfg(feature = "testkit")] pub mod testkit { pub async fn eventually(f: impl FnMut() -> bool, t: Duration); pub fn fake_program(dir: &Path, name: &str, sh: &str) -> PathBuf; }
```

### `loa-core`

```rust
impl Dom {
    pub fn check(&self, p: &Patch) -> Result<(), ApplyError>;
    pub fn apply(&mut self, p: &Patch) -> Result<(), ApplyError> { self.check(p)?; self.mutate(p); Ok(()) }
}
schema! { Session = "session" under [] { title: Str, status: Enum(SessionStatus) req, .. } .. } // emits ElementKind, specs, attr::* (names unchanged)
impl From<NodeId> for Value;                       // replaces 18 node_int copies
pub trait IntoAttrs { fn into_attrs(self) -> Attrs; }  #[macro_export] macro_rules! attrs { ($($k:ident: $v:expr),*) => .. }
pub struct DomWriter<'a> { sink: &'a dyn PatchSink }
impl DomWriter<'_> {
    pub fn insert(&self, parent: NodeId, kind: ElementKind, attrs: impl IntoAttrs, text: impl Into<String>) -> Result<NodeId, SinkError>;
    pub fn set(&self, node: NodeId, attr: &str, v: impl Into<Value>) -> Result<(), SinkError>;
    pub fn close(&self, node: NodeId, status: impl Into<Value>) -> Result<(), SinkError>; // status + ended=now_ms
    pub fn ext(&self, parent: NodeId, harness: &str, kind: &str, raw: &serde_json::Value) -> Result<NodeId, SinkError>;
}
pub mod xml { pub fn write(dom: &Dom, out: &mut impl fmt::Write) -> fmt::Result; }
pub mod rewind { pub fn plan(dom: &Dom, at: NodeId) -> RewindPlan; }
#[cfg(feature = "testkit")] pub mod testkit { pub struct DomFixture; /* .session().turn().message(role,text).call(tool, attrs).decision(kind).job() */
    pub struct AllowAll; pub struct DenyAll; pub struct MemoryPersist; }
```

### `loa-contracts` (contracts; bump `INTERFACE_VERSION` once in P05)

```rust
pub mod harness {  // former loa-adapter, unchanged trait shapes
    #[async_trait] pub trait Adapter: Send + Sync { /* id, capabilities, availability, start, import, export */ }
    #[async_trait] pub trait HarnessSession: Send + Sync { /* send, interrupt, answer, set_model, set_effort, models, wait_idle, shutdown, state */ }
    pub struct Capabilities { .. } impl Capabilities { pub const fn builder() -> CapsBuilder; }
    #[async_trait] pub trait HarnessAccount: Send + Sync {
        async fn sign_in_state(&self) -> SignInState;
        fn login(&self, kind: LoginKind) -> Result<Box<dyn LoginStrategy>, AdapterError>;
        fn logout_command(&self) -> Option<CommandSpec>;
        fn efforts(&self) -> &'static [&'static str];
        fn read_model(&self) -> Result<HarnessModel, AdapterError>;
        fn write_model(&self, m: &HarnessModel) -> Result<(), AdapterError>;
    }
    pub trait HarnessFactory: Send + Sync {
        fn id(&self) -> &'static str; fn label(&self) -> &'static str; fn icon_token(&self) -> &'static str; // e.g. "harness.claude"
        fn default_program(&self) -> &'static str;
        fn build(&self, exe: Option<PathBuf>) -> Arc<dyn Adapter>;
        fn account(&self, exe: Option<PathBuf>) -> Option<Arc<dyn HarnessAccount>>;
        fn traits(&self) -> HarnessTraits; // approval_modes, has_roles, server_url — replaces `== "omp"` checks
    }
    #[async_trait] pub trait LoginStrategy: Send { async fn run(self: Box<Self>, ctl: mpsc::Receiver<LoginControl>, out: mpsc::Sender<LoginUpdate>); }
    pub enum LoginUpdate { Url(String), Progress(String), Prompt { message: String, placeholder: Option<String> }, Succeeded(Option<String>), Failed(String), Cancelled }
}
pub mod surface { /* SurfaceUri, SurfaceInfo, Frame, InputEvent, Surface, SurfaceServer — former loa-surface */ }
pub mod tools {
    pub enum OutputBlock { Text(String), Image { mime: String, data: Vec<u8> } }
    pub struct ToolOutput { pub blocks: Vec<OutputBlock>, pub is_error: bool }
    #[async_trait] pub trait Tool: Send + Sync + 'static {
        type Args: DeserializeOwned + schemars::JsonSchema + Send;
        const NAME: &'static str;
        fn description(&self) -> &str;
        fn annotations(&self) -> ToolAnnotations { Default::default() }
        async fn run(&self, ctx: &CallCtx, args: Self::Args) -> Result<ToolOutput, String>;
    }
    pub struct ToolSet { tools: BTreeMap<&'static str, Arc<dyn DynTool>> }
    impl ToolSet { pub fn register<T: Tool>(&mut self, t: T) -> &mut Self; pub fn specs(&self) -> Vec<HostToolSpec>;
                   pub async fn call(&self, ctx: &CallCtx, name: &str, args: serde_json::Value) -> Result<ToolOutput, String>; }
    impl HostTools for ToolSet { .. } // the existing host-tool port is implemented once here
}
#[async_trait] pub trait Summarizer: Send + Sync { async fn summarize(&self, instructions: &str, input: &str) -> Result<String, String>; }
```

### `loa-bus` (bus + journal)

```rust
impl SessionWriter { pub fn request(&self, peer: &NodeKey, expected_seq: Option<u64>, d: Draft, ts: i64) -> Result<Patch, RequestError>; pub fn replicate(..); }
pub struct Queue { state: Mutex<State>, notify: Notify }  pub struct Subscription { q: Arc<Queue> } // non-generic, coalescing
pub trait Persist; pub trait Authorize;                    // ports unchanged
pub trait FramedHeader: prost::Message + Default { const MAGIC: [u8; 8]; const VERSION: u32; }
pub fn write_framed<H: FramedHeader>(w: &mut impl Write, h: &H) -> io::Result<()>; pub fn read_framed<H: FramedHeader>(r: &mut impl Read) -> Result<H, JournalError>;
pub trait SyncStrategy: Send { fn after_append(&mut self, pending: usize, oldest: Instant) -> bool; } // SyncPolicy implements it
pub struct JournalPersist; // Persist adapter
```

### `loa-config`

```rust
pub struct Appearance { pub theme: ThemeRef, #[serde(flatten)] pub overrides: toml::Table } // replaces enum Theme; "system"/"dark"/"light" accepted
pub struct ThemeRef(pub String); // registry name or path
struct Rule { key: &'static str, check: fn(&Config) -> Result<(), String> }  static RULES: &[Rule];
impl ConfigStore { pub fn edit<R>(&self, f: impl FnOnce(&mut Config) -> R) -> Result<R, ConfigError>; pub fn watch(&self) -> watch::Receiver<u64>; }
```

### `loa-harness` (feature per harness: `omp`, `claude`, `codex`, `opencode`, `acp`; `features` module always on)

```rust
pub mod kit {
    pub enum Incoming { Response { id: String, result: Result<Json, RpcError> }, Request { id: Json, method: String, params: Json }, Notification { method: String, params: Json } }
    pub trait Framing: Send + Sync + 'static { fn request(&self, id: u64, method: &str, params: Json) -> Json; fn classify(&self, msg: Json) -> Incoming; }
    // impls: JsonRpc2 (acp, codex), OmpFrames (omp, chunk reassembly), ClaudeControl
    #[async_trait] pub trait Inbound: Send + Sync { async fn on_request(&self, method: &str, params: Json) -> Result<Json, RpcError>; fn on_notification(&self, method: &str, params: Json); fn on_closed(&self, status: Option<ExitStatus>); }
    pub struct StdioRpc<F: Framing> { .. }
    impl<F: Framing> StdioRpc<F> { pub fn spawn(cmd: Command, f: F, inbound: Arc<dyn Inbound>) -> Result<(Arc<Self>, ChildGuard), AdapterError>;
        pub async fn request<T: DeserializeOwned>(&self, method: &str, params: Json) -> Result<T, RpcError>;
        pub fn notify(&self, method: &str, params: Json); pub fn respond(&self, id: Json, r: Result<Json, RpcError>); }
    pub struct TurnLedger<K: Hash + Eq> { turn: Option<OpenTurn<K>>, closed: bool }
    impl<K: Hash + Eq> TurnLedger<K> { pub fn state(&self) -> SessionState; pub fn begin(&mut self, w: &DomWriter, p: &Prompt) -> Result<NodeId>;
        pub fn end_turn(&mut self, w: &DomWriter, status: TurnStatus, hook: impl FnMut(EndEvent)); pub fn from_dom(dom: &Dom) -> Self; }
    pub trait EventMapper: Send { type Event; type Reply;
        fn apply(&mut self, ev: Self::Event, w: &DomWriter) -> Vec<Self::Reply>;
        fn answer(&mut self, d: NodeId, a: &Answer, w: &DomWriter) -> Result<Option<Self::Reply>, AdapterError>;
        fn ledger(&self) -> &TurnLedger<String>; }
    #[async_trait] pub trait HarnessOps: Send + Sync { async fn send(&self, p: &Prompt) -> Result<()>; async fn interrupt(&self); async fn set_model(&self, sel: &str) -> Result<()>; }
    pub struct DrivenSession<M: EventMapper, O: HarnessOps> { mapper: Mutex<M>, ops: O, state: watch::Sender<SessionState> } // impl HarnessSession once
    pub trait NativeCodec { fn import(p: &Path, sink: &dyn PatchSink) -> Result<()>; fn export(dom: &Dom, out: &Path) -> Result<()>; fn discover(home: &Path) -> Vec<NativeSession>; }
    pub mod residual { pub fn take(obj: &mut Map, keep: &[&str]) -> Map; pub fn restore(node: &Node, harness: &str) -> Map; pub fn split_parts(..); pub fn join_parts(..); }
}
pub fn factories() -> Vec<Arc<dyn HarnessFactory>>;   // registry.rs
pub mod omp { pub mod rpc; /* 18 used commands, tagged events */ pub struct OneShot; /* impl Summarizer */ }
pub mod features { /* Library, fork, rewind, compact, select_role, plan, hooks */ }
#[cfg(feature = "testing")] pub mod testing { pub struct WatchSink; pub struct FakeProcess; pub fn check_dom(d: &Dom) -> Vec<Violation>; pub async fn replay<M: EventMapper>(m: M, fixture: &Path); }
```

### Tools, surfaces, p2p, updater, models, projects

```rust
// loa-mcp
pub struct ToolSetMcp(Arc<ToolSet>); impl rmcp::ServerHandler for ToolSetMcp {..}
impl From<CallToolResult> for ToolOutput; impl From<ToolOutput> for CallToolResult;
pub trait ImportSource { fn app(&self) -> App; fn path(&self) -> &Path; fn read(&self) -> Result<Vec<Found>, String>; }
pub trait ConfigValue { fn str(&self, k: &str) -> Option<&str>; fn strings(&self, k: &str) -> Vec<String>; fn map(&self, k: &str) -> BTreeMap<String, String>; } // json + toml
impl Registry { async fn commit(&self, f: impl FnOnce(&mut Vec<ServerSpec>) -> Result<()>) -> Result<()>; }

// loa-remote
pub trait AuditSink: Send + Sync { fn record(&self, r: NewRecord) -> Result<i64>; fn query(&self, q: &AuditQuery) -> Result<Vec<AuditRecord>>; }
pub struct PromptQueue; impl PromptQueue { pub fn answer<R: 'static + Send>(&self, id: u64, reply: R) -> bool; pub fn pending(&self) -> Vec<PromptView>; }
pub struct PromptView { pub kind: PromptKind, pub action: String, pub target: String, pub detail: Option<String>, pub fingerprint: Option<String>, pub previous: Vec<String> }

// loa-sandbox
pub trait ProviderFactory: Send + Sync { fn kind(&self) -> SandboxKind; fn probe(&self, m: &Machine, c: &SandboxConfig) -> ProviderStatus; fn build(&self, m: &Machine, c: &SandboxConfig) -> Option<Arc<dyn SandboxProvider>>; }
pub trait Teardown: Send + Sync { fn argv(&self) -> Option<std::process::Command>; }
pub trait CheckoutBackend: Send + Sync { fn name(&self) -> &'static str; fn create(&self, src: &Path, dst: &Path) -> Result<()>; fn remove(&self, dst: &Path) -> Result<()>; }

// loa-surfaces
#[async_trait] pub trait SurfaceProvider: Send + Sync { fn name(&self) -> &'static str; fn list(&self) -> Vec<SurfaceInfo>;
    async fn open(&self, req: &OpenRequest) -> Result<Arc<dyn Surface>>; async fn close(&self, uri: &SurfaceUri) -> Result<()>; async fn shutdown(&self); }
pub struct RgbaImage { pub w: u32, pub h: u32, pub px: Vec<u8> } // blit/crop/fill/copy_rect; encode_png; decode_jpeg; png_size
pub struct FrameFeed { .. } impl FrameFeed { pub fn publish(&self, r: Rect, enc: Encoding, data: Bytes, key: bool) -> Arc<Frame>; pub fn republish_keyframe(&self) -> bool; }
pub trait Capture: Send + Sync { fn frames(&self) -> broadcast::Receiver<Arc<Frame>>; fn request_keyframe(&self); }
pub trait Document: Send + Sync { fn pages(&self) -> usize; fn render(&self, page: usize, opts: &RenderOpts) -> RgbaImage; fn text(&self) -> String; fn query(&self, q: &DataQuery) -> Option<Result<Value>> { None } }
pub struct DocFormat { kind: Format, mime: &'static str, exts: &'static [&'static str], sniff: fn(&[u8]) -> bool, open: fn(&[u8], &Fonts) -> Result<Box<dyn Document>> } static FORMATS: &[DocFormat];
pub struct RenderOpts { pub palette: DocPalette, pub typography: DocTypography } // injectable, defaults = today's constants
#[async_trait] pub trait RectDecoder: Send { fn encoding(&self) -> i32; async fn decode(&mut self, r: Rect, rd: &mut (dyn AsyncRead + Unpin + Send), fb: &mut RgbaImage) -> Result<Damage>; }

// loa-trust
pub struct PublicKey([u8; 32]); // derives; From/Into iroh::PublicKey, biscuit PublicKey, loa_bus::NodeKey
pub trait KeyStore { fn load(&self) -> Result<Option<[u8;32]>>; fn store(&self, s: &[u8;32]) -> Result<()>; fn delete(&self) -> Result<()>; }
pub struct FallbackStore<P: KeyStore, F: KeyStore>; // decorator
pub struct ResourceKind { name: &'static str, actions: &'static [Action], encode: fn(&Resource) -> String, decode: fn(&str) -> Result<Resource>, rule: &'static str } static KINDS: [ResourceKind; 8];

// loa-p2p
#[async_trait] pub trait Transport: Send + Sync { fn kind(&self) -> TransportKind; async fn dial(&self, peer: &NodeId, hints: &[SocketAddr], alpn: &[u8]) -> Result<Connection>; async fn dial_any(&self, addr: SocketAddr, alpn: &[u8]) -> Result<Connection>; fn close(&self); }
pub struct FallbackDialer { transports: Vec<Arc<dyn Transport>>, first_timeout: Duration }
#[async_trait] pub trait Discoverer: Send { async fn next(&mut self) -> Option<NearbyPeer>; fn availability(&self) -> Availability; }
pub async fn call<Req: prost::Message, Rep: prost::Message + Default>(s: &mut Stream, req: Req) -> Result<Rep, NodeError>;
pub trait Envelope: prost::Message + Default { fn ack(err: Option<String>) -> Self; fn into_ack(self) -> Result<Self, NodeError>; }
pub trait GrantGate { fn live_grants(&self, peer: &NodeId, now: i64) -> Vec<VerifiedGrant>; fn allows(&self, peer: &NodeId, req: &Request, now: i64) -> bool; }

// loa-updater
#[async_trait] pub trait FeedSource: Send + Sync { async fn fetch(&self, p: &Product, cached: Option<&CachedFeed>) -> Result<FeedResponse>; }
pub trait Unpacker { fn unpack(&self, artifact: &Path, payload: &Path) -> Result<()>; } // TarGz{strip}, Zip, Raw
// gate ports unchanged: ToolCatalog, FormatProbe, RpcProbe, ContractRunner, ChangelogSummarizer (omp impls live in loa-app)

// loa-models
pub struct Selector { provider: String, id: String, level: Option<String> } // the single selector grammar
pub trait ConfigDoc: Sized { fn parse(s: &str) -> Result<Self>; fn get(&self, path: &[&str]) -> Option<String>; fn set(&mut self, path: &[&str], v: Option<&str>) -> Result<()>; fn render(&self) -> String; }
pub fn edit_file<D: ConfigDoc>(snap: &mut Snapshot, f: impl FnOnce(&mut D) -> Result<()>) -> Result<bool>;
pub struct FieldSpec { key: &'static str, label: &'static str, kind: FieldKind } pub static OVERRIDE_FIELDS: &[FieldSpec];
pub struct ModelService; // former ui-models backend; returns mpsc::Receiver streams, no iced

// loa-projects
pub trait Scorer { fn score(&self, q: &[String], c: &Candidate) -> f64; } pub struct Weighted(Vec<(f64, Box<dyn Scorer>)>);
```

### UI and app

```rust
// loa-ui-kit (UI component port)
pub trait Component { type Message: Clone; type Event;
    fn update(&mut self, m: Self::Message) -> (Task<Self::Message>, Option<Self::Event>);
    fn view(&self) -> Element<'_, Self::Message>;
    fn subscription(&self) -> Subscription<Self::Message> { Subscription::none() } }
pub enum Msg<L, E> { Local(L), Emit(E) } // convention for every screen crate
pub struct Picker<T: PickerItem>; pub struct FormState<F: FormSpec>; pub struct VirtualList; // Fenwick heights shared by chat, diff, palette
pub trait PanelPane { fn id(&self) -> &str; fn title(&self) -> &str; fn icon(&self) -> Icon; fn view(&self) -> Element<'_, PaneMsg>; }

// loa-ui-session
pub trait Choice: Copy + 'static { const ALL: &'static [Self]; fn answer(self) -> &'static str; fn label(self) -> &'static str; fn past(self) -> &'static str; }
pub trait CardKind { fn body<'a>(&'a self, c: &'a Card, cx: &CardCtx) -> Element<'a, Message>; fn confirm(&self, c: &mut Card) -> Option<Reply>; fn dismiss(&self, c: &mut Card) -> Option<Reply>; }
pub trait RowRenderer { fn view<'a>(&self, item: &'a Item, cx: &RowCtx<'a>) -> Element<'a, Message>; fn estimate(&self, item: &Item, width: f32) -> f32; }
pub trait Notifier: Send + Sync { fn notify(&self, n: &Notice) -> BoxFuture<'static, Result<(), NotifyError>>; }

// loa-ui-terminal
pub trait PtyBackend { fn open(&self, t: &Target, size: GridSize) -> BoxFuture<'static, Result<PtyChannel>>; }

// loa-app
pub struct AreaMsg(Arc<dyn Any + Send + Sync>); // Clone + Debug
pub trait AreaMessage: Clone + Debug + Send + Sync + 'static { const AREA: &'static str; }
pub trait AreaDyn {
    fn id(&self) -> &'static str;
    fn update(&mut self, m: AreaMsg, cx: &mut Ctx) -> Task<AreaMsg>;
    fn commands(&self) -> &'static [CommandSpec];
    fn tabs(&self) -> &[PanelTab];
    fn view_tab<'a>(&'a self, id: &str, cx: &'a Ctx) -> Option<Element<'a, AreaMsg>>;
    fn view_screen<'a>(&'a self, cx: &'a Ctx) -> Option<Element<'a, AreaMsg>>;
    fn hooks(&mut self) -> Option<&mut dyn SessionHooks>;
    fn subscription(&self) -> Subscription<AreaMsg>;
}
pub struct Typed<A: Area>(A); impl<A: Area> AreaDyn for Typed<A> { .. } // downcast, debug_assert on mismatch
pub struct CommandSpec { pub id: &'static str, pub title: &'static str, pub section: Section, pub slash: Option<&'static str>, pub keys: Option<&'static str> }
pub fn watch<T: Clone + Send + 'static, M>(key: impl Hash, rx: impl FnOnce() -> broadcast::Receiver<T> + Send + 'static, map: fn(Result<T, Lagged>) -> Option<M>) -> Subscription<M>;
pub fn perform<T, E: Display, M>(f: impl Future<Output = Result<T, E>> + Send + 'static, ok: fn(T) -> M, err: fn(String) -> M) -> Task<M>;
pub trait GitProbe; pub trait FolderPicker; pub trait UsageSource; // CLI adapters plus test fakes
```

## Theme layer (`loa-ui-kit::theme`)

The goal is that a future theming scripting language only has to produce **data** (a `TokenSet` plus `StyleSpec` overrides) through a `ThemeSource`. No widget code changes when it lands. The scripting language itself is out of scope; only the seam is built.

```rust
roles! { ColorRole { Ground="ground", Side="side", Card="card", Raised="raised", Hover="hover", Selected="selected",
                     Line="line", LineStrong="line.strong", Text="text", Text2="text.2", Text3="text.3", Accent="accent",
                     OnAccent="on.accent", Ok="ok", Warn="warn", Danger="danger", AddedBg="diff.added", RemovedBg="diff.removed",
                     CursorRing="cursor.ring", Term0..Term15="term.N" } }
roles! { TextRole { Title, Heading, Body, Label, Caption, Mono } }   // sizes + FontSpec
roles! { SpaceRole { Xs, S, M, L, Xl, Gutter } }  roles! { RadiusRole { Chip, Card, Pill, Dot } }
roles! { AlphaRole { Muted, Disabled, Tint } }    roles! { LayoutRole { ComposerWidth, NavWidth, ListWidth, PopupWidth, ContentWidth, LabelColumn } }
// roles! = str_enum + COUNT; every role has a stable dotted name used by overrides/scripts.

pub struct TokenSet {
    colors: [Color; ColorRole::COUNT], text: [(f32, FontSpec); TextRole::COUNT], space: [f32; SpaceRole::COUNT],
    radii: [f32; RadiusRole::COUNT], alphas: [f32; AlphaRole::COUNT], layout: [f32; LayoutRole::COUNT],
    styles: HashMap<StyleKey, StyleStates>,       // data-driven widget recipes
}
impl TokenSet {
    pub fn color(&self, r: ColorRole) -> Color; pub fn text(&self, r: TextRole) -> f32; pub fn space(&self, r: SpaceRole) -> f32; /* ... */
    pub fn palette(&self) -> iced::theme::Palette;          // iced palette DERIVED from tokens
    pub fn apply(&mut self, key: &str, v: TokenValue) -> Result<(), ThemeError>;   // "color.accent", "style.button.ghost.hover.bg"
    pub fn apply_table(&mut self, t: &toml::Table) -> Vec<ThemeError>;          // config `[appearance]` overrides
}
pub enum TokenValue { Color(Color), Number(f32), Ref(String), Alpha { base: String, alpha: f32 } }
pub struct StyleKey { component: &'static str, part: &'static str }  // "button.ghost", "sidebar.row", "card", "badge"
pub struct StyleSpec { bg: Option<Paint>, fg: Option<Paint>, border: Option<(Paint, f32)>, radius: Option<RadiusRole> }
pub struct StyleStates { base: StyleSpec, hover: Option<StyleSpec>, pressed: Option<StyleSpec>, selected: Option<StyleSpec>, disabled: Option<StyleSpec> }
pub enum Paint { Role(ColorRole), Alpha(ColorRole, AlphaRole), Literal(Color) }

pub trait ThemeSource: Send + Sync {
    fn id(&self) -> &str; fn label(&self) -> &str;
    fn variants(&self) -> &[Variant];                       // Light, Dark
    fn tokens(&self, v: Variant) -> Result<TokenSet, ThemeError>;
}
pub trait ThemeLoader: Send + Sync {                        // future: script loader registers here
    fn can_load(&self, r: &ThemeRef) -> bool;
    fn load(&self, r: &ThemeRef) -> Result<Arc<dyn ThemeSource>, ThemeError>;
}
pub struct ThemeRegistry { sources: Registry<dyn ThemeSource>, loaders: Vec<Box<dyn ThemeLoader>> }
impl ThemeRegistry { pub fn builtin() -> Self /* "system","dark","light" via BuiltinTheme */; pub fn resolve(&self, r: &ThemeRef) -> Result<Arc<dyn ThemeSource>, ThemeError>; pub fn list(&self) -> Vec<(String, String)>; }

static ACTIVE: RwLock<(u64, Arc<TokenSet>)>;  // generation + tokens; iced style fns only receive &Theme
pub fn tokens() -> Arc<TokenSet>; pub fn generation() -> u64; pub fn set_active(t: TokenSet) -> u64;

pub mod style {   // recipes read StyleStates from active tokens => overridable by data
    pub fn button(k: ButtonKind) -> impl Fn(&Theme, button::Status) -> button::Style;   // Primary, Accent, Secondary, Ghost, Row{selected}, Tab{active}, Flat, Danger
    pub fn container(s: Surface) -> impl Fn(&Theme) -> container::Style;               // Card, Popup, Raised, Well, ErrorBox, Badge
    pub fn text(t: Tone) -> impl Fn(&Theme) -> text::Style;                            // Normal, Muted, Faint, Ok, Warn, Danger, Accent
    pub fn tint(t: Tone) -> impl Fn(&Theme, svg::Status) -> svg::Style;
    pub fn input() -> ..; pub fn pick() -> ..; pub fn scrollbar(thin: bool) -> ..; pub fn rule() -> ..;
    pub fn semantic(s: loa_util::Semantic) -> Tone;                                    // domain status → tone
}
pub fn icon(i: Icon, size: TextRole, tone: Tone) -> Element<'static, M>;
```

### How a theme is resolved at runtime

1. `loa-config` stores `Appearance { theme: ThemeRef, overrides }`.
2. `loa-ui-shell::theme::State` resolves the ref: `registry.resolve(ref)?.tokens(variant)`, then `apply_table(overrides)`, then `set_active(..)`.
3. The shell emits `Event::ThemeChanged(generation)`.
4. Components that cache per-theme data compare the generation and rebuild. These are the chat `Look` and markdown settings, the decisions markdown settings, and the terminal `TermPalette` (ANSI colours come from the `term.N` roles, with no hue rotation).
5. `ConfigStore::watch()` drives hot reload, so a future script file can be re-evaluated on change.
6. The theme picker lists `ThemeRegistry::list()`, so script themes appear in it automatically.

### Rules that keep the seam usable

- Domain crates never import iced or colours. Status enums expose `fn semantic(&self) -> Semantic`.
- Harness, surface and sandbox kinds expose stable string keys (`icon_token`, `SandboxKind::as_str`), which the UI maps to icons and tones.
- `RenderOpts { DocPalette, DocTypography }` in loa-surfaces is filled from tokens by loa-ui-surface.
- **CI lint** (`scripts/lint-ui-literals.sh`, run in P31 and by every UI package) forbids the following in every `loa-ui-*` crate except `loa-ui-kit/src/theme` and in `loa-app`:
  - `Color::from_rgb`, `Color {`
  - `extended_palette()`
  - `.size(<digit>`, `.padding(<digit or [digit>`, `.spacing(<digit>`
  - `Length::Fixed(<digit>`

## Deletions

### Whole crates or subsystems

| What | Lines (approx) | Why | Confirm with user? |
| --- | ---: | --- | --- |
| `loa-adapter-omp2` | 2,700 | Targets an unreleased omp. Only reachable through `services/harness.rs:34,119-122`. | Yes (P03) |
| `loa-adapter-testkit` | 4,027 (~250 salvaged) | No dependent crate. The DOM invariant checker, history checker and WatchSink are salvaged into `loa-harness::testing`. | No |
| `loa-surface-proto` network (wire, fragment, transport, viewer, server, tests) | ~2,460 | No production server or client; `mcp.rs` moves into the hub. | Yes (P15) |
| `loa-surface-bench` | 3,228 | Only reachable through `network_surface`, which has no production caller. | Yes (P15) |
| Hub video pieces: `network_surface`, `encoders`, `video`, `set_video`; ui-surface `h264.rs`; app `video_status`, `Encoders` | ~400 | Same reason as above. | Yes (P15) |
| `loa-exec`, `loa-xml`, `loa-rewind`, `loa-surface`, `loa-adapter` | | Folded into other crates (`loa-util`, `loa-core`, `loa-contracts`). | No |

### Modules and functions

- **loa-config:** `migrate.rs`, `native.rs`, `write_backup`, `Loaded::{migrated_from, backup}`, `tests/migrate.rs`. A version mismatch is refused with `TooNew`/`MissingMigration`.
- **loa-bus:** `Bus::firehose`, `Firehose`, `Hub`, generic `Queue<K>`, the `Drop` impl, `writer.rs::check`, and `SessionWriter::request` (merged into the new `request`).
- **loa-journal:** `proto.rs` `wire` module and its byte-equality test; `replay_of_100k_appends_is_fast`.
- **loa-omp-rpc:** the 56 unused `Command` variants and their response structs; `parse_known`/`Event::kind` (replaced by a tagged enum).
- **Adapters:**
  - the three `time.rs` calendar modules
  - claude's `base64`
  - the three MIME tables
  - codex `Handshake` and `take_array`
  - every per-adapter transport (acp `rpc.rs`, codex request paths, claude control, omp2)
- **loa-features:** `subagent`, `finish_subagent`, `enter_plan`, public `select_model`, `RunningSession`, `smol::Summarizer`.
- **Tools:**
  - omp-bridge: `rpc.rs` and the duplicated parts of `omp.rs`
  - remote: `ssh_config.rs`, `RemoteMcp::serve_stdio`, `import_hosts`, `host_tools.rs`, the `mcp.rs` dispatch
  - sandbox: `proto.rs` Enc/Dec, the async `read_frame` copy, `ssh_config_hosts`, `SandboxKind::ALL`/`as_str`, the duplicate kvm/which probes
  - audit: the hand-written `as_str`/`parse`
- **Surfaces:**
  - XLSX write path (`apply`, `XlsxEdit`, `CellInput`, `write_*`, `edit_xlsx`, `write_atomically`)
  - browser `bench.rs` and `examples/screencast_latency.rs`
  - `des.rs` (replaced by the `des` crate)
  - the in-source fake X11 server, moved into tests
  - `Format::ALL`
- **p2p and updater:**
  - `loa-grants/keys.rs`, `to_biscuit_string`/`parse_biscuit`, `tests/interop.rs`
  - share `History` and `catch_up`
  - ALPN `offer`/`server_order`/`highest_shared`, `Router::versions`, `local_alpns`
  - four of the five `DEFAULT_PORT` definitions (keep the one in loa-config)
  - `loa-node/src/ui.rs`, moved to the app
  - updater `feeds::Watcher`, `Swapper::swap_when_idle`, `check_inner` duplication, `gates/omp.rs` `Rpc`, `gates/rpc.rs` spawn code; `sign_*` moved to test support
- **Models and projects:**
  - models `catalog.rs`, `providers.rs`, `harness/jsonedit.rs`, `validate_provider_node`, `cli_login.rs`/`LoginFlow`/`FlowUpdates`
  - ui-models `catalog.rs` and `bridge.rs` (into the domain), the duplicate `settings.rs` types
  - projects `deliver`, `Delivered`, `McpResource`, `resource_uri`, `CONTEXT_*`, `Requester::Peer`, `future_since`
- **UI:**
  - all 13 `style.rs` files
  - composer `fuzzy.rs` and `popover()`
  - decisions `diff.rs` and `tests/live.rs`
  - terminal `pub mod loopback` (becomes `cfg(test)`)
  - shell `style::chrome`, `tab_underline`, the topbar Message/Event mirror, `From<sidebar::Event>`
  - ui-updates `model.rs` and `convert.rs`
  - chat `format_duration` and panel `format_elapsed` duplicates
- **loa-app:**
  - `borrow!`/`each_area!`/`on_area!`/`with_areas!`, `AreaId::ALL`/`key`, the ten per-area `Message` variants
  - every fake Adapter/Session and task drainer
  - `panel_style` modules, `ForgetRecent`
  - every `#[cfg(test)]` accessor
  - the surface-bench dependency
- **Every `#[ignore]` live test file and block** in the workspace (about 4.5k lines). Optionally keep one opt-in `loa-harness/tests/live_smoke.rs` behind `--features live` (at most 80 lines).

## Test policy

1. **Budget.** About 7.6k test lines in total (per-crate column above). Fixture data files (`fixtures/**/*.jsonl|.toml|.yml`) do not count toward the Rust budget but should stay small.
2. **No live, network, real-binary or wall-clock tests.** No `#[ignore]` tests and no stress loops of more than about 20 iterations. Exception: the optional `live` feature smoke test.
3. **Table-driven by default.** One test function per behaviour family, iterating `&[(input, expected)]`. Examples: Dom apply cases, journal damage cases, config invalid keys, authorize matrix, keymap and slash routing, keysym/CDP input, diff parsing, selector/effort parsing, YAML edit ops, login reducer, decision shortcuts.
4. **One testkit per layer, behind a feature. Never copy fixtures between crates.**

   | Testkit | Holds |
   | --- | --- |
   | `loa-util/testkit` | `eventually`, `fake_program`, tempdir helpers |
   | `loa-core/testkit` | `DomFixture`, `attrs!`, `AllowAll`/`DenyAll`/`MemoryPersist` |
   | `loa-harness/testing` | `WatchSink::until`, `FakeProcess`, `check_dom`, `replay()`, `round_trip()`, a minimal `ScriptedAdapter` with hooks (host tools, models, effort, ask→decision) used by the app tests |
   | `loa-surfaces/testkit` | `FakeSurface`, `next_frame`, `solid`, `chrome_or_skip!` (skips loudly unless `LOA_CHROME` is set) |
   | `loa-ui-kit/testkit` | key builders, Simulator `click_label`/`deliver`, `run_frames` |
   | `loa-app` `testing` module | `TestApp` driver, `TwoNodes` |

5. **Harness tests:**
   - Per harness, one transcript-replay test over fixture files, checked against a golden compact DOM rendering plus the expected outbound frames.
   - One native round-trip test per codec. **These fixtures are created before the codecs are refactored.**
   - One fake-process smoke test per `Framing`.
6. **Invariant tests that must survive:**
   - journal torn-tail recovery and fork lineage
   - bus persist ordering
   - config unknown-key preservation
   - YAML comment preservation and no-op saves
   - refusal of pasted secrets
   - ACP fs sandbox escape
   - remote: policy refusal before connect, host-key change refusal, ProxyJump, symlink escape
   - grant reshare narrowing
   - keystore fallback
   - chat incremental-equals-fresh and the windowing bound
   - diff 50k-line virtualization bound
   - tracker single-fire timeout
   - project pagination exactness
   - updater check→stage→gate→swap→rollback end-to-end on a fake feed
7. **UI tests assert on `Event` values and state, not on copy strings or pixels.** Keep one smoke render per screen at narrow width. The pixel-sampling theme test is replaced by a WCAG contrast test over `TokenSet` (both variants, all text-on-surface pairs).

## Work packages

All deltas are approximate, measured with `wc -l` over `*.rs`. They sum to roughly −116k, which brings the workspace to about 46–48k.

### Shared rules for every package

- Keep the workspace compiling and tests passing at the end of the package.
- Trim doc comments in the files you touch to one line per public item.
- When a crate is renamed or merged, update every `Cargo.toml` and `use` path in the same package (`sed` is fine).
- Run `scripts/budget.sh` and include its output in the package report.
- `parallel_safe: true` means the package touches files disjoint from every other package that is ready at the same time. The only overlap is single-line dependency edits in other crates' `Cargo.toml` files, which rebase trivially.
- `needs_user_confirmation: true` means stop and ask the user before deleting anything.

```json
[
{"id":"P01","title":"Create loa-util and budget tooling; delete loa-exec","touches":["Cargo.toml","crates/loa-util/**","crates/loa-exec/** (delete)","every Cargo.toml and file importing loa_exec (15 crates)","scripts/budget.sh","scripts/budgets.txt","scripts/lint-ui-literals.sh"],"depends_on":[],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"1) Add scripts/budgets.txt (crate name -> budget from the Final crate map; old crate names map to their target crate) and scripts/budget.sh: counts *.rs lines per crate dir with find|wc -l, prints the per-target sum against its budget, and exits non-zero only when the env var BUDGET_ENFORCE=1 is set. Add scripts/lint-ui-literals.sh with the theme-lint rules, not enforced yet. Record baseline output. 2) Create crates/loa-util with modules sync (LockExt, RwLockExt), fs (write_atomic, write_json_atomic, read_optional, sync_dir, Snapshot moved from loa-models/fsutil.rs, confine, home, data_dir), exec (move write_executable and retry_busy from loa-exec; add which, is_executable, parse_version (from loa-adapter availability.rs), probe_cli, run_checked, StderrTail, shell_quote, kvm_available), time (now_ms, now_us, ago, until, parse_iso_ms, format_iso_ms using the `time` crate), text (clip, first_line, floor_boundary, ceil_boundary, percent_encode, random_hex via getrandom, mime_of), registry::Registry<A: ?Sized>, the str_enum! macro (copy the generator from loa-core/schema.rs and generalize it; loa-core re-uses it later), Semantic enum, and a testkit feature (eventually, fake_program). Leave ssh as an empty module; P13 fills it. One table test per module, 120 test lines max. 3) Replace every loa_exec import with loa_util::exec and delete crates/loa-exec, including its stress test. Do not yet replace other duplicate helpers.","expected_delta":"+500"},
{"id":"P02","title":"Workspace helper sweep onto loa-util","touches":["all crates/*/src/** containing PoisonError::into_inner, fn lock, now_ms/now_us/now_unix copies, write_atomic/sync_dir copies, find_executable/which/on_path copies, ago/age copies, shell quote copies, random token copies, ISO calendar copies"],"depends_on":["P01"],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"Mechanical substitution with no behaviour change. 1) Replace every `.lock().unwrap_or_else(PoisonError::into_inner)` and `|e| e.into_inner()` with `.locked()`; do the same for RwLock with read_ok/write_ok. Delete every private `fn lock` helper (about 98 sites; grep `into_inner` until only loa-util remains). 2) Delete local now_ms/now_us/now_unix (loa-app start.rs and usage.rs, loa-ui-decisions desk.rs, loa-node, loa-updater, browser frames.rs, rfb surface.rs, hub mcp_surface.rs, proto lib.rs, sandbox tool.rs) in favour of loa_util::time. loa_core::now_ms becomes a re-export of loa_util::time::now_ms. 3) Replace write_atomic/sync_dir copies (loa-config store.rs, loa-journal snapshot.rs, loa-updater fsio.rs (delete the file), loa-models fsutil.rs (delete the file), loa-projects settings.rs and context.rs, loa-identity store.rs, loa-node folder.rs, loa-core sink.rs). 4) Replace find_executable/which/is_executable copies (loa-adapter availability.rs keeps a re-export, loa-node lib.rs, loa-updater installed.rs, sandbox service.rs and local.rs, browser chrome.rs, loa-app sessions on_path, ui-models tests). 5) Replace ago/age (loa-app start.rs, usage.rs, transfer.rs, loa-ui-projects screen.rs, loa-ui-updates lib.rs), shell_quote (remote service.rs, sandbox ssh.rs), random tokens (remote http.rs, omp-bridge omp.rs, hub endpoint.rs), and the three calendar modules (claude native/time.rs, omp native/time.rs, codex json.rs parse/format_rfc3339) with loa_util::time. Keep each crate's local error mapping. Run the full test suite.","expected_delta":"-1500"},
{"id":"P03","title":"Purge dead code, live tests and speculative crates","touches":["crates/loa-adapter-testkit/** (delete)","crates/loa-adapter-omp2/** (delete)","crates/loa-adapter/src/testing.rs (new, feature testing)","crates/loa-app/src/services/harness.rs","crates/loa-app/Cargo.toml","crates/loa-config/src/{migrate.rs,native.rs,store.rs,lib.rs}","crates/loa-config/tests/migrate.rs","crates/loa-bus/src/{bus.rs,queue.rs}","crates/loa-bus/tests/bus.rs","crates/loa-share/src/lib.rs","crates/loa-updater/src/{feeds/mod.rs,swap.rs}","crates/loa-updater/tests/live.rs","crates/loa-models/src/catalog.rs","crates/loa-projects/src/context.rs","crates/loa-remote/src/{mcp.rs,ssh_config.rs}","crates/loa-ui-terminal/src/{lib.rs,loopback.rs}","crates/loa-surface-docs/src/{xlsx.rs,server.rs}","crates/loa-surface-browser/{src/bench.rs,src/lib.rs,examples/**}","every tests/live*.rs and #[ignore] block workspace-wide"],"depends_on":["P02"],"parallel_safe":false,"needs_user_confirmation":true,"instructions":"Confirm only the omp2 removal with the user; everything else here has no production caller. 1) Salvage from loa-adapter-testkit: the check.rs invariants (turn shape, call input/result, waiting-decision consistency, dangling refs, check_history status machine) and WatchedSink go into crates/loa-adapter/src/testing.rs behind feature `testing`, max 300 lines. Then delete the crate and its workspace member. 2) Delete loa-adapter-omp2. Remove the KNOWN entry and the match arm at loa-app services/harness.rs:34 and :119-122; an unknown id returns None. 3) Delete loa-config migrate.rs, native.rs, with_migrator, write_backup, Loaded::{migrated_from,backup} and tests/migrate.rs. A version mismatch returns TooNew/MissingMigration. 4) Delete loa-bus Bus::firehose, Firehose, Hub, the bus.rs Drop impl and the firehose tests. 5) Delete loa-share History and Projection::catch_up. 6) Delete loa-updater feeds::Watcher (and tests/feeds.rs watcher tests), Swapper::swap_when_idle and its test, and tests/live.rs. 7) Delete loa-models catalog.rs, loa-projects context.rs deliver/Delivered/McpResource/resource_uri/CONTEXT_FILE_NAME/CONTEXT_MIME and their tests, loa-remote RemoteMcp::serve_stdio and ssh_config::import_hosts. 8) Make loa-ui-terminal loopback #[cfg(test)] and remove its pub use. 9) Delete the XLSX write path (XlsxDoc::apply, XlsxEdit, CellInput, write_data, write_input, DateFormats, Sheet::write_to, DocSurface::edit_xlsx, write_atomically); rust_xlsxwriter becomes a dev-dependency. 10) Delete loa-surface-browser bench.rs, examples/ and click_latency tests. 11) Delete every #[ignore] test and every tests/live*.rs file: claude, codex, opencode and omp live.rs, features live_omp.rs, loa-mcp live_omp_session, loa-remote app.rs live_omp, sandbox-stub live.rs, omp-bridge omp_live.rs, models/projects/ui-models live.rs, ui-decisions tests/live.rs (and its loa-adapter-omp dev-dependency), app decisions/sandbox/sessions live.rs and the ignored blocks plus Live/LiveOmp drivers in the app tests, net discovery multicast, node tailnet, identity os_keychain, hub tests/live.rs, journal replay_of_100k_appends_is_fast, bus pubsub 5000/10000/200x loops (keep one 20-iteration concurrency test). Remove helpers that become unused.","expected_delta":"-12800"},
{"id":"P04","title":"loa-core: check/apply split, schema! macro, absorb xml+rewind, DomWriter, testkit","touches":["crates/loa-core/**","crates/loa-xml/** (delete)","crates/loa-rewind/** (delete)","crates/loa-bus/src/writer.rs","crates/loa-features/src/{lib.rs,library.rs}","Cargo manifests that depend on loa-xml or loa-rewind"],"depends_on":["P03"],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"1) Split Dom::apply into pub check plus private mutate. Delete loa-bus writer.rs check() (lines 288-325) and call dom.check(). 2) Write a declarative schema! macro that emits ElementKind, the ElementSpec statics and the attr::* consts from one block. Generated names must stay identical. Use loa_util::str_enum! for the status enums. Collapse the value.rs accessors and From impls through a macro. Node::{str,int,bool}_attr delegate to the Value accessors. 3) Add impl From<NodeId> for Value, the IntoAttrs trait and the attrs! macro. 4) Add DomWriter (insert, set, close, ext). Base it on loa-adapter-testkit's scripted/writer.rs logic, which survives in git history. 5) Move loa-xml in as core::xml, writing to impl fmt::Write (drop the Out/IoOut sinks; io callers wrap with an adapter). Move loa-rewind in as core::rewind. Drop the xorshift property test and keep 3 table cases. Update loa-features imports and delete both crates. 6) Add feature `testkit` with DomFixture, AllowAll, DenyAll and MemoryPersist. 7) Tests become one table-driven apply_cases test, one short xml golden and the rewind table. Budget 1,300 lines.","expected_delta":"-1100"},
{"id":"P05","title":"loa-contracts: merge loa-adapter + loa-surface; add tools, account, factory, summarizer ports","touches":["crates/loa-contracts/** (new)","crates/loa-adapter/** (delete)","crates/loa-surface/** (delete)","root Cargo.toml workspace deps","every crate importing loa_adapter or loa_surface (sed)"],"depends_on":["P04"],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"1) Create loa-contracts with modules harness (all former loa-adapter items, plus the testing.rs salvaged in P03 behind feature `testing`), surface (all former loa-surface items), and availability (type only; probing lives in loa_util::exec). Bump INTERFACE_VERSION. 2) Add tools::{OutputBlock, ToolOutput, Tool, DynTool, ToolSet, CallCtx}, with `impl HostTools for ToolSet` and schemars specs. Add the harness::{HarnessAccount, HarnessFactory, HarnessTraits, LoginStrategy, LoginUpdate, LoginControl, SignInState, CommandSpec}, Summarizer and Capabilities::builder signatures from the Core abstractions section, with no implementations yet. Replace Registry with loa_util::registry::Registry. Delete Capabilities::missing and Feature::ALL if grep finds no external use; otherwise generate them with str_enum!. 3) In every crate, sed `loa_adapter::` to `loa_contracts::harness::` and `loa_surface::` to `loa_contracts::surface::`, and update the manifests. Check that Surface::negotiate/stream and StreamInfo are still used (the proto viewer still exists until P15); if they are, keep them. 4) Trim docs to one line per item. Budget 700 lines.","expected_delta":"-250"},
{"id":"P06","title":"loa-bus absorbs loa-journal; framed codec; prost only; queue simplification","touches":["crates/loa-bus/**","crates/loa-journal/** (delete)","crates/loa-share/src/lib.rs","crates/loa-projects/src/{sessions.rs,service.rs}","crates/loa-features/src/library.rs","crates/loa-app/** imports of loa_journal","crates/loa-bus/tests/fixtures/** (new: journal and snapshot files written by the current code)"],"depends_on":["P05"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"0) BEFORE changing any code, generate and commit fixture journal and snapshot files using the current loa-journal, plus a test that opens them. The on-disk format must stay byte-identical. 1) Move loa-journal into loa-bus as modules journal, snapshot, frame and proto. Merge store.rs into writer.rs. Delete the crate and repoint dependents (loa-share, loa-projects, loa-features, loa-node, loa-app). 2) frame.rs gets one framing codec (magic, version, len, crc) used by the journal header, the snapshot header and records. Keep byte layouts identical (use explicit LE fields; header prost messages only if the bytes match the fixtures, otherwise keep the fixed layout but shared). 3) Delete the proto.rs wire module and encode through prost. 4) Add SyncStrategy, implemented by SyncPolicy. 5) Merge SessionWriter::request and request_if_seq into request(peer, Option<u64>, draft, ts) and update loa-share lib.rs:88. 6) Make Queue non-generic, with Subscription { q: Arc<Queue> }. 7) Tests: a damage table for torn tails, a snapshot fallback table, fork lineage, lock, persist ordering, failed persist, replicate order, auth scope, close drain, one coalescing test and one 20-iteration concurrency test, all through a shared fixture() and loa-core testkit. Budget 1,300 lines.","expected_delta":"-2700"},
{"id":"P07","title":"loa-config: serde defaults, rule table, Appearance/ThemeRef, edit()","touches":["crates/loa-config/**","crates/loa-app/src/app.rs (theme mapping lines ~1421-1438 and the THEMES table)","crates/loa-app/src/areas/**/mod.rs (config load-mutate-save sites)","crates/loa-p2p-to-be: loa-net/src/{network.rs,endpoint.rs,tailscale.rs}, loa-node/src/lib.rs (DEFAULT_PORT only)"],"depends_on":["P05"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"1) Replace the hand-written Default impls with #[serde(default = ..)] or derive. Turn validate() into a static RULES table (keep the AnswerRule enum). Simplify the McpServer and Relay repr codecs with serde attributes. Keep the unknown-key preserving codec and its test. 2) Replace enum Theme with Appearance { theme: ThemeRef, overrides: toml::Table }, accepting the legacy values system/dark/light. In loa-app, map ThemeRef to the current shell Preference with a temporary function; P23 replaces it with the registry. 3) Add ConfigStore::edit(|c| ..) and watch(). Replace the 18 load/mutate/save sequences in loa-app areas with ctx-level edit calls. 4) Make loa-config DEFAULT_PORT the single definition and re-export it from loa-net and loa-node, deleting the four copies. 5) Tests: the FULL round trip, the 22-row invalid-key table, unknown-key preservation, and one table of (snippet, expected serialized presence). Budget 550 lines.","expected_delta":"-700"},
{"id":"P08","title":"loa-harness skeleton: kit + omp rpc (absorb loa-omp-rpc), OneShot summarizer","touches":["crates/loa-harness/** (new)","crates/loa-omp-rpc/** (delete)","crates/loa-adapter-omp/** (imports only)","crates/loa-models/{Cargo.toml,src/omp.rs,src/login.rs}","crates/loa-omp-bridge/Cargo.toml (if it uses loa-omp-rpc; it does not today)","crates/loa-projects/src/digester.rs","crates/loa-features/src/smol.rs","crates/loa-updater/src/gates/changelog.rs"],"depends_on":["P05","P06"],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"1) Create loa-harness with cargo features omp, claude, codex, opencode, acp, testing, all default except testing. Declare all five harness modules now (empty ones behind their features), plus a features module and a registry module, so later packages only fill their own directories. 2) kit/: rpc.rs (Framing, Incoming, Inbound, StdioRpc with pending map, writer task and read loop, about 250 lines), proc.rs (piped spawn via loa_util::exec::retry_busy, StderrTail, kill and grace shutdown, NotFound to NotInstalled), ledger.rs (TurnLedger with end_turn hooks so per-harness policies survive: claude drain-after-interrupt, codex self-started turns, omp queued follow-ups, acp replay), residual.rs (take, restore, split_parts, join_parts), mapper.rs (EventMapper, HarnessOps, DrivenSession), native.rs (NativeCodec). 3) Move loa-omp-rpc into omp/rpc. Delete the 56 unused Command variants and dead response structs. Replace parse_known and Event::kind with #[serde(tag=\"type\", rename_all=\"snake_case\")] plus an Unknown(Json) fallback. Collapse host_tool_call and host_uri_request into one spawn_host. Implement OmpFrames on kit::rpc with chunk reassembly and port RpcClient onto StdioRpc<OmpFrames>, keeping its public method names. Repoint loa-adapter-omp and loa-models to loa_harness::omp::rpc. 4) Add omp::OneShot implementing loa_contracts::Summarizer. Replace loa-projects OmpSummarizer, loa-features smol's summarizer and loa-updater OmpSmolSummarizer with it, keeping each crate's call site. 5) testing.rs: move loa_contracts::harness::testing into it (WatchSink with until(), check_dom, FakeProcess as one shared bash runner from omp-rpc fake.rs, replay() and round_trip() runners). Tests: one fake-process test per Framing implemented so far, plus chunk reassembly.","expected_delta":"-1100"},
{"id":"P09","title":"Move omp + acp adapters into loa-harness on kit","touches":["crates/loa-harness/src/{omp/**,acp/**}","crates/loa-harness/fixtures/{omp,acp}/**","crates/loa-adapter-omp/** (delete)","crates/loa-adapter-acp/** (delete)","dependent Cargo.toml lines (loa-app, loa-features, loa-mcp, loa-projects, loa-remote, loa-ui-*, loa-surface-hub, loa-sandbox-stub dev-deps)"],"depends_on":["P08"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"0) First convert the existing omp native round-trip tests and the mapping/rpc_flow scenarios into fixture files under fixtures/omp, and the acp tests/acp.rs scenarios into fixtures/acp. Make them pass through the replay and round_trip runners against the OLD code before refactoring. 1) git mv each crate into its module. Port acp rpc.rs onto StdioRpc<JsonRpc2> and the acp projector onto TurnLedger and DomWriter. Keep the fs sandbox (loa_util::fs::confine) and terminals, using one shared bounded byte buffer from kit. 2) Port omp mapping.rs onto TurnLedger and DomWriter, with dispatch through the tagged event enum rather than a match of 100+ lines. Native import/export go through residual helpers and loa_util::time. Write capabilities with Capabilities::builder. 3) Delete the old crates and repoint dependents (most only use omp as a dev-dependency through loa-harness features). Tests: the replay fixtures, one round-trip, one fake-process smoke test per harness and the acp sandbox escape table. Budget: omp module about 1,700 (rpc included), acp about 700.","expected_delta":"-5000"},
{"id":"P10","title":"Move claude + codex adapters into loa-harness on kit","touches":["crates/loa-harness/src/{claude/**,codex/**}","crates/loa-harness/fixtures/{claude,codex}/**","crates/loa-adapter-claude/** (delete)","crates/loa-adapter-codex/** (delete)","dependent Cargo.toml lines"],"depends_on":["P08"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"0) Convert the existing claude tests (events.rs, driver.rs, native.rs) and codex tests (adapter.rs, mapper_controls.rs, rollout round trip) into fixtures, passing against the old code first. 1) claude: driver control requests on StdioRpc<ClaudeControl> (add that Framing). Split events.rs into a protocol decoder (serde tagged enum) and a mapper on TurnLedger. Native import/export use residual split_parts/join_parts. Use the base64 crate and loa_util mime_of. 2) codex: the session moves onto StdioRpc<JsonRpc2> with one request path (delete Handshake). The mapper becomes a method-to-handler table. The rollout codec uses residual helpers (delete take_array). 3) Delete the old crates and repoint dependents. Budget: claude about 1,200, codex about 1,100, tests included.","expected_delta":"-5500"},
{"id":"P11","title":"opencode into loa-harness; loa-features as module; HarnessFactory registry; app wiring","touches":["crates/loa-harness/src/{opencode/**,features/**,registry.rs}","crates/loa-adapter-opencode/** (delete)","crates/loa-features/** (delete)","crates/loa-app/src/services/harness.rs","crates/loa-app/src/areas/{harness,models,sessions}/mod.rs","crates/loa-app/src/app.rs (sync_access omp check)"],"depends_on":["P09","P10"],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"1) opencode: fixtures first. Then a generic endpoint(method, path) client helper replacing the roughly 20 wrappers, serde tagged SSE events, the mapper on TurnLedger with tool_part and text_part split into small per-part handlers, and the native codec. 2) Move loa-features in as the features module and delete its dead API (subagent, finish_subagent, enter_plan, pub select_model, RunningSession, the local Summarizer trait, `let config = config;`). Native discovery moves into each NativeCodec::discover. Tests become one table-driven file. 3) registry.rs: implement a HarnessFactory per harness (id, label, icon_token, default_program, build, traits). account() returns None for now; P21 fills it. Expose loa_harness::factories(). 4) loa-app: replace the KNOWN list and the set_executable match with registry iteration. Replace the `== \"omp\"` and adapter-constant imports in harness/models/sessions/app.rs with HarnessTraits queries. 5) Optionally add tests/live_smoke.rs behind feature `live` (max 80 lines). Check the loa-harness total against its 8,000 budget and cut anything over.","expected_delta":"-3500"},
{"id":"P12","title":"loa-mcp: rmcp toolkit adapters, absorb loa-omp-bridge, import adapters, registry commit","touches":["crates/loa-mcp/**","crates/loa-omp-bridge/** (delete)","crates/loa-app/src/main.rs","crates/loa-app/src/areas/tools/mod.rs (bridge imports)"],"depends_on":["P08"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"1) Add toolkit/: ToolSetMcp (rmcp ServerHandler over loa_contracts::tools::ToolSet) and From conversions between CallToolResult and ToolOutput. McpHostTools uses ToolOutput (delete block_text, content and text). 2) Move loa-omp-bridge in as bridge/: OmpExecutor on loa_harness::omp::rpc::RpcClient (delete rpc.rs and the duplicated omp.rs handshake/command/response_data). If RpcClient lacks raw frame access for tool_execution_end, prompt_result or agentInvoked, add a minimal hook in loa-harness omp/rpc. Serve through ToolSetMcp. The CLI stays reachable through the `loa omp-bridge` app subcommand. Delete the standalone binary unless something outside the app launches it. 3) Add the ImportSource and ConfigValue adapters for json and toml (merge json_map and toml_map), Registry::commit for save and rollback, and one helper each for the manager service error and the UnknownServer lookup. 4) Tests: one fake-server integration file (stdio+http, filter, restart, registry rollback, import fixture table, host tools) of about 300 lines. Budget 1,500 lines.","expected_delta":"-2700"},
{"id":"P13","title":"loa-remote: ssh -G resolver, ToolSet tools, generic PromptQueue, AuditSink, audited()","touches":["crates/loa-remote/**","crates/loa-util/src/ssh.rs","crates/loa-app/src/areas/tools/{remote.rs,listing.rs,mod.rs}"],"depends_on":["P12"],"parallel_safe":true,"needs_user_confirmation":true,"instructions":"Confirm the `ssh -G` switch with the user. 1) Implement loa_util::ssh::{aliases(config) (Host-line scan with Include), resolve(alias) (runs `ssh -G`, parses hostname/user/port/proxyjump/identityagent, one call per ProxyJump hop)} with a clear error when ssh is missing. Add an SshResolver port in loa-remote with OpenSshCli and Static adapters. Delete ssh_config.rs and keep the config.rs API that the app uses. 2) RemoteBash and RemoteFile become loa_contracts Tool impls with their arg DTOs moved out of service.rs. Delete mcp.rs dispatch, host_tools.rs and the schema/parse/tool_ok helpers. RemoteHttp serves ToolSetMcp and keeps its url/session_spec/forget API. 3) PromptQueue::answer<R> replaces answer_host_key/answer_approval. Prompt exposes PromptView; the app formats the text (move the title/body strings into loa-app areas/tools). 4) Add the AuditSink trait with SqliteAudit and MemoryAudit; Action and Outcome get their string forms from str_enum!/serde. RemoteService::audited() replaces the duplicated outcome/record code in bash() and file(). Extract a pure apply_edit(text, old, new). 5) Tests: slim tests/support to about 300 lines (event log instead of the Stats mutexes; russh_sftp types for posix-rename) and about 10 scenarios through one setup(limits) builder. Policy, approval and read-only refusal cases go in a table; keep host-key change, ProxyJump, symlink escape and agent-only auth. Budget 1,800 lines.","expected_delta":"-5000"},
{"id":"P14","title":"loa-sandbox + stub: postcard proto, provider registry, teardown, checkout backends, typed tools","touches":["crates/loa-sandbox/**","crates/loa-sandbox-stub/**","crates/loa-app/src/areas/sandbox/{mod.rs,peer.rs}"],"depends_on":["P12","P13"],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"1) proto.rs: serde enums plus postcard frames plus the MAGIC scan. Bump VERSION and MAGIC, delete Enc/Dec and the async read_frame copy, and update the stub in this same package. 2) client.rs: a generic request<T>(req, pick) collapses read_file/write_file/list_dir/stat; deduplicate the ExecHandle match arms. 3) provider/: ProviderFactory plus ProviderRegistry, each provider file owning its probe() and build(). SandboxKind uses str_enum!. A Teardown trait is shared by the Drop and stop paths (docker, ssh). SshProvider hosts come from loa_util::ssh::aliases (delete ssh_config_hosts). Firecracker and QEMU go behind cargo feature `vm` (off by default; the app UI fields are cfg-gated to match). 4) isolation.rs: CheckoutBackend with GitWorktree, GitClone and Ovnfs (Ovnfs behind feature `ovnfs`). 5) worker.rs and tool.rs: the sandbox tools become loa_contracts Tool impls with schemars, keeping the public names NAME, SandboxTask, SandboxTaskArgs and SandboxEvent. 6) stub: split exec into spawn/pump/watchdog and move its tests to loa-sandbox/tests using loa-core testkit and the loa-harness testing doubles. Budgets: sandbox 1,300, stub 250.","expected_delta":"-2800"},
{"id":"P15","title":"Delete network video, omp/surface/1 networking and loa-surface-bench","touches":["crates/loa-surface-proto/** (delete; mcp.rs moved)","crates/loa-surface-bench/** (delete)","crates/loa-surface-hub/src/{hub.rs,tools.rs}","crates/loa-surface-hub/tests/hub.rs","crates/loa-ui-surface/src/{h264.rs,state.rs,panel.rs,lib.rs}","crates/loa-ui-surface/tests/video.rs","crates/loa-app/src/areas/surfaces/{mod.rs,tests.rs}","crates/loa-net/src/alpn.rs (surface ALPN constant)","crates/loa-contracts/src/surface.rs (negotiate/stream/StreamInfo if now unused)"],"depends_on":["P05"],"parallel_safe":true,"needs_user_confirmation":true,"instructions":"Ask the user first. This removes the visible but non-functional 'Live video' toggle and the omp/surface/1 protocol. If they decline, skip this package and plan the `remote-video` feature (+2.1k lines). 1) loa-app: remove video_status, set_video, Message::Encoders and the bench usage at surfaces/mod.rs:295; rewrite surfaces/tests.rs cases that used network_surface or bench Canvas. 2) ui-surface: delete h264.rs, NewDecoder, the VideoStatus launcher section and tests/video.rs. 3) hub: delete network_surface, encoders, video, set_video, HubConfig.ffmpeg and the hub tests that use serve/open. Move proto mcp.rs resource and parse_resource into hub tools.rs. 4) Delete loa-surface-proto and loa-surface-bench. Remove the surface ALPN constant. Remove Surface::negotiate, stream and StreamInfo from loa-contracts if grep finds no remaining user.","expected_delta":"-6450"},
{"id":"P16","title":"Create loa-surfaces (merge browser, docs, rfb, hub); pixels, FrameFeed, des crate","touches":["crates/loa-surfaces/** (new)","crates/loa-surface-browser/** (delete)","crates/loa-surface-docs/** (delete)","crates/loa-surface-rfb/** (delete)","crates/loa-surface-hub/** (delete)","crates/loa-ui-surface/Cargo.toml and imports","crates/loa-app (imports)"],"depends_on":["P15","P12"],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"1) git mv the sources in as modules browser/, docs/, vnc/ (from rfb) and hub/, behind features browser, docs and vnc (all default). Delete the four crates and repoint loa-app and loa-ui-surface; ui-surface uses default-features=false with features=[docs]. 2) pixels.rs: RgbaImage (blit, crop, fill, copy_rect, diff), encode_png, decode_jpeg (keep the Chrome quirk), decode_png, png_size. Replace rfb Framebuffer, docs Canvas blit/fill, the x11 band copy, ui-surface framebuffer.rs and decode.rs (ui-surface re-exports these), hub png_size and ui document image_size, and the docs Rgba::to_png and images png paths. 3) feed.rs: FrameFeed with InputLog. Port browser frames.rs, rfb Shared::send and mcp_surface publish onto it. 4) Replace des.rs with the RustCrypto `des` crate. 5) Add the testkit feature (FakeSurface, next_frame, solid, html_data_url, chrome_or_skip!) and delete the duplicate test helpers. The x11 fake server moves into tests. Real-Chrome tests skip loudly unless LOA_CHROME is set.","expected_delta":"-3600"},
{"id":"P17","title":"loa-surfaces internals: SurfaceProvider registry, tool registry, docs formats, VNC decoders","touches":["crates/loa-surfaces/src/**","crates/loa-surfaces/tests/**","crates/loa-app/src/areas/surfaces/mod.rs"],"depends_on":["P16"],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"1) hub: the SurfaceProvider port with Browser, Docs, Vnc and McpDeclared adapters, and the OpenRequest enum dispatched through a HashMap. SurfaceHub shrinks to registry, open list and events; capture-mode logic moves into BrowserProvider (deduplicate set_capture and browser_server). 2) Surface agent tools (screenshot, click, type, scroll, list, open_browser, navigate, page_text, elements, document_data) become loa_contracts Tool impls on one ToolSet served to HostTools and ToolSetMcp. Delete ToolCall::parse, ToolRunner, SurfaceTools::try_call, ToolResult::to_mcp/from_mcp and endpoint content(). 3) docs: a FormatRegistry of static FORMATS, the Document trait with per-format query(), and ooxml.rs (Package, rels, resolve_target, attr helpers, one quick_xml loop helper). Collapse docx sdt recursion to one function and share the reserve/new-page loop. pptx ShapeParser handlers become a fn-pointer table. RenderOpts { DocPalette, DocTypography } replace the module colour and size constants, with defaults identical to today. 4) vnc: an EncodingDecoder registry (raw, copyrect, rre, hextile, zrle, cursor, desktopsize). ENCODINGS is derived from the registry, and a message writer replaces the hand-built byte vectors. 5) Browser: the Capture trait with Screencast and X11Display strategies. 6) Tests per the test policy: keysym/CDP/format detection tables, codec tests against a tiny reference encoder in testkit, one round trip per doc format, one hub integration test (fake RFB plus a document), one MCP endpoint test. Budget 4,800 lines.","expected_delta":"-4200"},
{"id":"P18","title":"loa-trust: merge identity + grants, single PublicKey, ResourceKind table","touches":["crates/loa-trust/** (new)","crates/loa-identity/** (delete)","crates/loa-grants/** (delete)","crates/loa-node/**","crates/loa-net/** (key conversions)","crates/loa-app/** (imports, tools/tests.rs NodeKey)"],"depends_on":["P05"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"1) Merge both crates into loa-trust as modules key, keystore, scope, token, store, renew and select. A single PublicKey([u8;32]) with derives and From impls for iroh, biscuit and loa_bus::NodeKey. Delete grants keys.rs, NodeKey/NodePublicKey, to_biscuit_string, parse_biscuit and tests/interop.rs (keep one cross-check test), and loa-node's grant_key/bus_key/node_id helpers. 2) KeyStore with Memory, OsKeychain and File adapters, plus a FallbackStore decorator for the migration policy. 3) A ResourceKind table (KINDS) drives scope kind/supports/validate, RULES, right_fact, narrowing_check and token_scopes through generic loops. The Datalog fact names and the token format stay unchanged, so existing grants still verify; add a test with a token minted by the old code, committed as a fixture. 4) store.rs: shared row loaders and a shared holder/spec/parent/revoked select for renew and extend. 5) Tests: one authorize matrix table (8 kinds, prefix traps), one reshare chain, one renewal driver, one keystore fallback. Budget 950 lines.","expected_delta":"-2000"},
{"id":"P19","title":"loa-p2p: merge net + share + node; wire module, Transport, Discoverer, GrantGate; UI adapter to app","touches":["crates/loa-p2p/** (new)","crates/loa-net/** (delete)","crates/loa-share/** (delete)","crates/loa-node/** (delete)","crates/loa-app/src/areas/peers/{mod.rs,adapter.rs (new),folder.rs,remote.rs}","crates/loa-app/src/areas/sandbox/peer.rs","crates/loa-updater/src/peers.rs (ALPN label parsing)"],"depends_on":["P18","P06"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"Ask the user about Relay::Default mapping to Off (loa-app areas/peers/mod.rs:110-111) and change it only if they confirm it is a bug. 1) Delete ALPN multi-version negotiation (offer, server_order, highest_shared, Router::versions/protocols, local_alpns, the unshared_protocol test). Labels stay omp/<name>/1 with an exact-match check. 2) wire.rs: one length-prefixed prost codec, the Envelope trait and a generic call<Req,Rep>. Port control, the session protocol (from loa-share) and folder onto it, deleting the per-protocol Ack/refused/expect helpers. 3) transport/: the Transport trait with Quic (iroh) and Tcp (TLS+yamux, one shared verifier, one closed() error helper, dial_any) adapters, and FallbackDialer. 4) discovery/: the Discoverer trait with Mdns and Tailnet adapters and a single NearbyPeer constructor. Fix the tailscale probe test that rebinds `candidate` so its assertions actually run. 5) GrantGate implemented once and used by access, extra and folder. Shared auth handshake. Node becomes a facade over PeerDirectory, GrantService and SessionShare. Folder fs ops use loa_util::fs::confine. 6) Move loa-node/src/ui.rs to loa-app/src/areas/peers/adapter.rs (about 200 lines; resource ids via the ResourceKind encode/decode). loa-p2p must not depend on any loa-ui crate. 7) Tests: one loopback test per transport through a shared exercise(dial, accept), the projection replay test, and a two-node pair() fixture with about 5 scenarios. Budget 2,200 lines.","expected_delta":"-4150"},
{"id":"P20","title":"loa-updater: FeedSource registry, Unpacker strategies, data catalog, omp gate adapters move to app","touches":["crates/loa-updater/**","crates/loa-app/src/areas/updater/{mod.rs,omp_gates.rs (new)}","crates/loa-app/Cargo.toml"],"depends_on":["P08"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"1) FeedSource trait with GitHub and Npm adapters; Updater::check is a single code path (delete check_inner duplication). 2) Unpacker strategies (TarGz{strip}, Zip, Raw) over a shared EntrySink using loa_util::fs::confine. One shared error enum and NotFound-to-default read helper replaces the three copies in state.rs, swap.rs and stage.rs; one io_at helper. 3) The product catalog and OmpGates lists become a const table (or include_str! TOML). 4) loa-updater keeps only the gate ports and pure checks. Move OmpProbe, the ready probe and the changelog summarizer into loa-app areas/updater/omp_gates.rs, using loa_harness::omp::rpc::RpcClient (ready, protocol_version, omp_version) and omp::OneShot. Delete gates/omp.rs Rpc and the spawn code in gates/rpc.rs; check that SpawnOptions supports cwd, args, kill_on_drop and retry_busy. sign_artifact and sign_digest move to tests/support.rs. 5) Status types live in loa_updater::model and are rendered directly by the UI later; if that drags reqwest into the UI, gate the heavy modules behind a default `service` feature. 6) Tests: tests/support.rs (FakeFeed axum server, stage_fixture, key(seed)), table-driven tool-schema diff, decision and peer standing tests, one end-to-end scenario. Budget 1,450 lines.","expected_delta":"-4500"},
{"id":"P21","title":"loa-models: one settings model, Selector, ConfigDoc, HarnessAccount impls, unified login, service from ui-models","touches":["crates/loa-models/**","crates/loa-harness/src/{claude/account.rs,codex/account.rs,omp/account.rs,registry.rs}","crates/loa-ui-models/src/{backend.rs,bridge.rs,catalog.rs,settings.rs (types only),signin.rs (state types only),lib.rs}","crates/loa-app/src/areas/models/{mod.rs,tests.rs}","root Cargo.toml (serde_json preserve_order)"],"depends_on":["P11"],"parallel_safe":true,"needs_user_confirmation":true,"instructions":"Confirm with the user that ~/.claude/settings.json whitespace may be normalized once jsonedit is deleted (key order is kept). 1) selector.rs is the single Selector/ModelRef grammar. Replace ui catalog.rs ModelRef, bridge split_selector and chip short_model; delete ui-models catalog.rs. 2) Enable serde_json preserve_order workspace-wide. The ConfigDoc trait plus edit_file get implementations for claude JSON and codex TOML (toml_edit); YamlDocument implements it too. Delete jsonedit.rs. Apply str_enum! to the effort, auth, modality, OverrideKey, ProviderAuth and FieldKind enums. 3) The HarnessAccount implementations for claude, codex and omp live in loa-harness <harness>/account.rs, built from that harness's HarnessFactory::account(). Delete loa-models harness/mod.rs SignInState duplicates and providers.rs (fold the rank sort into the service). 4) Login: one LoginUpdate/LoginState, OmpOAuthLogin and CliLogin strategies (keep the login_frame secret-input filtering). Delete LoginFlow, FlowUpdates::map, CliLoginOutcome and cli_login.rs. 5) Move ui-models backend.rs into loa_models::service (no iced, returns mpsc receivers); loa-app areas/models wraps them in iced streams. Move bridge.rs into loa_models::settings as a diff-and-apply over a single ModelSettings, OVERRIDE_FIELDS and FieldValue. Delete validate_provider_node and the UI duplicate types, keeping the cheap checks (pasted secret, env-var name, id charset) in one place. 6) Slim yaml.rs (merged render paths, one path walker, table tests); config.rs getters/setters through ScopedKey/ListKey tables; omp.rs through a LazyClient delegation macro; process.rs reduced to about 60 lines on loa_util::exec. One FakeOmp behind a `testing` feature replaces the five fakes, including the one in loa-app. Budget 2,300 lines.","expected_delta":"-6500"},
{"id":"P22","title":"loa-projects: testing module, Scorer strategies, Tool impls, debouncer/paginator, drop Requester::Peer","touches":["crates/loa-projects/**","crates/loa-util/src/{async_util.rs or registry.rs additions}","crates/loa-app/src/areas/projects/{mod.rs,tests.rs}"],"depends_on":["P08","P06"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"1) One cfg(test)/`testing` module (Builder, MemorySource, Fake, JournalPersist, AllowAll via the loa-core testkit) replaces the source.rs/rebuild.rs/service.rs/sessions.rs duplicates. 2) Delete Requester::Peer, TestAccess::future_since and the future-grant test matrix; keep the Access trait with LocalOnly and one visibility table test. 3) rank.rs: the Scorer trait with Bm25 and Recency strategies combined by Weighted. 4) tools.rs: project_search and project_read become loa_contracts Tool impls registered in a ToolSet (delete the specs() literal and the string match). 5) Add KeyedDebouncer<K> and a byte Paginator to loa-util (add only if missing) and use them in rebuild.rs and read.rs. The text.rs helpers move to loa_util::text where they are generic (clip, boundaries); Folded and words stay local. 6) The Summarizer port is loa_contracts::Summarizer; the digester keeps only prompt building and answer parsing. 7) Tests: 2-3 table tests each for search, read (exact pagination coverage), rank and digest staleness, about 250 lines. Budget 1,500 lines.","expected_delta":"-2350"},
{"id":"P23","title":"loa-ui-kit: token layer, theme registry, style recipes, icons macro, widgets, text, testkit","touches":["crates/loa-ui-kit/** (new)","crates/loa-ui-icons/** (delete)","crates/loa-ui-shell/src/theme.rs (becomes thin State over kit)","crates/loa-app/src/app.rs (theme resolution)","all crates depending on loa-ui-icons (sed)"],"depends_on":["P07"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"Build exactly the Theme layer section of this plan. 1) theme/: role enums via a roles! macro, TokenSet (array-indexed, iced Palette derived from it, apply and apply_table overrides), Paint, StyleKey, StyleSpec, StyleStates, ThemeSource, ThemeLoader, ThemeRegistry::builtin (system, dark, light with today's DARK/LIGHT values defined ONCE: merge the shell DARK_PALETTE/LIGHT_PALETTE and Tokens tables), and the ACTIVE generation-stamped global with tokens()/set_active(). 2) style/: the button, container, text, tint, input, pick, scrollbar and rule recipes read StyleStates from the active tokens; state_bg helper; semantic(Semantic) -> Tone. 3) icons.rs: an icons! macro generating the enum and table, handles indexed by `self as usize`. Delete loa-ui-icons and sed its dependents. 4) widgets/: Popover (from composer), TabStrip, Collapsible, Card, Badge, IconButton with tooltip, menu_row and two_line_row, MonoBlock, Meter, SectionHeader, StatusRow, FormView, SideNav<T>, EmptyState, ListEditor<T>, Picker<T>, FormState<F>, VirtualList with Fenwick Heights (from chat heights.rs) and scroll_target (from the palette), Splitter edge. The Component and PanelPane traits and the Msg<L,E> convention. 5) text/: the palette fuzzy Matcher plus filter and highlight_runs, format_duration, format_bytes, truncate_leading/trailing. 6) testkit feature: key builders, Simulator click_label/deliver, run_frames. 7) loa-ui-shell theme::State resolves Appearance through ThemeRegistry, applies overrides, emits ThemeChanged and re-exports kit::theme so existing call sites compile. loa-app drops its temporary mapping from P07 and lists registry themes in the palette and settings. 8) Tests: a WCAG 4.5 contrast table over both variants, override parsing, round trip of role names. Budget 1,400 lines.","expected_delta":"+700"},
{"id":"P24","title":"Port core UI crates to kit; delete their style.rs and literals","touches":["crates/loa-ui-shell/**","crates/loa-ui-chat/**","crates/loa-ui-composer/**","crates/loa-ui-decisions/**","crates/loa-ui-panel/**","crates/loa-ui-terminal/**","crates/loa-ui-palette/**","crates/loa-ui-tools/**"],"depends_on":["P23"],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"For each listed crate, in order terminal, panel, chat, composer, decisions, palette, tools, shell: delete style.rs (and palette.rs's inline styles, plus shell style::chrome and tab_underline, using TabStrip pills); replace every colour, alpha, text size, padding, spacing and fixed width literal with kit roles and recipes; replace local helpers with kit widgets (composer popover.rs and fuzzy.rs, chat heights.rs, the shell tab-strip duplication, palette scroll_target, waiting_badge/waiting_text). The terminal TermPalette takes ANSI colours from term.N roles (delete rotate_hue) and rebuilds on ThemeChanged; the chat Look and the decisions markdown settings rebuild on the generation change. Build after each crate. Run scripts/lint-ui-literals.sh on these crates; it must pass. Do not restructure modules here; that is P25 and P26.","expected_delta":"-2000"},
{"id":"P25","title":"loa-ui-session: merge chat + composer + decisions with Picker, Choice, CardKind, RowRenderer, Notifier","touches":["crates/loa-ui-session/** (new)","crates/loa-ui-chat/** (delete)","crates/loa-ui-composer/** (delete)","crates/loa-ui-decisions/** (delete)","crates/loa-app/** (imports, decisions/composer/chat message wiring)"],"depends_on":["P24"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"1) git mv into modules transcript/, composer/ and decisions/, then delete the old crates and update loa-app in the same package. 2) transcript: a RowRenderer registry keyed by ItemKind; tool verbs become a static table instead of the tool_words if-else chain; one JobView model shared with the panel agents tab via kit::text::format_duration. 3) composer: the six pickers become kit Picker<T> with Message::Pick(PickerId, usize), deleting the mirrored Message/Event variants; reuse the kit fuzzy matcher. 4) decisions: a Choice trait plus choice_enum! for ApprovalChoice and PlanChoice; CardKind strategies (Approval, Question, Plan, Form) through kind_for(); a Notifier trait with DesktopNotifier and NullNotifier adapters; the unattended settings view goes in its own file; delete decisions diff.rs and use loa_ui_panel::diff::parse_unified with a compact view (that path stays stable in P26). 5) Tests: kit testkit plus the loa-core DomFixture; table-driven shortcut and answer mapping; keep incremental-equals-fresh, the windowing bound, reading position, the frame-by-frame regression and tracker single-fire. Budget 2,700 lines.","expected_delta":"-6300"},
{"id":"P26","title":"Fold tools into panel, palette into shell; terminal PtyBackend","touches":["crates/loa-ui-panel/**","crates/loa-ui-tools/** (delete)","crates/loa-ui-shell/**","crates/loa-ui-palette/** (delete)","crates/loa-ui-terminal/**","crates/loa-app/** (imports, panel routing, terminal)"],"depends_on":["P24"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"1) loa-ui-tools becomes loa_ui_panel::tools. Use kit FormState for add-server, host and limits forms (Message::FormEdit(FormId, FieldId, String) replaces about 20 field variants) and split view.rs per section. 2) Panel panes (files, diff, agents, session_tree, tools) implement kit PanelPane. loa-app routes through a PaneRegistry and drops the shell tab_icon string match. Keep the loa_ui_panel::diff::{parse_unified,...} paths stable. 3) loa-ui-palette becomes loa_ui_shell::palette, with keymap defaults as a table. Delete the shell topbar Message/Event mirror and the From<sidebar::Event> mirror (nest sidebar::Event). 4) Terminal: the PtyBackend trait with LocalPtyBackend and RemotePtyBackend (wrapping the existing RemotePty) adapters; State stops matching on Target; split canvas drawing into background, decoration and text passes; keep one /bin/sh integration test. 5) Tests: table-driven keymap, diff, form validation and palette colours; one smoke render per pane; keep the diff virtualization bound, files lazy listing and session_tree cycle tests. Budgets: panel 2,300, shell 1,400, terminal 1,000.","expected_delta":"-10500"},
{"id":"P27","title":"loa-ui-settings: merge ui-models, ui-projects, ui-peers, ui-updates on kit; Local|Emit; render domain types","touches":["crates/loa-ui-settings/** (new)","crates/loa-ui-models/** (delete)","crates/loa-ui-projects/** (delete)","crates/loa-ui-peers/** (delete)","crates/loa-ui-updates/** (delete)","crates/loa-app/src/areas/{models,projects,peers,updater}/**"],"depends_on":["P21","P19","P20","P23"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"1) Modules models/ (signin as a pure reducer over loa_models LoginUpdate, picker, chip, session, settings built from kit ListEditor/FormView over OVERRIDE_FIELDS), projects/screen, peers/ (state, view, grant_form) and updates/ (rendering loa_updater::model types directly, Event reusing HoldChoice, status as a STATUS_RULES first-match table). 2) Every screen uses kit Msg<Local, Emit> plus one guard function; delete the forwarding update() mirrors. Delete all four style.rs files plus ui-updates model.rs and convert.rs; no literals (the lint must pass). 3) Update loa-app areas in the same package. 4) Tests: reducer tables of (messages, expected Event, state predicate) plus one Simulator smoke test per screen, asserting on Events rather than copy. Budget 2,000 lines.","expected_delta":"-3700"},
{"id":"P28","title":"loa-ui-surface: SurfaceMode strategies, kit theming, slim tests","touches":["crates/loa-ui-surface/**","crates/loa-app/src/areas/surfaces/**"],"depends_on":["P17","P23"],"parallel_safe":true,"needs_user_confirmation":false,"instructions":"1) Split State into a tab shell plus SurfaceMode implementations PixelMode and DocumentMode, removing the roughly 10 Body matches; a FrameDecoder chain (StillDecoder only). 2) Split the panel launcher into one function per section. A SurfaceKindStyle lookup keyed by provider name replaces the 'vm'/'browser' strip() match. Delete style.rs and all literals, including the picker width, viewer cursor alphas and strokes, and sizes 13/12/14. Docs rendering gets RenderOpts built from kit tokens. 3) Pixel and geometry code uses loa_surfaces::pixels; percentile goes into latency.rs only. 4) Tests: keys and geometry tables; renderer.rs cut to about 250 lines and panel tests to about 80 using the loa-surfaces testkit; delete browser_panel.rs. Budget 1,500 lines.","expected_delta":"-3000"},
{"id":"P29","title":"loa-app infrastructure: AreaDyn registry, CommandSpec tables, watch/perform helpers, testing module","touches":["crates/loa-app/src/{app.rs,area.rs,areas/mod.rs,areas/*/mod.rs,areas/*/tests.rs,app/keys.rs,app/keys/tests.rs,app/tests.rs,session.rs,testing/** (new)}"],"depends_on":["P11","P14","P22","P25","P26","P27","P28"],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"1) Build the cfg(test) testing/ module first (TestApp driver with a current-thread runtime and send/area/palette/prompt/key/click/until/pump_feeds; FakeHarness on the loa-harness testing ScriptedAdapter with builder flags; TwoNodes). Port every area's tests to it with table-driven routing tests (keymap chord to command id, CommandSpec id runs without a notice, slash name to area) and one scenario of at most 60 lines per area. Delete the 8 fake adapters, 6 fake sessions and 8 drainers. 2) AreaDyn, Typed<A>, AreaMessage, Message::Area(&'static str, AreaMsg) and a Registry built from one AREAS table. Delete borrow!/each_area!/on_area!/with_areas!/the local area! macro, AreaId::ALL/key and the 10 Message variants; sed the test call sites to Message::area(..). 3) Per-area const COMMANDS: &[CommandSpec], with palette/run/slash/keymap derived from them; delete the three parallel lists in sessions and the duplicated ids in decisions, updater and keys. 4) Add the watch() subscription adapter with a PtrKey hash and replace the 7 stream adapters (peers events/remote feed, session feed, sandbox events, surfaces hub/mcp, tools mcp/queue, projects rebuilds). Add perform() plus Ctx::report to collapse Task::perform/map_err/notify(format!). 5) Delete the cfg(test) accessors, ForgetRecent and the panel_style modules. Test budget 700 lines.","expected_delta":"-8000"},
{"id":"P30","title":"loa-app lifecycle and workspace extraction, view rewrite on kit, sessions split","touches":["crates/loa-app/src/{app.rs,app/lifecycle.rs (new),app/workspace.rs (new),app/windows.rs,app/detach.rs,app/start.rs,app/usage.rs,areas/**/*.rs views,areas/sessions/**,services/**}"],"depends_on":["P29"],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"1) SessionController in lifecycle.rs (start, resume, send, spawn, interrupt, feed, dispatch; settle stays in App and passes &mut Ctx). workspace.rs takes folder/project/recents plus the GitProbe and FolderPicker ports with CLI adapters (zenity/kdialog). A UsageSource port for usage.rs. The shell::Event arm list becomes a route table to Intent. 2) sessions/ splits into mod, branch (one spawn_branch strategy for fork/rewind/compact), plan, hooks, transfer and tree. 3) Rewrite the views (usage, windows on kit SideNav, sandbox runs/settings via a FormView over a Form impl replacing Draft/Field, transfer, peers folder/remote, harness rows, start) on kit widgets. Merge start.rs into the app view and detach.rs into windows.rs. Layout constants (COMPOSER_WIDTH, insets, window sizes, nav/list widths) become LayoutRole tokens. 4) Services split into NodeServices, Storage and Harnesses. Enum mirror matches (listing.rs import_source/outcomes/Status, the Capture/CaptureMode pair) become From impls. 5) scripts/lint-ui-literals.sh must pass on loa-app. Budget: loa-app 5,000 lines total.","expected_delta":"-5375"},
{"id":"P31","title":"Final gate: docs, doc-comment pass, lint and budget enforcement","touches":["docs/ARCHITECTURE.md","scripts/budgets.txt","any crate over budget","CI config if present"],"depends_on":["P30"],"parallel_safe":false,"needs_user_confirmation":false,"instructions":"1) Rewrite the docs/ARCHITECTURE.md crate tables for the 22-crate map, the dependency layers, the port/adapter list (HarnessFactory, HarnessAccount, Tool/ToolSet, Summarizer, SurfaceProvider, ProviderFactory, Transport, Discoverer, FeedSource, KeyStore, ThemeSource/ThemeLoader) and the Theme layer contract, stating that scripting plugs in through ThemeLoader. 2) Run a workspace doc-comment pass: one line per public item, no module essays. 3) Run cargo fmt, clippy -D warnings and test --workspace; run scripts/lint-ui-literals.sh in enforce mode; run scripts/budget.sh with BUDGET_ENFORCE=1 (total <= 48,000 and each crate <= budget +5%). Fix overruns by deleting rather than reformatting. 4) Report the final per-crate line counts.","expected_delta":"-500"}
]
```

### Dependency graph and parallel lanes

- **Serial spine:** P01 → P02 → P03 → P04 → P05.
- **After P05, these lanes can run at the same time:**
  - P06 and P07 run in parallel with each other.
  - P15 can start right after P05.
  - P18 → P19, with P19 also waiting for P06.
- **After P06 (and P05):** P08.
  - After P08: P09, P10, P12, P20 and P22 run in parallel.
  - P11 waits for P09 and P10. P21 waits for P11.
  - P13 waits for P12; P14 waits for P12 and P13.
- **Surfaces lane:** P16 needs P15 and P12; then P17.
- **UI lane:**
  - P23 can start after P07.
  - P24 needs P23. After P24, P25 and P26 run in parallel.
  - P27 needs P21, P19, P20 and P23. P28 needs P17 and P23.
- **App and final:** P29 needs P11, P14, P22 and P25 through P28. Then P30, then P31.

## Risks

1. **Lossy native round trips.** Moving the claude, codex, opencode and omp codecs onto shared residual helpers can drop unknown fields. The mitigation is built into P09–P11: fixtures are made and must pass against the old code before any refactor.
2. **Turn semantics drift.** `TurnLedger` must keep per-harness end-of-turn policies (claude drain after interrupt, codex adopting self-started turns, omp queued follow-ups as separate turns, acp replay on `session/load`). It does this through `end_turn` hooks rather than one forced policy. Replay goldens catch regressions.
3. **On-disk and wire compatibility.**
   - **Journal:** guarded by the byte-identical fixtures in P06.
   - **Grants:** the Datalog schema is unchanged and guarded by an old-token fixture in P18.
   - **ALPN:** the `/1` labels are unchanged.
   - **Sandbox stub:** the protocol changes on purpose (postcard; VERSION and MAGIC bumped). Stubs already baked into user VM images become incompatible and need rebuilding; that is acceptable only because the VM providers ship no images today.
   - **ssh -G (P13):** needs an OpenSSH client and makes `Match` blocks effective. Both need a clear error message and user confirmation.
4. **Product-visible removals** need user confirmation: omp2 in the harness picker, the "Live video" toggle and `omp/surface/1` (P15), and the claude `settings.json` whitespace change (P21). Firecracker, QEMU and ovnfs are kept behind default-off features, not deleted.
5. **Runtime theming through a global.** iced 0.14 style functions only receive `&Theme`, so tokens live in a process-global `RwLock` with a generation number. Hot swaps must invalidate cached looks (chat `Look`, markdown settings, terminal palette, canvas caches) through `ThemeChanged`. Layout may shift slightly while literals move to tokens, so P24 and P30 should also be checked by running the app (`run` skill), not only by tests.
6. **Type-erased app messages.** `AreaMsg(Arc<dyn Any>)` loses compile-time routing checks. Mitigations: a `debug_assert!` with logging on downcast failure, and a typed `Message::area::<M>()` constructor in all tests. Tests that compared messages with `==` must change.
7. **Coverage loss.** Deleting live tests, the testkit contract suite and the stress tests removes real-CLI protocol-drift checks and ETXTBSY and race checks. Mitigations: one small deterministic concurrency test each in bus and util, the optional `live` smoke feature, and replay fixtures that call `check_dom`.
8. **Compile times.** Merging crates (`loa-harness`, `loa-surfaces`, `loa-p2p`, `loa-ui-session`) makes each crate larger. Mitigations: per-harness and per-surface cargo features, `loa-ui-surface` using loa-surfaces with only `docs`, and the terminal crate kept separate.
9. **Budget pressure points.** `loa-harness` (8.0k) and `loa-app` (5.0k) are the tightest. If either overruns, P31 deletes rather than compresses. The first candidates are the opencode HTTP client wrappers, `features::plan`/`hooks` prose, the peer sandbox provider UI, and the usage "Use a reset" flow; the last two are user-visible, so confirm them with the user first.
10. **Cross-package ownership.** loa-app is touched by most packages. Each package must leave the app compiling and update only the app files it lists. A rebase conflict in a `Cargo.toml` dependency line is expected and trivial; a conflict in app source means the package overstepped its touch list.
11. **Possible real bugs found during planning.** These are not fixed silently:
    - `Relay::Default` maps to `Off` at `loa-app/src/areas/peers/mod.rs:110-111`. P19 asks the user before changing it.
    - The tailscale test's accept-path assertions never run (`loa-net/src/tailscale.rs:153-170`). P19 fixes the test.

## User decisions (2026-11-20) — these override anything above

1. **Network video stack (P15): KEEP behind a default-off cargo feature `remote-video`.** Do not delete `omp/surface/1` networking, H.264 encode/decode, the bench encoder pieces the video path needs, or the "Live video" toggle. Move them into `loa-surfaces` (and `loa-ui-surface`) behind `remote-video`; the latency benchmark binary/live tests may still be deleted. Budget: `loa-surfaces` +2,100.
2. **omp2 (P03): KEEP.** Do not delete `loa-adapter-omp2`. Port it onto `loa-harness::kit` as a feature-gated `omp2` module with its own `HarnessFactory` (P11). Budget: `loa-harness` +800.
3. **SSH config (P13): use `ssh -G`.** Show a clear error when no OpenSSH client is found.
4. **jsonedit (P21): delete it.** Normalized serde_json output with key order preserved is acceptable.
5. **Relay::Default → Off (P19):** do NOT change behavior; leave a `// TODO(relay)` and report it.
6. Firecracker/QEMU/ovnfs stay behind the default-off `vm` feature as planned.

Because 1 and 2 add ~2.9k lines, the **hard ceiling is still 50,000 lines total** (`find crates -name '*.rs' | xargs cat | wc -l`). Every package should aim ~6% under its budget; P31 trims to the ceiling.
