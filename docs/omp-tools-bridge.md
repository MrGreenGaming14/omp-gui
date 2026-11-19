# omp tools bridge

How the app offers omp's own tools (hashline `read`/`edit`, `lsp`, `debug`, `ast_grep`, `ast_edit`) to harnesses that are not omp, as an MCP server. Code: `crates/loa-omp-bridge`. Checked against omp 19.0.0.

## Can omp's tools run outside an omp agent loop? (#104)

| Option | Evidence | Verdict |
| --- | --- | --- |
| omp as an MCP server | `omp --help` lists no MCP server command. `omp://mcp-config.md`, `mcp-protocol-transports.md` and `mcp-server-tool-authoring.md` cover omp as an MCP client only. `omp acp` is an ACP agent server: it takes prompts, not tool calls | Does not exist |
| RPC command that runs a tool | `omp://rpc.md` "Command Schema" lists every `RpcCommand`. The only one that executes anything without a turn is `bash` (shell command, no tool dispatch). `set_host_tools` goes the other way (omp calls the host) | Does not exist |
| `xd://` tool devices through RPC `bash` | `write xd://<tool>` dispatches a tool, and the embedded shell routes redirects to `xd://` handlers (`omp://tools/bash.md`). Tried: RPC `{"type":"bash","command":"cat xd://edit"}` answers `no filesystem provider for xd://`. The RPC `bash` command runs outside the agent's tool context | Does not work |
| CLI subcommands | `omp read PATH` prints what the read tool returns and `omp grep` tests the grep tool. There is no `omp edit`, `omp lsp`, `omp debug` or `omp ast-*`. One process per call would also lose LSP and DAP state | Read only, not enough |
| SDK (`createAgentSession`, `createTools`, tool classes) | `omp://sdk.md` "Entry points": the package exports `createTools`, `BUILTIN_TOOLS` and tool classes, so a Bun script could call `tool.execute()` directly | Works, but needs Bun and TypeScript. The repo is Rust only and the SDK is not installed here |
| Native addon (`@oh-my-pi/pi-natives`) | `omp://natives-architecture.md`: a Node-API addon with AST matching/editing, grep, edit sessions | N-API is callable only from a JS runtime. It has no hashline tag checks, LSP or DAP (those live in TypeScript) |
| RPC agent turn driven by a scripted loopback model | omp accepts any OpenAI compatible provider from `models.yml` (`omp://models.md`). A loopback endpoint that answers the prompt with exactly one tool call makes omp execute its real tool and emit the result as `tool_execution_end`. Tried by hand, then automated in the tests below: `read` returned `[a.txt#B6C1]` with hashline rows, `edit` with that tag changed the file on disk, `ast_grep` ran. The only HTTP requests omp made went to the loopback endpoint | **Works today. Chosen** |

`get_state` returns `dumpTools`: every active tool's name, description and JSON Schema (`parameters`). The bridge serves those schemas as they are, so nothing is retyped and omp updates flow through.

### What works

- Real omp tool code, the same hashline tags, LSP servers, debugger sessions and AST engine an omp agent gets. Tags survive across calls (one omp process) and across `new_session`.
- No model call. omp's model is `loa-omp-bridge/scripted` at `http://127.0.0.1:<random port>/v1`, guarded by a random bearer token. omp's startup probes (seven small completions) and any compaction reach only that endpoint. `--offline` keeps auxiliary services on the active provider.
- The user's `~/.omp/agent` is untouched: the bridge points `PI_CODING_AGENT_DIR` at its own state directory and runs with `--no-session`.
- Cancellation: an MCP `notifications/cancelled` aborts the turn (RPC `abort`) and the next call gets its own result.

### What does not

- One call at a time. omp runs one turn per process, so calls are serialized. Concurrent calls from one harness queue behind each other.
- Each call costs one RPC turn (two loopback completions) and the transcript grows with every result. The bridge starts a fresh omp session after 8 MiB of tool output; tags still resolve afterwards.
- Because the agent directory is the bridge's own, the user's `config.yml` (LSP servers, debugger settings) applies only when passed as a `--config` overlay (`OmpBridgeConfig::config_overlays`).
- `debug` with `launch` and `lsp` need the debug adapters and language servers installed, same as in omp.
- `ast_grep` is off by default in omp. The bridge's overlay enables it and turns `tools.xdev` off so every tool is top level in `dumpTools`.

## The bridge (#105)

```text
harness ──MCP stdio──▶ loa-omp-bridge ──RPC JSONL──▶ omp --mode rpc ──HTTP──▶ loopback "model" (in the bridge)
```

1. Start: write `models.yml` registering the loopback provider and a settings overlay, spawn `omp --mode rpc --no-session --no-ui --no-extensions --no-skills --no-rules --no-context-files --no-title --offline --auto-approve --model loa-omp-bridge/scripted --tools <list>`, wait for `ready`, negotiate protocol v2 (chunked frames are reassembled), read `get_state.dumpTools`.
2. `tools/list`: the configured tools with omp's schemas.
3. `tools/call`: send `prompt` with a marker carrying the call id, tool name and arguments. The loopback endpoint sees the marker in the newest user message and streams back that one tool call. omp executes it and emits `tool_execution_end`. The endpoint answers the follow-up request with a short text, the turn ends with `prompt_result`, and the bridge returns the tool's content blocks and `isError`.

Entry points: `OmpExecutor::start(OmpBridgeConfig)`, `serve_stdio(executor)`, or the `loa-omp-bridge` binary (`--state-dir DIR [--cwd DIR] [--omp PATH] [--tools a,b] [--config FILE]`). `ToolExecutor` is the port the MCP server uses, so the server shell is tested with an in-memory executor.

Default tools: `read`, `edit`, `ast_grep`, `ast_edit`, `lsp`, `debug`. `read` is included because `edit` needs the tags it returns.

Tests (`cargo test -p loa-omp-bridge`): `harness_reads_edits_and_searches_through_omp` spawns the binary as an MCP client would and, against the installed omp in a temp directory, reads a file, applies a hashline edit, checks a stale tag is refused, and runs `ast_grep`. `a_cancelled_call_leaves_the_next_call_correct` covers cancellation.

## If omp grows a direct path

Should omp add an RPC command that runs a tool, or an MCP server mode, only `OmpExecutor::run_tool` changes. The MCP side and the port stay as they are.
