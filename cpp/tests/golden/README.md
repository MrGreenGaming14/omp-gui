# Regenerating the golden fixtures

The fixtures were written by the Rust app and its generator crates. Both were removed at the C++
cutover (#410); the C++ tree only reads them. To regenerate, check out the last commit with the Rust
workspace, `ecc0cf0` (`git worktree add ../omp-gui-rust ecc0cf0`), and run the commands below from
its repo root, with any `CARGO_TARGET_DIR`:

```sh
export LOA_GOLDEN_OUT=$PWD/cpp/tests/golden
cargo test -p loa-bus --test golden -- --ignored
cargo test -p loa-settings --test config write_golden -- --ignored
cargo test -p loa-settings --lib write_golden -- --ignored
cargo test -p loa-p2p --lib write_golden -- --ignored
cargo test -p loa-trust --lib write_golden -- --ignored
cargo test -p loa-surfaces --features remote-video --lib write_golden -- --ignored
cargo test -p loa-remote --lib write_golden -- --ignored
cargo test -p loa-contracts --test golden -- --ignored
cargo test -p loa-util --test golden -- --ignored
(cd cpp/libs/surfaces/tests/docs-golden && cargo run)
(cd cpp/libs/settings/tests/agents-golden && cargo run)
(cd cpp/libs/mcp/tests/import-golden && cargo run -- "$LOA_GOLDEN_OUT/mcp" > "$LOA_GOLDEN_OUT/mcp/import.expected.json")
cargo run --manifest-path cpp/libs/harness/tools/golden_gen/Cargo.toml
cargo run --manifest-path cpp/libs/sandbox/tools/golden_gen/Cargo.toml -- golden cpp/tests/golden/sandbox
cargo run --manifest-path cpp/libs/projects/tools/golden_gen/Cargo.toml -- cpp/tests/golden/projects
(cd cpp/libs/p2p/tests/interop && cp ../../../../../Cargo.lock . && cargo run -- yamux-capture "$LOA_GOLDEN_OUT/p2p/yamux/rust-client.bin"; rm Cargo.lock)
cargo run --manifest-path cpp/libs/updater/tools/golden_gen/Cargo.toml -- cpp/tests/golden/updater
cargo run --manifest-path cpp/libs/surfaces/tools/hub_golden/Cargo.toml -- cpp/tests/golden/surface/hub
```

Then update the rows of `MANIFEST.md` (length, sha256, source SHA) for every changed file.
`loa_golden_tests` fails while a file and the table disagree.

| Rust test | writes |
|---|---|
| `crates/loa-bus/tests/golden.rs` `write_golden_journal_and_bus` | `bus/*`, `journal/*` |
| `crates/loa-settings/tests/config.rs` `write_golden_toml` | `settings/config.toml`, `settings/unknown.toml` |
| a scratch binary on `loa-settings` (no Rust test writes them): for `config.toml` and `unknown.toml`, `ConfigStore::load` then `save`; `ConfigStore::save(&Config::default())`; load+save of `version = 1` with a `[theme]` table holding `name = "nord"`, `accent = "#88c0d0"`, `x = { a = [1, 2.5, "q\"\n"], b = 1979-05-27T07:32:00.5-07:00 }` and `[network] relay = "off"`; for `unknown.toml`, the `unknown_keys_survive_load_edit_save` edit | `settings/config.saved.toml`, `settings/unknown.saved.toml`, `settings/default.saved.toml`, `settings/theme.saved.toml`, `settings/unknown.edited.toml` |
| `crates/loa-settings/src/models/settings.rs` `write_golden_yml` | `settings/config.yml`, `settings/models.yml` |
| `cpp/libs/settings/tests/agents-golden` (scratch binary on `loa-settings`; reads the hand-written inputs `settings/agent.md`, `settings/agent-crlf.md`, `settings/workflows/RULES.md`, `settings/workflows/config.yml`) | `settings/agent*.edited.md`, `settings/agent.new.md`, `settings/workflows/*.created.*`, `settings/workflows/*.edited.*` |
| `crates/loa-p2p/src/golden.rs` `write_golden_wire_and_peers` | `p2p/*`, `sqlite/net_peers.*` |
| `crates/loa-trust/src/golden.rs` `write_golden_trust_and_grants` | `trust/*`, `sqlite/grants.*` |
| `crates/loa-surfaces/src/proto/golden.rs` `write_golden_surface_wire` | `surface/*.bin` |
| `cpp/libs/surfaces/tools/hub_golden` (standalone crate over `loa-surfaces` feature `hub`; outside the workspace) | `surface/hub/*` |
| `crates/loa-remote/src/audit_golden.rs` `write_golden_remote_audit` | `remote/*` |
| `crates/loa-contracts/tests/golden.rs` `write_golden_tool_schemas` | `contracts/*` |
| `crates/loa-util/tests/golden.rs` | `util/iso.txt`, `util/ssh/aliases.txt` (`util/ssh/config`, `util/ssh/conf.d/*` are hand-written inputs) |
| `cpp/libs/surfaces/tests/docs-golden` (scratch binary on `loa-surfaces` feature `docs`; `crates/` stays untouched) | `docs/*` |
| `cpp/libs/p2p/tests/interop` `yamux-capture` (scratch binary on `loa-p2p` and `yamux` 0.14); `p2p/yamux/cpp-client.bin` is what `loa_p2p_tests` asserts the C++ `yamux::Session` writes | `p2p/yamux/rust-client.bin` |
| `cpp/libs/harness/tools/golden_gen` (standalone crate over `loa-harness`; outside the workspace) | `harness/*` |
| `cpp/libs/mcp/tests/import-golden` (scratch binary on `loa-mcp`; reads the hand-written `mcp/home/*`, `mcp/project/*` configs) | `mcp/import.expected.json` |
| `cpp/libs/sandbox/tools/golden_gen` `golden` (standalone crate over `loa-sandbox`; its `client <abs path to stub>` mode runs the Rust `StubClient` against a stub binary) | `sandbox/*` |
| `cpp/libs/projects/tools/golden_gen` (standalone crate over `loa-projects`; outside the workspace) | `projects/*` |
| `cpp/libs/updater/tools/golden_gen` (standalone crate over `loa-updater`, `loa-settings` and `loa-util`; signs artifacts with Rust Ed25519 and writes a swapped state directory) | `updater/*` |
