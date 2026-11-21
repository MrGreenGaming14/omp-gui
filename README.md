# OMP GUI

Loa is a desktop workspace for OMP and other coding-agent harnesses. It brings project chats, files, terminals, and agent tools into one app.

The app is in development and does not have a packaged release. The [design artifact](https://claude.ai/artifact/4nPNTWD7iG2bp6yuHuKYkL#f6baca39-b36f) and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) describe the design and crate layout.

## Run from source on Linux

The commands below use Bash on Ubuntu 24.04, matching the Linux CI environment. On other distributions, install the equivalent compiler, build tools, and graphics libraries with your package manager. Run the desktop app from a graphical X11 or Wayland session, not a headless SSH session.

### 1. Install prerequisites

```bash
sudo apt-get update
sudo apt-get install -y build-essential cmake pkg-config curl git python3 \
  libxkbcommon-dev libwayland-dev libvulkan1 mesa-vulkan-drivers
```

Install [stable Rust with rustup](https://rustup.rs/), then load Cargo into your current shell:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
. "$HOME/.cargo/env"
rustc --version
cargo --version
```

Install [OMP](https://github.com/can1357/oh-my-pi#install) using its official Linux installer:

```bash
curl -fsSL https://omp.sh/install | sh
```

Follow the installer's `PATH` instructions or open a new terminal, then check `omp --version`. Run `omp` once to configure your model provider or sign in before starting an OMP chat in Loa.

### 2. Clone, build, and launch

```bash
git clone https://github.com/MrGreenGaming14/omp-gui.git
cd omp-gui
cargo build --workspace --all-targets
cargo run -p loa-app
```

Run the Cargo commands from the repository root. The workspace build also creates `loa-sandbox-stub`, which the app needs for local sandbox tasks; `cargo run -p loa-app` alone does not build that companion executable. With Cargo's default target directory, the app and stub are in `target/debug`. Keep them together if you move the binaries.

To launch an already-built app without Cargo:

```bash
./target/debug/loa-app
```

### 3. Check the desktop

Open a project, start an OMP chat, run `pwd` in the Terminal tab, and use the Files tab to open a project file.

### Optional: run the tests

From the repository root, use Xvfb to provide the display required by GUI tests, as Linux CI does:

```bash
sudo apt-get install -y xvfb
xvfb-run -a cargo test --workspace
```

### Troubleshooting

- **`cargo` is not found:** run `. "$HOME/.cargo/env"` or open a new terminal after installing Rust.
- **`omp` is not found:** follow the OMP installer's instructions to add its installation directory to `PATH`, then reopen the terminal.
- **Build errors mention a compiler, CMake, `pkg-config`, Wayland, or xkbcommon:** install the prerequisite packages above (or your distribution's equivalents).
- **No window or no graphics adapter:** launch from your local X11/Wayland desktop and check that your GPU driver or Mesa Vulkan driver is installed. Xvfb is for automated tests, not an interactive desktop.
- **Local sandbox reports that `loa-sandbox-stub` was not found:** rerun `cargo build --workspace --all-targets` and keep the stub alongside the app.

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
