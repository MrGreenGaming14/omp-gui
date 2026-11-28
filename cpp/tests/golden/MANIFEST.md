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
- `mcp/bridge/provider-<case>.request.json` / `.sse`: chat completions request bodies (`serde_json::to_vec`) and the event
  stream `crates/loa-mcp/src/bridge/provider.rs` `completion` answers each with (a call marker in a text part or a string,
  a tool result, a plain prompt, a marker without `tools`). `mcp/bridge/call-prompt.txt`: `call_prompt` for an `edit`
  call; `mcp/bridge/models.yml`, `mcp/bridge/bridge-config.yml`: what `launch` writes (base URL `http://127.0.0.1:40000/v1`).
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
- `remote/remote_bash.schema.json`, `remote/remote_file.schema.json`: `schemars::schema_for!` (1.2.3) of `BashArgs` and
  `FileArgs` in `crates/loa-remote/src/tools.rs`, as `serde_json::to_string`; `loa_remote_tests` rebuilds each.
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
- `harness/configdoc_in.json` and `harness/configdoc_in.toml` (hand-written inputs); `harness/configdoc_out.json` and
  `harness/configdoc_out.toml`: the inputs after the `JsonDoc::set` / `TomlDoc::set` sequences in `golden_gen`
  (serde_json pretty with a trailing newline; toml_edit keeping decor).
- `harness/scripted.txt`: `testing::render` of a `ScriptedAdapter` session through every prompt kind;
  `harness/scripted_violations.txt`: its `check_dom` then `check_history` rules and node ids, one per line.
- `harness/omp2/*.jsonl`: omp2 stdout `patch@1`, `stream@1`, `snapshot@1` and decision frames, with
  simulated sends and answers. Corresponding `*.golden` files are the Rust mapper's DOM and reply output.
- `sandbox/*.bin`: `loa_sandbox::proto::frame` output: a u32 LITTLE-endian body length, then the postcard body
  (varint integers, zigzag `i32`, varint enum variant index, `0`/`1` option tag, varint-length strings and byte
  vectors). `request_*` are `Request`s, `response_*` are `Response`s; `hello.bin` is what `write_hello` writes:
  `MAGIC` (`\0LOA-STUB/2\n`) then the `Ready { version: 2 }` frame.
- `projects/filenames.tsv`: `<project name>\t<file FileSettingsStore writes>` (every byte but ASCII alphanumerics, `-`
  and `_` as uppercase `%XX`, then `.json`). `projects/settings.*.json`: `serde_json::to_vec_pretty` of a full and a
  default `ProjectSettings`; `projects/ideas.md`: `save_ideas` bytes (CRLF, non-ASCII, no final newline).
  `projects/read-*.txt` and `projects/search-*.json`: `project_read`/`project_search` `HostTools::call` output from
  session `b` over the `tests.rs` fixture plus a session `d` whose digest was written twice (`x-own_patches=2`);
  `projects/*.schema.json`: the two tools' input schemas; `projects/prompt-a.txt`: `build_prompt` of session `a`;
  `projects/context.md`: `render_markdown(build_context(..))`; `projects/links.txt`: `.omgui` entries after
  `links::sync` (`name -> target relative to the scratch root`, or `name = {contents:?}`).
