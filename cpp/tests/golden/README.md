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
```

Then update the rows of `MANIFEST.md` (length, sha256, source SHA) for every changed file.
`loa_golden_tests` fails while a file and the table disagree.

| Rust test | writes |
|---|---|
| `crates/loa-bus/tests/golden.rs` `write_golden_journal_and_bus` | `bus/*`, `journal/*` |
| `crates/loa-settings/tests/config.rs` `write_golden_toml` | `settings/config.toml`, `settings/unknown.toml` |
| `crates/loa-settings/src/models/settings.rs` `write_golden_yml` | `settings/config.yml`, `settings/models.yml` |
| `crates/loa-p2p/src/golden.rs` `write_golden_wire_and_peers` | `p2p/*`, `sqlite/net_peers.*` |
| `crates/loa-trust/src/golden.rs` `write_golden_trust_and_grants` | `trust/*`, `sqlite/grants.*` |
| `crates/loa-surfaces/src/proto/golden.rs` `write_golden_surface_wire` | `surface/*` |
| `crates/loa-remote/src/audit_golden.rs` `write_golden_remote_audit` | `remote/*` |
| `crates/loa-contracts/tests/golden.rs` `write_golden_tool_schemas` | `contracts/*` |
| `crates/loa-util/tests/golden.rs` | `util/iso.txt`, `util/ssh/aliases.txt` (`util/ssh/config`, `util/ssh/conf.d/*` are hand-written inputs) |
