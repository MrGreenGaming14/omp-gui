#pragma once

// The app's `Message` (port of `loa-app/src/app.rs` `Message`) and the session lifecycle's
// own messages (`app/lifecycle.rs`). Variant names match the Rust ones.

#include <cstdint>
#include <expected>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/app/area_msg.hpp"
#include "loa/app/windows.hpp"
#include "loa/app_runtime/lifecycle.hpp"
#include "loa/app_runtime/session.hpp"
#include "loa/ui/panel/diff.hpp"
#include "loa/ui/panel/files.hpp"
#include "loa/ui/session/composer/message.hpp"
#include "loa/ui/session/transcript/state.hpp"
#include "loa/ui/shell/keymap.hpp"
#include "loa/ui/shell/layout.hpp"
#include "loa/ui/shell/palette.hpp"
#include "loa/ui/terminal/state.hpp"
#include "loa/view/shell.hpp"

namespace loa::app {

namespace lifecycle {
namespace msg {
// A session's feed delivered something.
struct Feed {
    app_runtime::SessionId id;
    app_runtime::Feed feed;
};
// A harness call the runtime started returned.
struct Completed {
    app_runtime::Completion completion;
};
} // namespace msg
using Message = std::variant<msg::Feed, msg::Completed>;
} // namespace lifecycle

// The Usage window's messages (`app/usage.rs`); its window and parsing belong to the usage
// module, the app routes them.
namespace usage {
namespace msg {
struct Refresh {};
// `omp usage --json` as read, or why it couldn't be.
struct Loaded {
    std::expected<std::string, std::string> json;
};
struct Toggle {
    std::string provider;
};
struct Spend {
    std::string target;
};
struct Spent {
    std::expected<void, std::string> result;
};
struct Close {};
} // namespace msg
using Message = std::variant<msg::Refresh, msg::Loaded, msg::Toggle, msg::Spend, msg::Spent, msg::Close>;
} // namespace usage

// A pasted image saved to disk, with its size in pixels (`app/media.rs`).
struct Pasted {
    std::filesystem::path path;
    std::uint32_t width = 0;
    std::uint32_t height = 0;
};

namespace msg {
struct Shell {
    ui::shell::layout::Message message;
};
struct Chat {
    ui::session::transcript::Message message;
};
struct Composer {
    ui::session::composer::Message message;
};
struct Files {
    ui::panel::files::Message message;
};
struct Diff {
    ui::panel::diff::Message message;
};
struct Terminal {
    ui::terminal::Message message;
};
struct Palette {
    ui::shell::palette::Message message;
};
struct RunCommand {
    std::string id;
};
// A key press no widget captured.
struct Keyboard {
    ui::shell::palette::KeyPress key;
};
// Redraw while a turn runs.
struct Tick {};
struct Session {
    lifecycle::Message message;
};
// The working tree's changes against HEAD, already parsed off the UI thread; `nullopt`
// outside a git work tree with a commit.
struct WorkingDiff {
    std::optional<std::expected<std::vector<ui::panel::diff::FileDiff>, std::string>> diff;
};
// A message for area `id` (`Message::area`).
struct Area {
    std::string_view id;
    AreaMsg message;
};
struct Usage {
    usage::Message message;
};
struct Window {
    WindowMessage message;
};
// The transcript in a detached session's window.
struct DetachedChat {
    std::string session;
    ui::session::transcript::Message message;
};
// The folder dialog closed with this folder, or none when cancelled.
struct FolderChosen {
    std::expected<std::optional<std::filesystem::path>, std::string> folder;
};
// A recent session picked on the start screen.
struct OpenRecent {
    std::string id;
};
// Sessions read from disk at startup. `empty` lists sessions with no turns.
struct Recents {
    std::vector<loa::view::shell::SessionSummary> summaries;
    std::vector<std::string> problems;
    std::vector<std::string> empty;
};
// The clipboard image a paste asked for, saved to disk, or none.
struct ImagePasted {
    std::expected<std::optional<Pasted>, std::string> pasted;
};
// A file was dropped on a window.
struct FileDropped {
    std::filesystem::path path;
};
} // namespace msg

using Message = std::variant<msg::Shell, msg::Chat, msg::Composer, msg::Files, msg::Diff, msg::Terminal, msg::Palette,
                             msg::RunCommand, msg::Keyboard, msg::Tick, msg::Session, msg::WorkingDiff, msg::Area,
                             msg::Usage, msg::Window, msg::DetachedChat, msg::FolderChosen, msg::OpenRecent,
                             msg::Recents, msg::ImagePasted, msg::FileDropped>;

} // namespace loa::app
