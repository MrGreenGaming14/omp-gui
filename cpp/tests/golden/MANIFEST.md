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
- `settings/*.saved.toml`, `settings/unknown.edited.toml`: what `ConfigStore::save` writes (`toml::to_string` of the
  config table: keys sorted, dotted table headers, empty tables kept) after loading `config.toml`, `unknown.toml`, the
  default config and a `[theme]` table with nested overrides; `unknown.edited.toml` after the `unknown_keys_survive_load_edit_save` edit.
- `settings/config.yml`, `settings/models.yml`: written by `OmpConfig::save` / `ModelsYml::save` after a role edit.
- `settings/agent.md`, `settings/agent-crlf.md`, `settings/workflows/RULES.md`, `settings/workflows/config.yml`: hand-written
  inputs (an omp agent file with YAML frontmatter, LF and CRLF; a RULES.md with hand-written, managed and external workflow
  sections). `settings/agent*.edited.md`, `settings/agent.new.md`, `settings/workflows/*.created.*`, `*.edited.*`: what
  `AgentDefinition::to_text` and `WorkflowFiles::save`/`delete` write after the edits in `cpp/libs/settings/tests/agents-golden`.
- `mcp/home/.claude.json`, `mcp/project/.mcp.json`, `mcp/home/.cursor/mcp.json`, `mcp/project/.cursor/mcp.json`,
  `mcp/home/.codex/config.toml`: hand-written MCP configs of Claude Code, Cursor and Codex covering every dialect key and
  every refusal. `mcp/import.expected.json`: what the Rust `JsonServers`/`CodexToml` readers return for them (each entry's
  serde `ServerSpec` or its reason) and the `plan_found` outcomes, as `serde_json::to_string_pretty` plus a newline.
- `p2p/*.bin`: the bytes `crates/loa-p2p/src/wire.rs` `send` writes: a 4-byte BIG-endian length (wire frames are
  big-endian; journal records are little-endian), then the prost message. The length excludes the prefix, is at most
  `MAX_FRAME_SIZE` (1 MiB = 1048576) and never 0: an empty or oversized message is refused before writing, and a read
  of either size is a protocol error. `control-<body>.bin` is a `ControlFrame` per body (Hello, Presence,
  GrantDelivery, GrantRenewal, GrantRenewalResult with token and refused, GrantRevocation, Ack and refused Ack);
  `frame-<body>.bin` is a session `Frame` per body (Subscribe, Update, Drive, Challenge, Present, ListSessions,
  SessionList, Seed, DriveResult with patch and conflict, Prompt, Ack and refused Ack, Answer);
  `folder-request-*.bin`, `folder-reply-*.bin` and `wire-entry.bin` are the bare folder messages.
  `p2p/labels.txt`: the protocol labels in hello order, one per line (`omp/control/1`, `omp/session/1`, `omp/folder/1`).
- `trust/node.key`: the 32 raw seed bytes `FileKeyStore` writes to `node.key` (mode 0600) for the seed `[7; 32]`;
  `trust/node.pub`: its public key as 64 lowercase hex. The OS keychain entry is service `loa`, user `node-key`.
- `trust/v1_reshared_token.bin`: a copy of `crates/loa-trust/tests/fixtures/v1_reshared_token.bin`;
  `trust/v1_reshared_token.keys`: the root (seed `[1; 32]`) and holder (seed `[3; 32]`) public keys it verifies with.
- `sqlite/net_peers.sql`, `sqlite/grants.sql`: the exact schema text executed by `PeerDirectory::open`
  (`crates/loa-p2p/src/peers_db.rs`) and `GrantStore::with_connection` (`crates/loa-trust/src/store.rs`).
- `sqlite/net_peers.rows.json`, `sqlite/grants.rows.json`: every row, columns in table order (blobs as `{"hex": ...}`),
  after one save of each row kind (a saved and observed peer; an issued then revoked grant, an extra revocation,
  the fixture token received then refused) and a load through the Rust store.
- `surface/stream-*.bin`: one length delimited prost `StreamMessage` of `omp/surface/1` (VERSION 1,
  `crates/loa-surfaces/src/proto/wire.rs`), one per `Body` (Open, Accept with pixel and document info, Reject,
  Resize, Close, KeyframeRequest, Frame, DocumentHeader, DocumentChunk). `stream-input-*.bin` are `Body::Input`
  with each `InputKind` (tags 3 to 8: MouseMoveMsg, MouseButtonMsg, ScrollMsg, KeyMsg, TextMsg, Resize).
