# C++ rewrite recipe

Porting spec for the C++ rewrite on branch `cpp-rewrite`. Issues are labeled `cpp-port` and titled `CPP_PORT: …`; execution steps are in [RUNBOOK.md](RUNBOOK.md).

## Rewrite rules

- **Source of truth.** The Rust code under `crates/` on branch `cpp-rewrite` is the spec. Port its behavior and public surface, not its internals.
- **Libraries.** Each one lives in `cpp/libs/<name>/` (name = crate name without `loa-`, with `-` turned into `_`) and contains:
  - `CMakeLists.txt` declaring the `STATIC` library `loa_<name>` and the test executable `loa_<name>_tests`;
  - `include/loa/<name>/*.hpp`;
  - `src/*.cpp`;
  - `tests/*.cpp`.
- **Language.**
  - C++23, with `std::expected<T, loa::Error>` for fallible APIs. Exceptions never cross a library's public API.
  - Namespace `loa::<name>`.
  - Async code uses Asio C++20 coroutines on the `loa::runtime` executor.
  - Rust traits become abstract classes. Rust `Arc<dyn T>` becomes `std::shared_ptr<T>`.
- **Architecture** (unchanged from `docs/ARCHITECTURE.md`):
  - Contracts depend only on `loa::core`.
  - A library that needs another library's functionality declares a port (abstract class), and `loa-qt` (`cpp/app`) wires in the implementation.
  - UI modules never spawn a harness, open a socket or read the journal.
- **Formats stay byte-identical with the Rust app:** journal and patch protobuf, snapshots, SQLite schemas, files on disk, config TOML and YAML, key files, keychain entries, `omp/control/1`, `omp/session/1`, `omp/folder/1`, `omp/surface/1`, port 31415, ALPNs, mDNS records and grant tokens. Each format has a golden fixture in `cpp/tests/golden/` (from F5) that the C++ code must decode and re-encode identically.
- **Compile speed.**
  - Heavy headers (Asio, toml++, cpp-httplib, Qt) go only in `.cpp` files or behind `target_precompile_headers`. Public headers forward-declare.
  - One translation unit (TU) per header-only third-party library implementation.
- **Platforms.** Port every Rust `cfg(target_os …)` branch 1:1 behind `#if` blocks. Only Linux is built and verified.

## C++ dependency map

| Rust | C++ |
| --- | --- |
| tokio, futures, async-trait | standalone Asio (`libasio-dev`) with C++20 coroutines; `loa::runtime` owns the `io_context` threads |
| serde_json | yyjson 0.10.0 (CMake `FetchContent`, pinned tag) wrapped in `loa::json` |
| prost | hand-written wire codec `loa::proto` in `cpp/libs/runtime` (varint, zigzag, fixed32/64, length-delimited) plus a hand-written encode/decode per message, matching prost field numbers |
| rusqlite | sqlite3 |
| toml, serde_ignored, serde_path_to_error | toml++ (`TOML_HEADER_ONLY=0`, one TU), with unknown-key reporting by dotted path in `loa::config` |
| toml_edit (loa-models) | targeted line-based edits that keep untouched lines byte-identical |
| yaml-rust2 | yaml-cpp |
| reqwest, rustls, webpki-roots | libcurl (system CA store) |
| axum, hyper | cpp-httplib (one TU) |
| rmcp | in-house MCP in `cpp/libs/mcp`: JSON-RPC 2.0 over stdio and streamable HTTP; client in T1, server in T2 |
| russh, russh-sftp | libssh, `ssh-agent` auth only |
| iroh | `iroh-c-ffi` fork, prebuilt static library (F3) |
| biscuit-auth | `biscuit-capi` fork on 6.0, prebuilt static library (F4) |
| ed25519-dalek, sha1/sha2, hmac, getrandom, zeroize, rcgen, tokio-rustls | OpenSSL 3 (EVP, `RAND_bytes`, `OPENSSL_cleanse`, X509 self-signed ed25519 certs, TLS 1.3) |
| yamux | in-house yamux in `cpp/libs/net`, wire-compatible with Rust `yamux` 0.14 |
| mdns-sd | mjansson/mdns 1.4.3 (`FetchContent`), same service type and TXT records |
| keyring | libsecret on Linux; Keychain and Credential Manager behind `#if` |
| alacritty_terminal, portable-pty | libvterm plus `forkpty` (ConPTY behind `#if _WIN32`) |
| chromiumoxide | in-house CDP client over a minimal RFC 6455 WebSocket client on Asio (local `ws://`) |
| x11rb | libxcb (+shm, xtest) |
| hayro | QtPdf `QPdfDocument` |
| calamine, rust_xlsxwriter, docx-rs, zip, quick-xml | minizip + pugixml (xlsx/docx read), libxlsxwriter (xlsx write), libarchive (zip/tar in the updater) |
| image, png, jpeg-decoder, jpeg-encoder | libpng, libjpeg-turbo (decode and encode), libwebp, giflib; stb_image only for BMP |
| rusty_h264 | openh264 |
| ab_glyph | stb_truetype |
| flate2, crc32fast | zlib |
| similar | in-house Myers line and inline diff `loa::runtime::diff` |
| ignore | in-house gitignore matcher in `loa::runtime` (nested files, negation, `**`) |
| notify-rust | libnotify |
| semver | in-house `loa::runtime::semver` |
| tracing | `loa::log` (levels from env `LOA_LOG`, stderr) |
| tempfile | `mkstemp`/`mkdtemp` RAII wrappers in `loa::runtime` |
| iced, iced_test | Qt 6.4 Widgets, QtSvg, QtPdf; logic tests on C++ `State` with doctest |
| iced markdown | md4c |

