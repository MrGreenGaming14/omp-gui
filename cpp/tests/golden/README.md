# Regenerating the golden fixtures

From the repo root, any `CARGO_TARGET_DIR`:

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
cargo run --manifest-path cpp/libs/harness/tools/golden_gen/Cargo.toml
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
| `crates/loa-surfaces/src/proto/golden.rs` `write_golden_surface_wire` | `surface/*` |
| `crates/loa-remote/src/audit_golden.rs` `write_golden_remote_audit` | `remote/*` |
| `crates/loa-contracts/tests/golden.rs` `write_golden_tool_schemas` | `contracts/*` |
| `crates/loa-util/tests/golden.rs` | `util/iso.txt`, `util/ssh/aliases.txt` (`util/ssh/config`, `util/ssh/conf.d/*` are hand-written inputs) |
| `cpp/libs/surfaces/tests/docs-golden` (scratch binary on `loa-surfaces` feature `docs`; `crates/` stays untouched) | `docs/*` |
| `cpp/libs/harness/tools/golden_gen` (standalone crate over `loa-harness`; outside the workspace) | `harness/*` |