- `surface/datagram-fragment.bin`: one encoded `Datagram` (not length delimited) holding a `Fragment` with a header.
- `surface/stream-unknown-body.bin`: a valid length delimited `Close`, then a length delimited message holding only
  field 99 (`9a 06 02 aa bb`). The second message MUST decode as `body: None` and be skipped.
- `docs/sample.pdf`: a four page PDF (Helvetica text, a /Rotate 90 page, an A4 page with fractional size) built like
  the `loa-surfaces` `docs/pdf.rs` test; `docs/sample.sizes.txt`: `<page> <width> <height> <scale> <render width> <render height>`
  per page from `PdfDoc`; `docs/text.json`: `DataQuery::Text { page: None }` on it.
- `docs/info.json`: `DocSurface::data(DataQuery::Info)` (`DocInfo`) after `DocsServer::open_file("sample.pdf")`;
  page sizes are f32 widened to f64 by serde_json. `docs/queries.jsonl`: `serde_json::to_string` of each `DataQuery`
  variant (`kind` tag, snake_case; `None` fields are written as `null`), one per line.
- `docs/sample.docx` (docx-rs: styles with `basedOn`, nested decimal/letter/roman and bullet lists, a table, page
  breaks, 30 body paragraphs), `docs/sample.xlsx` (rust_xlsxwriter: `Data` with strings, floats, a bool, a cached
  formula, date/datetime/time/duration formats; an empty `Empty`; `Offset` from D3) and `docs/sample.pptx` (the
  `pptx.rs` test deck plus a table, a master inherited title, `mc:AlternateContent` and a slide number).
  `docx.outline.json`/`docx.text.json`: `DocxDoc::query` Outline / Text { page: None }; `docx.pages.txt`:
  `<page count> <page count at body 24 pt> <width> <height>`. `xlsx.sheets.json`: Sheets; `xlsx.cells.jsonl`: one
  Cells answer per line (`None` `A1:C6`, `offset` `$E$4`, `Offset` `A1:Z1000`, `Empty` `B2:C3`, `Data` `c2`);
  `xlsx.sizes.txt`: `<sheet> <width> <height>`. `pptx.slides.json`/`pptx.text.json`: Slides / Text { page: None }.
  `office.errors.tsv`: `<file>\t<query json>\t<error Display>` for every failing `DocSurface::data` query.
- `remote/remote_audit.sql`: the `SCHEMA` constant of `crates/loa-remote/src/audit.rs` byte for byte
  (`CREATE TABLE remote_audit` and the indexes `remote_audit_session`, `remote_audit_host`).
  sqlite stores each statement without `IF NOT EXISTS` and the `;`.
- `remote/remote_audit.rows.json`: `SELECT * FROM remote_audit ORDER BY id` after four `SqliteAudit::record` calls.
- `contracts/*.schema.json`: `HostToolSpec::input_schema` (schemars 1.x via `DynTool::spec`) for the argument
  structs in `crates/loa-contracts/tests/golden.rs`; `loa_contracts_tests` rebuilds each with `loa::contracts::schema`.
- `util/iso.txt`: `<ms> <format_iso_ms(ms)>` lines from `loa_util::time`.
- `util/ssh/config` and `util/ssh/conf.d/*` (hand-written, with Include, wildcards, `!` and `=`); `util/ssh/aliases.txt`: `loa_util::ssh::aliases(config)`, one per line.
- `harness/jsonrpc.jsonl`: `JsonRpc2` frames (`header` true, then false): requests, notifications with and without
  params, and responses for a result, an error with a numeric code, one with a non-numeric code and every `RpcError`.
