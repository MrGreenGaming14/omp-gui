# OMP GUI

Loa is a desktop workspace for OMP and other coding-agent harnesses. It brings project chats, files, terminals, and agent tools into one app.

The app is in development and does not have a packaged release. The [design artifact](https://claude.ai/artifact/4nPNTWD7iG2bp6yuHuKYkL#f6baca39-b36f) and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) describe the design and crate layout.

## Run from source on Windows

These instructions use native Windows x64 and PowerShell; WSL is not required.

### 1. Install prerequisites

- [Git for Windows](https://git-scm.com/downloads/win). Keep Git available on `PATH`; its Bash is also used by the test fixtures.
- [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/). Select **Desktop development with C++**, including the MSVC x64/x86 build tools and a Windows SDK.
- [Rust via rustup](https://rustup.rs/). Use the stable MSVC toolchain, not the GNU toolchain.

Open a new PowerShell window after installation so it picks up the updated `PATH`, then select the toolchain and check the commands:

```powershell
rustup default stable-x86_64-pc-windows-msvc
rustc --version
cargo --version
git --version
```

Install [OMP](https://github.com/can1357/oh-my-pi#install) using its official Windows installer:

```powershell
irm https://omp.sh/install.ps1 | iex
```

Open a new PowerShell window if the installer updated `PATH`, then run `omp --version`. Run `omp` once to configure your model provider or sign in before starting an OMP chat in Loa.

### 2. Clone, build, and launch

```powershell
git clone https://github.com/MrGreenGaming14/omp-gui.git
cd omp-gui
cargo build --workspace --all-targets
cargo run -p loa-app
```

Run the Cargo commands from the repository root. The workspace build also creates `loa-sandbox-stub.exe`, which the app needs for local sandbox tasks; `cargo run -p loa-app` alone does not build that companion executable. With Cargo's default target directory, the app and stub are in `target\debug`. Keep them together if you move the binaries.

To launch an already-built app without Cargo:

```powershell
.\target\debug\loa-app.exe
```

### 3. Check the desktop

Open a project, start an OMP chat, run `cd` in the Terminal tab, and use the Files tab to open a project file. Browser virtual-display mode requires Linux; Windows can use headed Chrome or Chromium.

### Optional: run the tests

Install [Python 3](https://www.python.org/downloads/windows/) with `python.exe` on `PATH`, then run these commands from the repository root. Put Git for Windows' Bash ahead of the WSL launcher for the test fixtures, and use the software test backend as Windows CI does:

```powershell
$git = (Get-Command git.exe).Source
$env:Path = "$(Split-Path (Split-Path $git))\bin;$env:Path"
$env:ICED_TEST_BACKEND = 'tiny-skia'
cargo test --workspace --no-fail-fast
```

`ICED_TEST_BACKEND` selects the test backend; it is not required to launch the desktop app.

### Troubleshooting

- **`cargo`, `git`, or `omp` is not recognized:** reopen PowerShell after installation and check that the corresponding executable is on `PATH`.
- **`link.exe` or a Windows SDK library is missing:** install the C++ workload and Windows SDK in Visual Studio Build Tools, confirm `rustup show active-toolchain` reports `x86_64-pc-windows-msvc`, and retry from a new PowerShell window.
- **Local sandbox reports that `loa-sandbox-stub` was not found:** rerun `cargo build --workspace --all-targets` and keep the stub alongside the app.
- **Tests cannot find Bash or Python:** check `Get-Command bash.exe` resolves to Git for Windows rather than WSL, and that `python.exe --version` works.

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