F1 apt line (also used by CI):
```
sudo apt-get install -y build-essential cmake ninja-build ccache qt6-base-dev qt6-pdf-dev qt6-svg-dev qt6-tools-dev qt6-image-formats-plugins libsqlite3-dev libssl-dev libcurl4-openssl-dev libssh-dev libvterm-dev libarchive-dev libsecret-1-dev libjpeg-turbo8-dev libpng-dev libwebp-dev libgif-dev zlib1g-dev libopenh264-dev libtomlplusplus-dev doctest-dev libasio-dev libpugixml-dev libxlsxwriter-dev libcpp-httplib-dev libstb-dev libnotify-dev libxcb1-dev libxcb-shm0-dev libxcb-xtest0-dev libmd4c-dev libyaml-cpp-dev libminizip-dev
```
F3 and F4 also need a Rust toolchain (`rustup`, stable) to prebuild the FFI static libraries once. Their Cargo target directory is `${LOA_RUST_FFI_TARGET:-$HOME/.cache/loa/rust-ffi-target}`, shared across worktrees.

## Qt UI rules

- `cpp/ui/<name>/` contains:
  - `state.hpp/.cpp`: plain C++, no Qt. It defines `struct State`, `using Message = std::variant<…>`, `using Event = std::variant<…>`, and `std::pair<loa::runtime::Task, std::optional<Event>> State::update(Message)`. `loa::runtime::Task` mirrors `iced::Task`.
  - `view.hpp/.cpp`: a `QWidget` subclass. It renders `const State&`, emits Messages through one `message(Message)` signal and never mutates `State`.
- Session data arrives as `const loa::core::Dom&` or as patches.
- Colors come only from `loa::ui::theme` tokens, which map the iced extended palette names 1:1. No color literals.
- Custom widgets call `setAccessibleName`.

## Test policy

- **Port** each crate's integration tests (`tests/`) and public-API unit tests to doctest in `tests/`. **Do not port** tests that only check private implementation details, or `iced_test` render and pixel tests.
- **Fast iteration:** build and run only your own target. No full-tree builds while iterating.
- **Standard acceptance for library issues:**
  - [ ] `cmake --build build --target loa_<name>_tests && ./build/libs/<name>/loa_<name>_tests` passes.
  - [ ] Golden fixtures for this library's formats round-trip byte-identically.
- **Standard acceptance for UI issues:**
  - [ ] `cmake --build build --target loa_ui_<name>_tests loa-qt` succeeds.
  - [ ] The state tests pass.
  - [ ] `QT_QPA_PLATFORM=offscreen timeout 5 ./build/app/loa-qt --smoke <screen>` exits 0.
- **P3 additionally:**
  - [ ] A C++ node and a Rust node built from `origin/main` connect over iroh on loopback and exchange one `omp/control/1` message.
  - [ ] The same works over the TCP fallback.

## Crate → library map

The Rust workspace on `origin/main` has 27 crates. Each maps to one C++ target; the general rule (`cpp/libs/<crate without loa->`) holds except where noted.

|Rust crate|C++ target|Namespace|
|---|---|---|
|`loa-util`|`cpp/libs/runtime` (folded into the foundation library)|`loa::runtime`|
|`loa-core`|`cpp/libs/core`|`loa::core`|
|`loa-contracts`|`cpp/libs/contracts`|`loa::contracts`|
|`loa-view`|`cpp/libs/view`|`loa::view`|
|`loa-bus`|`cpp/libs/bus`|`loa::bus`|
|`loa-session`|`cpp/libs/session`|`loa::session`|
|`loa-settings`|`cpp/libs/settings`|`loa::settings`|
|`loa-runtime`|`cpp/libs/app_runtime` (renamed: `runtime` is the foundation library)|`loa::app_runtime`|
|`loa-harness`|`cpp/libs/harness` (one subdirectory per adapter)|`loa::harness`|
|`loa-mcp`|`cpp/libs/mcp`|`loa::mcp`|
|`loa-remote`|`cpp/libs/remote`|`loa::remote`|
|`loa-projects`|`cpp/libs/projects`|`loa::projects`|
|`loa-models`|`cpp/libs/models`|`loa::models`|
|`loa-surfaces`|`cpp/libs/surfaces` (one subdirectory per module: proto, vnc, browser, docs, hub)|`loa::surfaces`|
|`loa-trust`|`cpp/libs/trust`|`loa::trust`|
|`loa-p2p`|`cpp/libs/p2p` (in-house yamux lives here)|`loa::p2p`|
|`loa-sandbox`, `loa-sandbox-stub`|`cpp/libs/sandbox`, executable `loa-sandbox-stub`|`loa::sandbox`|
|`loa-updater`|`cpp/libs/updater`|`loa::updater`|
|`loa-ui-kit`|`cpp/ui/kit` (theme tokens are `loa::ui::theme`)|`loa::ui::kit`|
|`loa-ui-shell`, `loa-ui-session`, `loa-ui-panel`, `loa-ui-terminal`, `loa-ui-surface`, `loa-ui-settings`|`cpp/ui/<name>`; a crate with several screens gets one `state`/`view` pair per screen|`loa::ui::<name>`|
|`loa-app`|`cpp/app` (executable `loa-qt`)|`loa::app`|

UI modules depend only on `loa::view` and `cpp/ui/kit`, never on contracts, settings, surfaces or any domain library, mirroring the `loa-view` rule in `docs/ARCHITECTURE.md`. Where the Qt UI rules say session data arrives as `loa::core::Dom`, read: as the `loa::view` session view models.
