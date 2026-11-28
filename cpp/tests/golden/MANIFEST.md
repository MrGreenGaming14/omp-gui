# Golden fixtures

Byte-exact files produced by the Rust code at source SHA `6853d95bd272adf65b37e64c449b02c08813bc53`.
The C++ readers must decode these and, where they write, reproduce the same bytes.
`loa_golden_tests` re-hashes every row below with OpenSSL EVP.

Regenerate (from the repo root, any `CARGO_TARGET_DIR`):

```sh
export LOA_GOLDEN_OUT=$PWD/cpp/tests/golden
cargo test -p loa-bus --test golden -- --ignored
cargo test -p loa-settings --test config write_golden -- --ignored
cargo test -p loa-settings --lib write_golden -- --ignored
cargo test -p loa-contracts --test golden -- --ignored
```

Then update this table and the source SHA.

## Formats

- Journal file (`*.loaj`): preamble `LOAJRNL\0`, then version u32 LE (1), then records.
  Each record is `[len u32 LE][crc32 u32 LE][payload]`. The first record is the header,
  prost `{session_id = 1 string, parent_session = 2 optional string, forked_at = 3 optional uint64}`;
  every later record is a prost `Patch` (see `crates/loa-bus/src/proto.rs`).
- Snapshot file (`*.loas`): preamble `LOASNAP\0`, then version u32 LE (1), then fixed LE fields.
  The crc32 of the bytes before offset 48 is stored LE at offset 48; a prost `Dom` follows.
- `bus/patch-NN.pb`: prost `Patch` for seq NN of `journal/g/journal.loaj`; `bus/dom.pb`: prost `Dom` after all of them.
  Together they cover every `Op` (Insert, Insert at index, Set with value, Set without value, Append, Remove)
  and every `Value` (bool, sint64, double, string, bytes, list).
- `journal/f`, `journal/s`: copies of `crates/loa-bus/tests/fixtures/journal`.
- `settings/config.toml`, `settings/unknown.toml`: copies of `crates/loa-settings/tests/fixtures/full.toml` and `unknown.toml` (unknown keys kept byte-identical).
- `settings/config.yml`, `settings/models.yml`: written by `OmpConfig::save` / `ModelsYml::save` after a role edit.
- `contracts/*.schema.json`: `HostToolSpec::input_schema` (schemars 1.x via `DynTool::spec`) for the argument
  structs in `crates/loa-contracts/tests/golden.rs`; `loa_contracts_tests` rebuilds each with `loa::contracts::schema`.

## Files

| path | length | sha256 | rust source sha |
|---|---|---|---|
| bus/dom.pb | 525 | 976ec692b392efff09d128efd2d7713b3a4b819a7c1a6b728a78a2fef585e550 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| bus/patch-01.pb | 86 | 80b5931e19545fc392a7e1a8973a2ed372d832ae623acf09d10068fcf58fc88d | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| bus/patch-02.pb | 419 | 6b268b0e7a8e1c0737226e40197e90d254e67af8441ec4332d7fd392c6c6362d | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| bus/patch-03.pb | 40 | fd4a4ac2efa2daea9ecccbd4376cb3aa0cf1ec1ac3ae0b2d12bccca8f4e43bcd | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| bus/patch-04.pb | 31 | 736ee8c22c5fc7bd0b36d620abc79e5ccc110421105951f3e1debffc0e5a51a1 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| bus/patch-05.pb | 23 | bee9d7a6bea2926c7dc390e3f8e2240004e1fe72b6e4edaf274c7ad800f34eda | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| bus/patch-06.pb | 21 | bbaf5cf0147c062f01a03ca708e30cb9cdf1a8a38823aaf28b5739dad20a4a74 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| bus/patch-07.pb | 38 | 1649caa1a2860f3bf1d32b932ff18ab86bad1803742e192f1cdcade066569586 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| bus/patch-08.pb | 15 | d6d7959935e1788c6a2dae0f7f4c7d664662e1acf79c0475f7099ebaf5a21859 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| contracts/args.schema.json | 229 | 70881f67193954277551de9841222519ffd6d1febc640e98cd29f934b43479b0 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| contracts/data_args.schema.json | 962 | 8aacf0688857367c3452dbf11ba7634c28ecd13c08a5025713ca6d1cb7726c69 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| contracts/no_args.schema.json | 120 | 068f287b973e1cb2cb111022b6c9b77db19e4c84704b640dcd3034e8abe682c4 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| contracts/search_args.schema.json | 1483 | ffdc2eef6fa051ac05480e99970465c1d6e0a4bdb5ef62a8adf3a4b7fce7dd52 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| journal/f/journal.loaj | 28 | fc7fc20ca23862d10f7abb288d92756a6db93e297dbacaea6907ee8a2d103940 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/g/journal.loaj | 760 | c1bebc8200e456c3330d395a64e3ddb84bc8a648ea710324d24f22d13aab4777 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/s/journal.loaj | 2180 | eec47b483bacb5599cae3ad4e8392f1b845bbb6277288eb8756a70468e6801ef | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/s/snap-00000000000000000016.loas | 678 | 1357b93c16f73b19a21f8de1843b5a5fea6f06152287d5aac99b09522c2bab34 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/s/snap-00000000000000000032.loas | 936 | 66b29261fccb06c881c5f7aaa025086e366e007e855fb80cb77d6b8dbbf21923 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/config.toml | 1325 | 0e1608a486264a50fd0e32f0e5d54434f8acdc6fda84c40d37225a51f7dd5ee2 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/config.yml | 419 | 532f8114e105bbede85b0384b6219906f228e3f6f5f1eede38a82526b95b4c33 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/models.yml | 393 | 42a0270620cb325567227ae0a675f28957fb53186def72ae5837493602f4b88b | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/unknown.toml | 338 | 1f982a717c8ec3ac55a0b9ac3ebd12e420145b558c578785b79cbecef2c40ef3 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
