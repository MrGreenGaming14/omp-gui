#pragma once

// Local programs on a PTY as a `PtyChannel` (forkpty; ConPTY behind `_WIN32`). The only place the UI spawns a
// process. Port of `crates/loa-ui-terminal/src/local.rs`; the source lives in `cpp/ui/terminal/local/`.

#include <filesystem>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "loa/view/terminal.hpp"

namespace asio {
class any_io_executor;
} // namespace asio

namespace loa::ui::terminal {

// `program` with `args` (the user's login shell when unset) in `cwd`, with `env` added to the app's environment.
struct LocalShell {
    std::optional<std::filesystem::path> program;
    std::vector<std::string> args;
    std::filesystem::path cwd;
    std::vector<std::pair<std::string, std::string>> env;

    [[nodiscard]] static LocalShell login_shell(std::filesystem::path cwd) { return {std::nullopt, {}, std::move(cwd), {}}; }
    friend bool operator==(const LocalShell&, const LocalShell&) = default;
};

// Starts `shell` on a new PTY whose channel runs on `executor`. Output is read in chunks of up to 64 KiB;
// after it ends the child's exit status is reported. Closing or destroying the writer hangs up (SIGHUP).
[[nodiscard]] loa::view::terminal::port::Result<loa::view::terminal::port::PtyChannel>
spawn(const LocalShell& shell, loa::view::terminal::port::GridSize size, const asio::any_io_executor& executor);

} // namespace loa::ui::terminal