- `harness/rpc_errors.txt`: the `Display` text of each `RpcError` variant, one per line.
- `harness/residual.json`: `residual::attr_name("claude-code")` and `join_parts(["first", "", "trois é", "4"], "\n\n")`.
- `harness/discover.tsv`: `native::discover` over a fixed home tree with set mtimes:
  `harness`, path relative to home, project, title, modified ms, size, tab-separated, in discovery order.

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
| docs/docx.outline.json | 13669 | 7ffd25b4a4ee46fad151b9f688edb2ff7924ac212b5b4c093cbe03cb82f1cd3b | ffbbfe4d1587f76f31fc143c23cbfc1dbe8cc1df |
| docs/docx.pages.txt | 13 | 78c7ac09c5be39ec6cf5970076e8ae80b78ea4db52b618911213a1881b89de8b | ffbbfe4d1587f76f31fc143c23cbfc1dbe8cc1df |
| docs/docx.text.json | 2434 | dc466e98713763cbf8f156be4d9a347ba6cc670e500603e5b08c36caf6c4689c | ffbbfe4d1587f76f31fc143c23cbfc1dbe8cc1df |
| docs/info.json | 264 | e25537a0e0e80441156f51571778cf2647ab59dccabe1d58b15ade93ab1b66f3 | 4466718181c0a6fe99b8d80f86684646549a7cff |
| docs/office.errors.tsv | 1911 | 33e8de7c84005b4b653846563887163804f6f9d98e1f4eb433b7a25da9e4b81b | ffbbfe4d1587f76f31fc143c23cbfc1dbe8cc1df |
| docs/pptx.slides.json | 1269 | 07f81bb3f8a3e20ae75dc30e8849b02f168237d1b43452d698bf9fb16a17c1a9 | ffbbfe4d1587f76f31fc143c23cbfc1dbe8cc1df |
| docs/pptx.text.json | 138 | 58b72aa3498dca1f6d18145df69cfd3b9ee399f1fc50a5ae794544b656c8bc07 | ffbbfe4d1587f76f31fc143c23cbfc1dbe8cc1df |
| docs/queries.jsonl | 215 | 6a0eb2f29fa3978b4676c83c23ac427455b51cfe92d527674fcfedc06be06538 | 4466718181c0a6fe99b8d80f86684646549a7cff |
| docs/sample.docx | 28983 | 723057c9b763c7aa8c027b593988edbc06cbe546ed70efd1901b6ce5fdbd8887 | ffbbfe4d1587f76f31fc143c23cbfc1dbe8cc1df |
| docs/sample.pdf | 1606 | 5ccbb5bbc9353843504c561912e482444d925b6210d05ec86734fa38d83c1003 | 4466718181c0a6fe99b8d80f86684646549a7cff |
| docs/sample.pptx | 4914 | 879fbea0449adf908632bbc41c81a17ae20e40a04c226d00b8f171391edc68d4 | ffbbfe4d1587f76f31fc143c23cbfc1dbe8cc1df |
| docs/sample.sizes.txt | 99 | 1b6c490d85b75e45eaa8f6121e2fd2dd4162cee03881dfa4c84e2ef0e70701fb | 4466718181c0a6fe99b8d80f86684646549a7cff |
| docs/sample.xlsx | 6674 | 6e21da9bc92054a687323e1e660389433f2a906fb7f0efc8860e46c63490cfc0 | ffbbfe4d1587f76f31fc143c23cbfc1dbe8cc1df |
| docs/text.json | 78 | 6cad7c300b483fabef0a41330d24d7018bc5c81d235acc88deb4dd23e59bd4e2 | 4466718181c0a6fe99b8d80f86684646549a7cff |
| docs/xlsx.cells.jsonl | 3076 | 40b403b811165115c23c305a8a7314b21779f8efabf39c3b22dd2077de04a110 | ffbbfe4d1587f76f31fc143c23cbfc1dbe8cc1df |
| docs/xlsx.sheets.json | 154 | def18b61727ef05212dde6613f8525a3fc7e56a59e10f9b43eb83f087428c5f4 | ffbbfe4d1587f76f31fc143c23cbfc1dbe8cc1df |
| docs/xlsx.sizes.txt | 29 | 026afd3ade4d88685a3abfaebbbe3e593387464a8a9fbc9bb2ac73f5a9e33f2c | ffbbfe4d1587f76f31fc143c23cbfc1dbe8cc1df |
| harness/discover.tsv | 376 | 2b69e4f815233b4e35ad736b7fa411f0bea73f53d49d320535668699680ebb7f | f1c2345fbe7ddd1c65b499065fdbe3a1808b78da |
| harness/jsonrpc.jsonl | 3266 | 04d033cb837b99efaf038804e07083e2f1cc2b44769805f312e11af574837fc6 | f1c2345fbe7ddd1c65b499065fdbe3a1808b78da |
| harness/residual.json | 82 | dd58be7676e8f6323a47c561fc78a2cdf610a6609d276b1d8c00955577daaa49 | f1c2345fbe7ddd1c65b499065fdbe3a1808b78da |
| harness/rpc_errors.txt | 554 | 3ec8b40f64608008c9fd831736c935deb7f043c7b9826a1d5f1d01e1098ba7d8 | f1c2345fbe7ddd1c65b499065fdbe3a1808b78da |
| journal/f/journal.loaj | 28 | fc7fc20ca23862d10f7abb288d92756a6db93e297dbacaea6907ee8a2d103940 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/g/journal.loaj | 760 | c1bebc8200e456c3330d395a64e3ddb84bc8a648ea710324d24f22d13aab4777 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/s/journal.loaj | 2180 | eec47b483bacb5599cae3ad4e8392f1b845bbb6277288eb8756a70468e6801ef | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/s/snap-00000000000000000016.loas | 678 | 1357b93c16f73b19a21f8de1843b5a5fea6f06152287d5aac99b09522c2bab34 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/s/snap-00000000000000000032.loas | 936 | 66b29261fccb06c881c5f7aaa025086e366e007e855fb80cb77d6b8dbbf21923 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| mcp/home/.claude.json | 687 | e6dce7220943d5338a959a184195de05da24bbc7f5a6192898ede6af211bda43 | 23c276225f993297e9908138bfe328425a5904e4 |
| mcp/home/.codex/config.toml | 589 | f5848e6ebae077a65a85b3757fe772615808df8f4f6549628414706ae6941a2e | 23c276225f993297e9908138bfe328425a5904e4 |
| mcp/home/.cursor/mcp.json | 255 | 9cf8a2a21244213934f0fa82a220ef8da8153a7c1a7bb7d17242d3796085a838 | 23c276225f993297e9908138bfe328425a5904e4 |
| mcp/import.expected.json | 7397 | 079490ea30b048dbdd4d57ea7064ecb5ffb51e1e7103c6a5ab622c9dde760d46 | 23c276225f993297e9908138bfe328425a5904e4 |
| mcp/project/.cursor/mcp.json | 207 | a77428989234b61963f9e0b84b092a5026c21b2944bd993efa2a47ce87b04d01 | 23c276225f993297e9908138bfe328425a5904e4 |
| mcp/project/.mcp.json | 456 | 212d487e8483bb02b6ae2f30f50b16a93b883639377d6e70375c0e7e495a8a63 | 23c276225f993297e9908138bfe328425a5904e4 |
| p2p/control-ack.bin | 6 | 89982dbb136749d03c2ccd2b69afc33eb1c90314bd00c351bd9f2da24d6f666f | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/control-ack-refused.bin | 10 | d324b6dd9184cb98fd1f7c6700701ee719f6740d9a5def33be3e14f038363512 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/control-grant-delivery.bin | 46 | f1785788247a998b1ffe53f9e2a7d56c66a88fabf3458049349503e7c3c9b4e4 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/control-grant-renewal.bin | 24 | f17df3bc723829c02714c984db88e07b3c4fb9ed2c89ce4735c452bca7162816 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/control-grant-renewal-refused.bin | 15 | e0b8a492cdce877b6f8bc20bb58ce1d0b29dd8aa91651b1f6396ab162a33be33 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/control-grant-renewal-result.bin | 12 | 7265b8dfc55cfc4b841ea1a3df916343010db6008ffcc09cfec55cfe0a7108eb | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/control-grant-revocation.bin | 24 | e15243a828c0f9ae6f0b916ff3bb740d4a67dafe52c13007de21c4c1b0d48684 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/control-hello.bin | 71 | 75f9c9d5742cf2ca44632ae968cfde0bc08c7a00a8441d450facb964ad8091ef | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/control-presence.bin | 15 | ac37af88319c885a140df44bd22a79ec255eedbfd351ee46e7a38a80f630b0d5 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/folder-reply-entries.bin | 28 | 7247c2e04d03c9ad0b1bc2dbd7ad39c41866a5c4ff33dbdc165a635816bc452c | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/folder-reply-refused.bin | 14 | 60b547b83fc0873b017629ca0067195fad4f11d722a65993dd92f61823b52202 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/folder-request-list.bin | 21 | 662d074eb4a4dfa4aeb55c83856b05216fdfef475fa19880eb3a821069498cb8 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/folder-request-write.bin | 31 | 57f4be5019c3b04e236cde24a6d76a10645330165307aaed05212293c256a170 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-ack.bin | 6 | 80660ac9e223b8e9a52a634daa7aaf694f255b4ebcc6269f1fa7b531937d7452 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-ack-refused.bin | 10 | 13168b81fae34582ed7cd9abcc8ad648ce73008c76b27e1fbe4df4438800d2c1 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-answer.bin | 33 | a5bc19b6df8b85908faad604a4000405f278d8f5c619d675976ac05019ad0186 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-challenge.bin | 40 | 917e6f5a564da8f811d386b960247099d620bab735087eee787a37d2d8f558c0 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-drive.bin | 44 | 066e2993bfe1b61ba4c6aa37fbf2209141872050d6bebc6e59ae0fa1d2f9c55b | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-drive-result.bin | 28 | be44e9954f557052bf180410ae483429c19b3d751239a1797bd6d837e6166297 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-drive-result-conflict.bin | 18 | d57a69fddcf6187836a67f8841ffe09dab8f20c277e2f25bedce2a3b3367f18e | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-list-sessions.bin | 6 | be7bdd193abecc9b451ee86b158c8badee0596be09c1057d595b2557cd065efb | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-present.bin | 80 | 5bdebee800f3af2da5bad26be14b5d0bd2c5b8186f3e564b278b5a0335c5e609 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-prompt.bin | 27 | 1a97b85b4997ac016093000241b90c0e3238629ff25152fcf451972d98554cbb | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-seed.bin | 131 | 8747206981386a59e3b47f51ff4ebfa0137f2f8b79f0f0d4ff58dec9ba374e73 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-session-list.bin | 76 | 655a5a303c25039ac2012aebfb5805301c9be2fc2806aef60c91a39aa7a7736c | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-subscribe.bin | 16 | d0d658e10b14ef6b640d5115e41771e79e5697518ccb663a6e456a553886b450 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/frame-update.bin | 30 | c19d12a209db61f43c8ca49577e97351a1eaf6e0afbb07fbe8417aa3265c655b | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/labels.txt | 41 | 6634a422f9e65d48499fede47fd5d2151554a41b5cc2002003720069813808a1 | a849b040ba4004d6581283112704296c4e77f026 |
| p2p/wire-entry.bin | 13 | 16019d8665892d3c9b26f849f501efc0dfef2208866502911f3bf3dc6775a1c0 | a849b040ba4004d6581283112704296c4e77f026 |
| settings/agent-crlf.edited.md | 198 | 83aa894260f2c32d8e937ef1786e6eb542c00dd604f57235e418ef18df9d9303 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/agent-crlf.md | 155 | 53004bbbc40a05ed363f461f99eff9b5ec1636a218e2ab897e6dec2288231234 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/agent.edited.md | 244 | 5d157da3dde5ccbb3fa40510c2202eb512de291c52583a4a911ea08ab0e3e78f | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/agent.md | 335 | e23005c1e4fd59fdff1a751db40a39d194ebf12ab8238d631c30a789e0f4bb4e | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/agent.new.md | 112 | 2c083099f70dd40eff7288c42756b2424fc566e17305aaa236517545806c14cc | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/config.saved.toml | 1374 | 7380ccfacf073e09ee4e55955700fc8ec0e34044eea3d09f15067746b35a55b7 | 9d00385fd7812e85efc56e5e7e34ef70c7682e84 |
| settings/config.toml | 1325 | 0e1608a486264a50fd0e32f0e5d54434f8acdc6fda84c40d37225a51f7dd5ee2 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/config.yml | 419 | 532f8114e105bbede85b0384b6219906f228e3f6f5f1eede38a82526b95b4c33 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/default.saved.toml | 339 | b6c110d27df2326953f677dee0c3d4f984cc483c8fae03a711ecc55afbc398cf | 9d00385fd7812e85efc56e5e7e34ef70c7682e84 |
| settings/models.yml | 393 | 42a0270620cb325567227ae0a675f28957fb53186def72ae5837493602f4b88b | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/theme.saved.toml | 430 | c5fdcc6a98b7b82ddae0a4216efc889cf9d8a8021a7f1638d3636850b0c5db38 | 9d00385fd7812e85efc56e5e7e34ef70c7682e84 |
| settings/unknown.edited.toml | 703 | 5a2eac01e0a333efb327b41a15846d5ae5acdc9fb63514db51eee306c6cecc74 | 9d00385fd7812e85efc56e5e7e34ef70c7682e84 |
| settings/unknown.saved.toml | 773 | 905ecfb2159cd8b49cdf0f7435840c8b8b2f54314cd75deb6ecafd9b490330a8 | 9d00385fd7812e85efc56e5e7e34ef70c7682e84 |
| settings/unknown.toml | 338 | 1f982a717c8ec3ac55a0b9ac3ebd12e420145b558c578785b79cbecef2c40ef3 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/workflows/RULES.created.md | 1103 | 3651a5e807697e02e1a1625653d6ee464bbebe7fdce5ceee99ad62619ec53297 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/workflows/RULES.edited.md | 824 | 9f7b4a6b2bb559ba39b46a94efc533ed816f72b51e579983ee17c1392226ece6 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/workflows/RULES.md | 507 | 18c31492e9022de5e69840ac85d36a98af2b7fa6e66b60e29e0dd86c0354b43f | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/workflows/config.created.yml | 160 | 592e46a70736b211d05db70e586c68a92dffcb9ae9366f27e8d514b3d9511a70 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/workflows/config.edited.yml | 143 | 48a7da2adc35dcaef867e188f2ff4f6c9ba6c88b2cb1110ae316bf2ae5a404d7 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| settings/workflows/config.yml | 133 | eb635e797495a37d6d9da7436935fe9ac29ab0ecf25d1d8fb49cd73262201115 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| sqlite/grants.rows.json | 3978 | 4d179c6eb1b61530592f31ae23a7b3bc7742264431d85d18b4152ef47ae1a380 | a849b040ba4004d6581283112704296c4e77f026 |
| sqlite/grants.sql | 501 | 2790821f15bc862a88a0b465fb8c295ba38083167aeb114c8fe16f82c3d80a91 | a849b040ba4004d6581283112704296c4e77f026 |
| sqlite/net_peers.rows.json | 323 | b9e44fe657b48b996d6fab2d1a6f31dcda7896c606e429c96beeb0034e27f71c | a849b040ba4004d6581283112704296c4e77f026 |
| sqlite/net_peers.sql | 238 | 4410ef022c2d43f080e45b8aaaaf41917211299d243066007f0267e03b199f5a | a849b040ba4004d6581283112704296c4e77f026 |
| trust/node.key | 32 | 4bb06f8e4e3a7715d201d573d0aa423762e55dabd61a2c02278fa56cc6d294e0 | a849b040ba4004d6581283112704296c4e77f026 |
| trust/node.pub | 65 | 52ae2a9b4c641fe431a9ca578223ad018baf6fa3eecd4c8c441b9f68fb7a63cc | a849b040ba4004d6581283112704296c4e77f026 |
| trust/v1_reshared_token.bin | 1376 | 5b66e5a5e3039a14daae755125f9412590678f06d668b4246fc259ca5d58e74c | a849b040ba4004d6581283112704296c4e77f026 |
| trust/v1_reshared_token.keys | 142 | 96e9baaf4a558b6c971427ab2b0e57909aab10a501688219712f6a6de3931a44 | a849b040ba4004d6581283112704296c4e77f026 |
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
| util/iso.txt | 280 | 9006e6f63b49bffaee1442bb095e62c95cc5444da3a4f6a59316a13b11fc854b | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| util/ssh/aliases.txt | 43 | 11a0dc406390d101147047702a22186a1ec2c0ee5dc70822a5131f8173a86314 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| util/ssh/config | 100 | db9bb111236f3bf56757b7d396432255fc303eee34c0c481e9f92d506b8ce2a3 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| util/ssh/conf.d/a.conf | 43 | 15921304dd8b16f74da99fd598f9fa52b1289cd0b43541d74765765c26f66ee7 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| util/ssh/conf.d/b.conf | 14 | 39c842d687267318cdc2aa419e73c9eb0735350f2a4d0a2306a6c742f463eeb6 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| util/ssh/conf.d/nested.inc | 18 | 4fdd2b1e906ab64b96697697d38222c5c2e6d0abcf7cd4aedf56f8c50e3995e8 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