- `updater/staged*.json`, `active*.json`, `feeds.json`, `state.json` and `products.json`: Rust
  `loa-updater` serde JSON (pretty, two-space indent), including the manifest, feed cache, decisions
  and default catalog. `updater/root/` is a Rust-written updater state directory after two signed app
  swaps; its `versions/app/<version>/` trees contain the artifact, signature, payload and manifest.
  `updater/sig/` contains a Rust Ed25519 signature over an artifact SHA-256 digest and two public keys.

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
| harness/configdoc_in.json | 52 | 7713d2bee8a1374b16eaf74a8e6b22f8039137fb12441e6f761b60c3a66d0a1c | f1c2345fbe7ddd1c65b499065fdbe3a1808b78da |
| harness/configdoc_in.toml | 207 | b2b2bb4dfb92fc1a87af87144ac2302f6974dd39ffa9d73a4d81eab0a571e12f | f1c2345fbe7ddd1c65b499065fdbe3a1808b78da |
| harness/configdoc_out.json | 151 | 88a0ed2e274b0057e8d7765a9dd357ebf3ccb2a763e439b9eb5742b345f42fcc | f1c2345fbe7ddd1c65b499065fdbe3a1808b78da |
| harness/configdoc_out.toml | 282 | d74215e9a22bdadd99a1fb1340a42a52b94db53b7af6506562e26b48198913d2 | f1c2345fbe7ddd1c65b499065fdbe3a1808b78da |
| harness/scripted.txt | 2896 | fe62fb630a674358a0ebc25d4ac8035dddfb3fd27d95799ef9d2d46bf6225cb5 | f1c2345fbe7ddd1c65b499065fdbe3a1808b78da |
| harness/scripted_violations.txt | 366 | 888668e274bf611f82afc3643e6ad5d8bcff2a654efb690172b2153af900562f | f1c2345fbe7ddd1c65b499065fdbe3a1808b78da |
| harness/omp2/stream.golden | 893 | 3125c5b4cd905cf55fa9f55775582b502c95e7855f732199007a282bbb326d30 | 0990f25fa0d216dbbec2ed80086145bf62b5e0cf |
| harness/omp2/stream.jsonl | 1662 | 1392c96a1457311ead71fa7e14a026fa5fb1181b3a21de37f777005fd01c2e9f | 0990f25fa0d216dbbec2ed80086145bf62b5e0cf |
| harness/omp2/tools.golden | 2058 | 0f655794a2db7e83365bc67d5d3f76353584281b4edcc90c365b7c29158c6a32 | 0990f25fa0d216dbbec2ed80086145bf62b5e0cf |
| harness/omp2/tools.jsonl | 1696 | 98827f188f170a9d23dd2c799348f871cf4132656fd4cf842c16fafb3ba39d01 | 0990f25fa0d216dbbec2ed80086145bf62b5e0cf |
| harness/omp2/tree.golden | 482 | 9e4d0adb8794930fd44ce2267efebcdf002dbc2a31250aafd481f042b07e394a | 0990f25fa0d216dbbec2ed80086145bf62b5e0cf |
| harness/omp2/tree.jsonl | 1901 | 83a47cc0ff480dda8ee5b8c773d93fba52e7db6c16d40d75ad1e4772e6cbc5ee | 0990f25fa0d216dbbec2ed80086145bf62b5e0cf |
| journal/f/journal.loaj | 28 | fc7fc20ca23862d10f7abb288d92756a6db93e297dbacaea6907ee8a2d103940 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/g/journal.loaj | 760 | c1bebc8200e456c3330d395a64e3ddb84bc8a648ea710324d24f22d13aab4777 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/s/journal.loaj | 2180 | eec47b483bacb5599cae3ad4e8392f1b845bbb6277288eb8756a70468e6801ef | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/s/snap-00000000000000000016.loas | 678 | 1357b93c16f73b19a21f8de1843b5a5fea6f06152287d5aac99b09522c2bab34 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| journal/s/snap-00000000000000000032.loas | 936 | 66b29261fccb06c881c5f7aaa025086e366e007e855fb80cb77d6b8dbbf21923 | 6853d95bd272adf65b37e64c449b02c08813bc53 |
| mcp/bridge/bridge-config.yml | 51 | e0fd6f39f81c0cd8f7711ea8bb1a5a0ad659fb646db1c14fcf718f3765e3fd1e | 665f7504038e6295339743c2652a056faa80ec2b |
| mcp/bridge/call-prompt.txt | 142 | 817f348d7edb25cc035dfb6cbf84081a7af2d19171b4a470de17a7f51e4e8a16 | 665f7504038e6295339743c2652a056faa80ec2b |
| mcp/bridge/models.yml | 332 | e2ea4c72278077661878de43ca174f3d96cbfeee4ff3d58a99e1df9c304c9306 | 665f7504038e6295339743c2652a056faa80ec2b |
| mcp/bridge/provider-marked-parts.request.json | 367 | 1086e0a21734fae49b09b4d2500bccfb222fb11af65fb875b7b073e81c406352 | 665f7504038e6295339743c2652a056faa80ec2b |
| mcp/bridge/provider-marked-parts.sse | 518 | 8b988e3126a9ddf8b787f17f8df1c40162e659234fa88166826c477950f629d4 | 665f7504038e6295339743c2652a056faa80ec2b |
| mcp/bridge/provider-marked-string.request.json | 221 | ae46fb8376607bff42902200f9b1d2868ec17864385c7c7bd76d47d62961c417 | 665f7504038e6295339743c2652a056faa80ec2b |
| mcp/bridge/provider-marked-string.sse | 441 | 11467d0a9f756cd156b3edf32640960bc80f3751bfd8db9f015b79fc17930dac | 665f7504038e6295339743c2652a056faa80ec2b |
| mcp/bridge/provider-no-tools.request.json | 164 | 99d79bdaa2f6b4952e7d88d39df46d045af143c5e5052b50bb8a00b02e9af6e5 | 665f7504038e6295339743c2652a056faa80ec2b |
| mcp/bridge/provider-no-tools.sse | 354 | de8eff9ed567c5f2316d22e454f44eac30e6f258251683d43cc91ede6c2c9767 | 665f7504038e6295339743c2652a056faa80ec2b |
| mcp/bridge/provider-plain-user.request.json | 168 | 7cba4af074b61b85a456bf1d3f962d8b08a8759994406a770b29132eb605ee77 | 665f7504038e6295339743c2652a056faa80ec2b |
| mcp/bridge/provider-plain-user.sse | 354 | de8eff9ed567c5f2316d22e454f44eac30e6f258251683d43cc91ede6c2c9767 | 665f7504038e6295339743c2652a056faa80ec2b |
| mcp/bridge/provider-tool-result.request.json | 173 | da2399d496d4ec3ba00c6130f2ddef452e36fdf2c31ca32766fc24cc8227c473 | 665f7504038e6295339743c2652a056faa80ec2b |
| mcp/bridge/provider-tool-result.sse | 354 | de8eff9ed567c5f2316d22e454f44eac30e6f258251683d43cc91ede6c2c9767 | 665f7504038e6295339743c2652a056faa80ec2b |
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
| remote/remote_bash.schema.json | 607 | f86a1b9c6260041d17352c284536d0d6a514f531ec3617b4d1756545110d04c0 | 4ac9ef8596c347a6b4240da5807c1f24880440e2 |
| remote/remote_file.schema.json | 1029 | 73ae251244fc1d5cc379fe5f63b1bcc20f3a7d92709efa087e6d14931da4c7ad | 4ac9ef8596c347a6b4240da5807c1f24880440e2 |
| sandbox/hello.bin | 18 | 009a43e580d8592a9631260defbfcf412a678b11d341bd0133a4b339068678d2 | 196e70f4605a2098833377482c9457a5440e27a2 |
| sandbox/request_exec_default.bin | 11 | 98e11f2427bc7a173415d4b058e4d1402329936757867005302f6f6d96e7329d | 196e70f4605a2098833377482c9457a5440e27a2 |
| sandbox/request_exec_full.bin | 504 | d08b434c63190ca08fd73965b9bc7af3a4a31b3106a5c86e255d68f4c9e4ac70 | 196e70f4605a2098833377482c9457a5440e27a2 |
| sandbox/request_exec_shell.bin | 41 | 73e6b0a3cbb647cc3e249b4f8a00e092569ab1db835cb941c38ab6f0725c4108 | 196e70f4605a2098833377482c9457a5440e27a2 |
| sandbox/request_shutdown.bin | 5 | a1cb20470d89874f33383802c72d3c27a0668ebffd81934705ab0cfcbf1a1e3a | 196e70f4605a2098833377482c9457a5440e27a2 |
| sandbox/response_error.bin | 56 | 8c45a9a1a00e3044f695305b1931ba3db8fbc7a82cd3edb7e33934b6246f701b | 196e70f4605a2098833377482c9457a5440e27a2 |
| sandbox/response_exit_code.bin | 10 | fd47b41d236b097301df2a0b2544297914008911359f1753450dfe7d30c8cb6d | 196e70f4605a2098833377482c9457a5440e27a2 |
| sandbox/response_exit_signal.bin | 12 | 40a85a24052f23bb40ccde031bb4d65a969896d4e57fc03960667d9ff4d32ca4 | 196e70f4605a2098833377482c9457a5440e27a2 |
| sandbox/response_ready.bin | 6 | a9744c200268f48c0369779fdbf61c300d83c7040a33bf12e173403fc5522821 | 196e70f4605a2098833377482c9457a5440e27a2 |
| sandbox/response_stderr.bin | 309 | 6979ed0eb878d020a32c56e6a57b59048404e13f6512d63bd19a608f57aa50b1 | 196e70f4605a2098833377482c9457a5440e27a2 |
| sandbox/response_stdout.bin | 11 | 1ca3f62a8a31d9403662ef7e8138c251e65fa64bf89700dd99208a3fd3a9375d | 196e70f4605a2098833377482c9457a5440e27a2 |
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
| updater/active-first.json | 44 | 6bc96598883ee756aa8fc7711f256fad23d5b247a03af40e5197a5dafd5a0751 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/active.json | 47 | a267c82671d7b6fae98ee1c3453ffdba1e602e264c3fd94fc17189a3c06fc02c | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/feeds.json | 1733 | 45b7c7d66a8f6f6a41808c164b30af23dd51ad5d8a8918e6e51672b35e2cd652 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/products.json | 2750 | 5f92ba4ef38f9cced015047f1d81c01dbb8c1ab2dc8e186d7478d1b803055ccb | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/root/state.json | 134 | 923793efa634320055d432ec736c7184929731e664ffccf88b4e69dd90030ca9 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/root/versions/app/0.1.0/artifact/app-0.1.0 | 25 | e66f641ebe425650fad4ca0ee020567e0267cf09717018fa23d148badab4a479 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/root/versions/app/0.1.0/artifact/app-0.1.0.sig | 129 | 07ac46953ff9418dd6cb72b2a1a703df2cf6e6d58c6a2daf8740c2ce1574bd09 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/root/versions/app/0.1.0/payload/app-0.1.0 | 25 | e66f641ebe425650fad4ca0ee020567e0267cf09717018fa23d148badab4a479 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/root/versions/app/0.1.0/staged.json | 207 | 0e5cf8db29c1572d2b51626d1271226cbb0e3e2a58a98e5e4345606f00f64698 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/root/versions/app/0.2.0/artifact/app-0.2.0 | 25 | 790b243c054e8e28a79332b7ba1ead9a139e67613ed544ffc62763ff28e8a0ec | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/root/versions/app/0.2.0/artifact/app-0.2.0.sig | 129 | b8fed40618f80a566eac035c2982b65f1de79cdc7fd4e0ea10a241cf0dde0411 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/root/versions/app/0.2.0/payload/app-0.2.0 | 25 | 790b243c054e8e28a79332b7ba1ead9a139e67613ed544ffc62763ff28e8a0ec | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/root/versions/app/0.2.0/staged.json | 207 | 4cb79724793c3409f1fde613683e80ae031c18e86cec9309dc8317c849942589 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/root/versions/app/active.json | 47 | c454c45f888ec086a86e4cc56b638874268bd28d5ac3adb709382c7368886402 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/sig/app-0.2.0 | 25 | 790b243c054e8e28a79332b7ba1ead9a139e67613ed544ffc62763ff28e8a0ec | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/sig/app-0.2.0.sig | 129 | b8fed40618f80a566eac035c2982b65f1de79cdc7fd4e0ea10a241cf0dde0411 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/sig/stranger.hex | 65 | f8718da38842cd9761c86e5fabd38e9dd5c2bd898b8222cf8b6694d6f831aea9 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/sig/trusted.hex | 65 | 52ae2a9b4c641fe431a9ca578223ad018baf6fa3eecd4c8c441b9f68fb7a63cc | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/staged-unsigned.json | 225 | 30325354f54838fc14a949e8ddd1012a1e6a0bbec596b49c3cc45c39ada43fcc | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/staged.json | 251 | 4a69fdb7f9be8b9f55b6624a59279b08b4c6c3df364a2afbbbca3f8cea999f8c | e95ff8064a4e67a6938ed952967f8532da31c997 |
| updater/state.json | 1261 | d342fb0f61b04649fc05e65863f6a389decc606246703366a687de9200abd486 | e95ff8064a4e67a6938ed952967f8532da31c997 |
| util/iso.txt | 280 | 9006e6f63b49bffaee1442bb095e62c95cc5444da3a4f6a59316a13b11fc854b | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| util/ssh/aliases.txt | 43 | 11a0dc406390d101147047702a22186a1ec2c0ee5dc70822a5131f8173a86314 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| util/ssh/config | 100 | db9bb111236f3bf56757b7d396432255fc303eee34c0c481e9f92d506b8ce2a3 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| util/ssh/conf.d/a.conf | 43 | 15921304dd8b16f74da99fd598f9fa52b1289cd0b43541d74765765c26f66ee7 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| util/ssh/conf.d/b.conf | 14 | 39c842d687267318cdc2aa419e73c9eb0735350f2a4d0a2306a6c742f463eeb6 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| util/ssh/conf.d/nested.inc | 18 | 4fdd2b1e906ab64b96697697d38222c5c2e6d0abcf7cd4aedf56f8c50e3995e8 | b128c428ea2976fd62bc1ef28ccc783137388e80 |
| projects/context.md | 718 | e3a3ba097584e3d95d97159967b2671df9ceb81435033b4c6848bafe2a1efe63 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/filenames.tsv | 281 | aba82aaae95d11df7bdf9281f938a28e691dc795f24cbc6fd77012be82dd183e | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/ideas.md | 55 | f85a05e64fcb254f0e1fdbb220ea7e0228e76a00eb601777ee81188e724abc22 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/links.txt | 74 | 45b91df12f5b72ac69e9465bee45f3f0e25f7719e342d3ec0f069e9e3e653023 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/project_read.schema.json | 632 | 327daa5ef014dc1252fa3ca7f4ee7572f259edc33bad27801014fd5c13651679 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/project_search.schema.json | 1557 | 4e1d1f8dd78f5385eeda894814dc0a20a178587ad0a510ef911c94ade30d8913 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/prompt-a.txt | 184 | 2f3d3d29cdbc5bdefb952b224e4785694be9767d4375ab0d4ddac41c84bf8d3f | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/read-a-turn-64-next.txt | 130 | 4ff7922070e55a7d2b108aec2e4e5d0028253288b64adf1f0220ea017fd44750 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/read-a-turn-64.txt | 133 | 82322bb75b6da2222404d57ee9fe56c32a0d3a7d7a7eae46cd14a12cacbec75a | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/read-a.txt | 391 | cb10bcfdf635cb0f965091b31e4f581184ac9f79d2875ce00ba811fa4a7b68e6 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/read-b.txt | 378 | ed3e9722ab6087c407345c5e3bb83a7e7c4525adaf72baadaee2cc25c4462056 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/read-d.txt | 663 | ed67886bd3ff8df7a7e99e0d8a55d20e1d2556aa786254482533221d292215c2 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/search-all-words.json | 195 | a1c047c3ea16f5d9d5e8ea5c962eb67f844c099c64b610a8acb95376add4b29d | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/search-attr.json | 40 | 485ba50421dbe03242822b7e608c7f2491690695fc5df1de755bae2460c663d8 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/search-call.json | 182 | 0d7e55298f71e4619f8b75fb488f352b18b2f4b60073d2fca53dd2655ce8f134 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/search-digest.json | 189 | 3c8ddb2579f8bb0df189d72a42967170786f8eaea65f6b5f286e2b5c666ad256 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/search-iroh.json | 196 | abc6ce742e20ca36cff6b77d8cbc35b5076d081621fa1f5752b5e2dd12e15be3 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/search-messages-page.json | 500 | f9cdf52a35bc8232b4177b90eda82b74ab66d452b9ca839bf2efb1fcb5052ec8 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/search-messages-page2.json | 502 | 91b30973f3a9b8f7e9aade03e3f59f1237f7f10550985f4f55e605fbc0e94fe7 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/search-own.json | 333 | 3f4e5e790d4d6d7d901a4b933389974a45236100a8d0e1df0704998fb2750f63 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/search-tool-attr.json | 178 | 50d9e5cdb5ee89bacb5eccdcd54517969667ce887a6dc04139655a88d35aaef9 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/search-unicode.json | 185 | 15abff138bc5b77f30e4127a250500eac09acbbc7baa7df7165611b88de7bca2 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/settings.default.json | 40 | 5b90f305ddd7b9864af3ebc7012f67ce035fdd9900f0f2ecce2a9c575d964a7d | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
| projects/settings.full.json | 261 | 55a5fe8d1e2aecc0e537e27e40fca6e2fa6c325a8b2b59a03c2b9c2000a6de58 | fba1a170e53a3ce5898201d9cadd91feb6cc97a4 |
