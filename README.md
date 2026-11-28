# OMP GUI

Loa is a desktop workspace for OMP and other coding-agent harnesses. It brings project chats, files, terminals, and agent tools into one app.

The app is in development and does not have a packaged release. The [design artifact](https://claude.ai/artifact/4nPNTWD7iG2bp6yuHuKYkL#f6baca39-b36f) and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) describe the design and the code layout under `cpp/` (`cpp/libs`, `cpp/ui`, `cpp/app`).

## Run from source on Linux

The commands below use Bash on Ubuntu 24.04, matching the Linux CI environment. Run the desktop app from a graphical X11 or Wayland session, not a headless SSH session.

### 1. Install prerequisites

```bash
sudo apt-get update
sudo apt-get install -y build-essential cmake ninja-build ccache qt6-base-dev qt6-pdf-dev qt6-svg-dev qt6-tools-dev qt6-image-formats-plugins libsqlite3-dev libssl-dev libcurl4-openssl-dev libssh-dev libvterm-dev libarchive-dev libsecret-1-dev libjpeg-turbo8-dev libpng-dev libwebp-dev libgif-dev zlib1g-dev libopenh264-dev libtomlplusplus-dev doctest-dev libasio-dev libpugixml-dev libxlsxwriter-dev libcpp-httplib-dev libstb-dev libnotify-dev libxcb1-dev libxcb-shm0-dev libxcb-xtest0-dev libmd4c-dev libyaml-cpp-dev libminizip-dev
sudo apt-get install -y curl git
```

The peer network and grants link two vendored C libraries, `iroh-c-ffi` and `biscuit-capi` (`cpp/libs/iroh_ffi/fork`, `cpp/libs/biscuit_ffi/fork`), which are written in Rust. CMake prebuilds them with `cargo` once, into `${LOA_RUST_FFI_TARGET:-$HOME/.cache/loa/rust-ffi-target}`, so install a stable toolchain with [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
. "$HOME/.cargo/env"
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
cmake -S cpp -B build -G Ninja
cmake --build build
./build/app/loa-qt
```

`cmake --build build` builds every library, the `loa-qt` app and all test executables. To build only the app, run `cmake --build build --target loa-qt`; it also builds `loa-sandbox-stub`, which the app needs for local sandbox tasks, into `build/app/` beside `loa-qt`. Keep the two executables together if you move them, or point `LOA_SANDBOX_STUB` at the stub.

Set `LOA_LOG` (for example `LOA_LOG=debug`) to change the log level written to stderr.

### 3. Check the desktop

Open a project, start an OMP chat, run `pwd` in the Terminal tab, and use the Files tab to open a project file.

### Optional: run the tests

```bash
ctest --test-dir build --output-on-failure
QT_QPA_PLATFORM=offscreen timeout 5 ./build/app/loa-qt --smoke shell
```

`ctest` runs every `loa_*_tests` executable, the presentation API check (`loa_view_api`), the theme lint (`loa_ui_lint_literals`) and one offscreen smoke run per screen (`loa_qt_smoke_<screen>`). While working on one library, build and run only its tests, for example `cmake --build build --target loa_bus_tests && ./build/libs/bus/loa_bus_tests`.

### Troubleshooting

- **CMake cannot find a package (Qt6, OpenSSL, libssh, …):** install the prerequisite packages above.
- **`cargo` is not found during the build:** run `. "$HOME/.cargo/env"` or open a new terminal after installing rustup, then rerun `cmake --build build`.
- **`omp` is not found:** follow the OMP installer's instructions to add its installation directory to `PATH`, then reopen the terminal.
- **No window appears:** launch from your local X11/Wayland desktop. `QT_QPA_PLATFORM=offscreen` is for automated tests, not an interactive desktop.
- **Local sandbox reports that `loa-sandbox-stub` was not found:** run `cmake --build build --target loa-sandbox-stub` and keep it beside `loa-qt`, or set `LOA_SANDBOX_STUB`.

## macOS and Windows

Platform code is ported behind `#if` blocks, but only Linux is verified. CI builds macOS and Windows from `cpp/vcpkg.json` with Qt 6.4 from the Qt installer; those jobs do not gate merges:

```bash
cmake -S cpp -B build -G Ninja -DCMAKE_TOOLCHAIN_FILE="$VCPKG_ROOT/scripts/buildsystems/vcpkg.cmake"
cmake --build build
```

## Experience and roadmap

- **Projects and chat.** Pin projects in a left sidebar. Selecting one switches the working folder and opens its chats, with project files accessible from the same dashboard. Tabs for open projects are an optional addition. A Claude Code–style layout puts streaming chat and inline approvals in the center, with files, diffs, terminals, and running subagents in a side panel.
- **Multiple harnesses.** Use OMP, omp², Claude Code, Codex, OpenCode, or another ACP-compatible harness through adapters. The app can preserve a common session history across harnesses and let chats search relevant context from other sessions in the same project.
- **Live tools and viewers.** Watch an agent use a browser and interact with the same view. Use the same interface for other viewers, such as PDFs and Office files. A Tools panel manages built-in tools and MCP servers; proposed remote tools cover SSH hosts and isolated workers.
- **Cross-device work.** Connect app instances directly to access or share projects, sessions, files, or live views. Permissions are scoped to the peer and resource rather than granting access to the whole workspace.
- **Custom agents and workflows.** Build an agent from template fields and permission checkboxes, then save it as a Markdown definition. Define named workflows that allow or disallow specific agents and switch workflows next to the prompt. The proposal stores workflow names in `config.yml` and routing rules in `RULES.md`.

## Architecture

- **Desktop:** C++23 with Qt 6 Widgets for the GUI and libvterm for terminal panes.
- **Core:** One app-owned session format and one app-owned configuration format. Adapters translate harness events into an append-only session journal, with snapshots for faster replay. Model and provider setup reuses OMP’s existing configuration and sign-in flows.
- **Networking:** An iroh peer endpoint on port `31415`, with separate channels for control, sessions, and live views. Peer identities use public keys; resource grants govern what each peer can access.
- **Updates:** Stage app and harness updates, check compatibility before activating them, and retain the prior version for rollback.
