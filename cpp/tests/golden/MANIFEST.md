# Golden fixtures

Byte-exact files produced by the Rust code at the source SHA in each row.
The C++ readers must decode these and, where they write, reproduce the same bytes.
`loa_golden_tests` re-hashes every row below with OpenSSL EVP.

Regeneration commands and which Rust test writes which file: `README.md`.
Every file in this directory except `CMakeLists.txt`, `MANIFEST.md`, `README.md` and `golden_test.cpp`
has exactly one row below; `loa_golden_tests` fails otherwise.

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
- `surface/stream-*.bin`: one length delimited prost `StreamMessage` of `omp/surface/1` (VERSION 1,
  `crates/loa-surfaces/src/proto/wire.rs`), one per `Body` (Open, Accept with pixel and document info, Reject,
  Resize, Close, KeyframeRequest, Frame, DocumentHeader, DocumentChunk). `stream-input-*.bin` are `Body::Input`
  with each `InputKind` (tags 3 to 8: MouseMoveMsg, MouseButtonMsg, ScrollMsg, KeyMsg, TextMsg, Resize).
- `surface/datagram-fragment.bin`: one encoded `Datagram` (not length delimited) holding a `Fragment` with a header.
- `surface/stream-unknown-body.bin`: a valid length delimited `Close`, then a length delimited message holding only
  field 99 (`9a 06 02 aa bb`). The second message MUST decode as `body: None` and be skipped.
- `remote/remote_audit.sql`: the `SCHEMA` constant of `crates/loa-remote/src/audit.rs` byte for byte
  (`CREATE TABLE remote_audit` and the indexes `remote_audit_session`, `remote_audit_host`).
  sqlite stores each statement without `IF NOT EXISTS` and the `;`.
- `remote/remote_audit.rows.json`: `SELECT * FROM remote_audit ORDER BY id` after four `SqliteAudit::record` calls.

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
| journal/f/journal.loaj | 28 | fc7fc20ca23862d10f7abb288d92756a6db93e297dbacaea6907ee8a2d103940 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/g/journal.loaj | 760 | c1bebc8200e456c3330d395a64e3ddb84bc8a648ea710324d24f22d13aab4777 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/s/journal.loaj | 2180 | eec47b483bacb5599cae3ad4e8392f1b845bbb6277288eb8756a70468e6801ef | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/s/snap-00000000000000000016.loas | 678 | 1357b93c16f73b19a21f8de1843b5a5fea6f06152287d5aac99b09522c2bab34 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/s/snap-00000000000000000032.loas | 936 | 66b29261fccb06c881c5f7aaa025086e366e007e855fb80cb77d6b8dbbf21923 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/config.toml | 1325 | 0e1608a486264a50fd0e32f0e5d54434f8acdc6fda84c40d37225a51f7dd5ee2 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/config.yml | 419 | 532f8114e105bbede85b0384b6219906f228e3f6f5f1eede38a82526b95b4c33 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/models.yml | 393 | 42a0270620cb325567227ae0a675f28957fb53186def72ae5837493602f4b88b | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/unknown.toml | 338 | 1f982a717c8ec3ac55a0b9ac3ebd12e420145b558c578785b79cbecef2c40ef3 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| remote/remote_audit.rows.json | 895 | 1d57706b8d18f85bbbff2a005ddd4b8b70caead55ec0f80823306bc5438cfde6 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| remote/remote_audit.sql | 402 | b7334365648a9196c6a55413655a3d21c81ea5b3a9e26eec6796eff77069bfdd | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/datagram-fragment.bin | 47 | ae284eeebe478e5024f9d60da284428924c034a62241211f37dc26b9ebc98b84 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-accept-document.bin | 60 | 663ab4a7c7ec24c64632fc1c8f4668fd307d8ce27332e39ba12dc806e5690a1c | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-accept.bin | 59 | bdb2a0bba175a7d7e032de44b0c0d9f320df64321319219481c328047bc471eb | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-close.bin | 8 | d7c5f535400ea81c82827a72dd4a94b410f1285d4ae4cb32e844cd9ef31cdc66 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-document-chunk.bin | 10 | 560a3f411b39e5f794e4abf1b5d14fb8ba7a3d1bc2ef3c130112aaf3d825e1b0 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-document-header.bin | 35 | 027ea44f5a951733bf8094db9230e4c4d8990b85c79d08589117867645d9f4be | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-frame.bin | 46 | 44d855e5869be56ab4d4c54416cb40ee98a0b054212d5a43de55dbf381a6cd70 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-input-key.bin | 25 | 4559feea23687623335e7073e9e268645d304dbde53f3b6bdc33890d8897a658 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-input-mouse-button.bin | 25 | 5655058b8496f725d3ac1e30f9dd34908e814e19c73d47727efbb6117ff0d9ad | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-input-mouse-move.bin | 19 | 5d49a23ddc03ca5d0785d176bb040b7213bc158eff4dadea225f6b8ba3e17ce7 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-input-resize.bin | 15 | 81ba62d74b6537b03fc312527993e624bc9f71ec9dae22ebf5c56053e1ab157c | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-input-scroll.bin | 31 | 9e65d5f7e387483679fb56a87aa1d1bbe446eccf67406cc2e075a433d809acee | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-input-text.bin | 17 | f90abe127454ee2ad46bdbaf264c4adfd88fd29a451cd8c2627196b35a768ddc | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-keyframe-request.bin | 3 | 9e9a02fbfab3c0130e0416734eefba5dbe09feddc61272fa254e23df06d4b8d6 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-open.bin | 24 | 1c97c4b74b06cf3b9b7741d0509740209091aa792b61f4f68349e776cb3e369e | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-reject.bin | 22 | 24fa6532b33efd94a367fdf7bfcf6e6daf0d0a3c204adb5d2c0a7420b040e44f | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-resize.bin | 9 | 93c16b99ec9cf90756ca9aafd9f6676fe9c609b14ace298b4ac290f4a5ad1523 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| surface/stream-unknown-body.bin | 14 | ac72d470973eb97154fc66ee221cb9cf8d921843857e7847b8b1eecd1f6c4dd3 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
