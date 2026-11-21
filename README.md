# OMP GUI

Loa is a desktop workspace for OMP and other coding-agent harnesses. It brings project chats, files, terminals, and agent tools into one app.

The app is in development and does not have a packaged release. The [design artifact](https://claude.ai/artifact/4nPNTWD7iG2bp6yuHuKYkL#f6baca39-b36f) and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) describe the design and crate layout.

## Run from source on Windows

Install stable Rust with the MSVC toolchain, Visual Studio Build Tools with the C++ workload and Windows SDK, and Git for Windows. Install [OMP](https://github.com/can1357/oh-my-pi/releases) and make sure `omp --version` works in PowerShell.

```powershell
cargo build --workspace --all-targets
cargo run -p loa-app
```

The workspace build also places `loa-sandbox-stub.exe` alongside the app. To test on Windows, install Python 3 (`python.exe` on `PATH`) and put Git for Windows' Bash ahead of the WSL launcher:

```powershell
$git = (Get-Command git.exe).Source
$env:Path = "$(Split-Path (Split-Path $git))\bin;$env:Path"
$env:ICED_TEST_BACKEND = 'tiny-skia'
cargo test --workspace --no-fail-fast
```

For a desktop smoke check, open a project, start an OMP chat, run `cd` in the Terminal tab, and use the Files tab to open a project file. Browser virtual-display mode requires Linux; Windows can use headed Chrome or Chromium.

## Experience and roadmap

- **Projects and chat.** Pin projects in a left sidebar. Selecting one switches the working folder and opens its chats, with project files accessible from the same dashboard. Tabs for open projects are an optional addition. A Claude Code–style layout puts streaming chat and inline approvals in the center, with files, diffs, terminals, and running subagents in a side panel.
- **Multiple harnesses.** Use OMP, omp², Claude Code, Codex, OpenCode, or another ACP-compatible harness through adapters. The app can preserve a common session history across harnesses and let chats search relevant context from other sessions in the same project.
- **Live tools and viewers.** Watch an agent use a browser and interact with the same view. Use the same interface for other viewers, such as PDFs and Office files. A Tools panel manages built-in tools and MCP servers; proposed remote tools cover SSH hosts and isolated workers.
- **Cross-device work.** Connect app instances directly to access or share projects, sessions, files, or live views. Permissions are scoped to the peer and resource rather than granting access to the whole workspace.
- **Custom agents and workflows.** Build an agent from template fields and permission checkboxes, then save it as a Markdown definition. Define named workflows that allow or disallow specific agents and switch workflows next to the prompt. The proposal stores workflow names in `config.yml` and routing rules in `RULES.md`.

## Proposed architecture

- **Desktop:** Rust with Iced 0.14 for the GUI and `alacritty_terminal` for terminal panes.
- **Core:** One app-owned session format and one app-owned configuration format. Adapters translate harness events into an append-only session journal, with snapshots for faster replay. Model and provider setup reuses OMP’s existing configuration and sign-in flows.
- **Networking:** An iroh peer endpoint on port `31415`, with separate channels for control, sessions, and live views. Peer identities use public keys; resource grants govern what each peer can access.
- **Updates:** Stage app and harness updates, check compatibility before activating them, and retain the prior version for rollback.
