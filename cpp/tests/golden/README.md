# Regenerating the golden fixtures

From the repo root, any `CARGO_TARGET_DIR`:

```sh
export LOA_GOLDEN_OUT=$PWD/cpp/tests/golden
cargo test -p loa-bus --test golden -- --ignored
cargo test -p loa-settings --test config write_golden -- --ignored
cargo test -p loa-settings --lib write_golden -- --ignored
cargo test -p loa-surfaces --features remote-video --lib write_golden -- --ignored
cargo test -p loa-remote --lib write_golden -- --ignored
```

Then update the rows of `MANIFEST.md` (length, sha256, source SHA) for every changed file.
`loa_golden_tests` fails while a file and the table disagree.

| Rust test | writes |
|---|---|
| `crates/loa-bus/tests/golden.rs` `write_golden_journal_and_bus` | `bus/*`, `journal/*` |
| `crates/loa-settings/tests/config.rs` `write_golden*` | `settings/config.toml`, `settings/unknown.toml`, `settings/config.yml` |
| `crates/loa-settings/src/models/settings.rs` `write_golden*` | `settings/models.yml` |
| `crates/loa-surfaces/src/proto/golden.rs` `write_golden_surface_wire` | `surface/*` |
| `crates/loa-remote/src/audit_golden.rs` `write_golden_remote_audit` | `remote/*` |
