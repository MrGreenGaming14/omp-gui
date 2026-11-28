# Design docs

An all C++ desktop app on top of omp that looks and works like Claude Code, with a p2p network between instances on port 31415.

These files are the source of truth for every issue in this repo. Issue bodies name a tab like "Tai > GUI". The table below maps each tab to its file.

| Tab | File | Covers |
| --- | --- | --- |
| Overview | [overview.md](overview.md) | Architecture map, patterns, engineering practices, tech stack |
| Sessions | [sessions.md](sessions.md) | Session DOM, patch journal, harness support and adapters |
| GUI | [gui.md](gui.md) | Claude Code style layout, decision prompts |
| Tools and MCP panel | [tools-and-mcp.md](tools-and-mcp.md) | Tools panel, MCP servers, remote hosts (SSH) |
| Proposed tools | [proposed-tools.md](proposed-tools.md) | New app tools, app hosted remote MCP server, MCP exposed tools |
| Surfaces | [surfaces.md](surfaces.md) | Universal streamed views, latency budget, built in surfaces |
| Models and providers | [models-and-providers.md](models-and-providers.md) | Sign in, model picker and settings through omp |
| Projects | [projects.md](projects.md) | Session digests, project context, project_search and project_read |
| Networking | [networking.md](networking.md) | iroh on 31415, ALPN split, UDP and TCP |
| Feature ideas | [feature-ideas.md](feature-ideas.md) | P2P network and grants |
| Updater | [updater.md](updater.md) | Keeping the app and omp in sync |

## Decisions already made

- C++23 only. No TypeScript anywhere in this repo
- UI is Qt 6 Widgets
- omp is driven over RPC (`omp --mode rpc`). The app never patches omp or modifies its tools
- Harness agnostic from day one. Every omp feature works on every harness, provided by the app where a harness lacks it
- The app has no model or provider code of its own. It calls omp's mechanisms
- New tools are new tools (`sandbox_task`, `project_search`, `project_read`, and the `remote` MCP server with `remote_bash` and `remote_file`), never changes to omp's tools
- Surface tools (`surface_*`) live only in surface MCP servers

## Open questions

- [ ] Does Chrome's screencast fit the 50 ms budget, or do we need native capture
- [ ] Is QtPdf good enough on our real PDFs, or do we keep PDFium as a fallback
- [ ] Support MCP Apps (HTML in a webview) for compatibility, or pixel surfaces only
- [ ] Does omp's RPC `btw` follow the session model or can it use the `smol` role
- [ ] Can omp settings be changed over RPC, or does the settings screen edit config files
