#pragma once

// What an app `update` asks for (iced `Task<Message>`): messages to run next, coroutines to run
// on the `loa::runtime` executor that emit messages back to the GUI thread, and the widget and
// window operations of the UI components it called. Plain data, so state tests can assert what
// an update asked for; the Qt host (`AppHost`) runs it.

#include <functional>
#include <optional>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/app/message.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/ui/session/composer/message.hpp"
#include "loa/ui/session/transcript/state.hpp"
#include "loa/ui/shell/task.hpp"
#include "loa/ui/terminal/state.hpp"

namespace loa::app {

// Operations on the widgets and windows the host draws.
namespace ui_op {
// A shell-style widget operation of one component.
struct Pane {
    enum class Target : std::uint8_t { Shell, Palette, Files, Diff };
    Target target = Target::Shell;
    ui::shell::Op op;
};
struct Composer {
    ui::session::composer::Task task;
};
// The active transcript, or a detached one by key.
struct Chat {
    std::optional<std::string> detached;
    ui::session::transcript::Task task;
};
struct Terminal {
    ui::terminal::Task task;
};
struct OpenWindow {
    WindowId id = 0;
    float width = 0;
    float height = 0;
};
struct FocusWindow {
    WindowId id = 0;
};
struct CloseWindow {
    WindowId id = 0;
};
// Quit the app.
struct Exit {};
} // namespace ui_op
using UiOp = std::variant<ui_op::Pane, ui_op::Composer, ui_op::Chat, ui_op::Terminal, ui_op::OpenWindow,
                          ui_op::FocusWindow, ui_op::CloseWindow, ui_op::Exit>;

class Task {
  public:
    // Hands a message back to the app; thread-safe in the host.
    using Emit = std::function<void(Message)>;
    // Work spawned on the executor, which hands its messages to `Emit` (`perform` in
    // `loa/app/perform.hpp`). It spawns its coroutine itself, so this header needs no Asio.
    using Job = std::move_only_function<void(Emit, loa::runtime::Executor&)>;

    std::vector<Message> messages;
    std::vector<Job> jobs;
    std::vector<UiOp> ops;

    Task() = default;
    Task(Task&&) noexcept = default;
    Task& operator=(Task&&) noexcept = default;
    Task(const Task&) = delete;
    Task& operator=(const Task&) = delete;

    [[nodiscard]] static Task none() { return {}; }
    // Runs `message` through `update` next.
    [[nodiscard]] static Task done(Message message) {
        Task t;
        t.messages.push_back(std::move(message));
        return t;
    }
    [[nodiscard]] static Task op(UiOp op) {
        Task t;
        t.ops.push_back(std::move(op));
        return t;
    }
    [[nodiscard]] static Task job(Job job) {
        Task t;
        t.jobs.push_back(std::move(job));
        return t;
    }
    [[nodiscard]] static Task batch(std::vector<Task> tasks) {
        Task t;
        for (auto& each : tasks) t.append(std::move(each));
        return t;
    }
    // A shell-style component task as widget operations.
    [[nodiscard]] static Task pane(ui_op::Pane::Target target, const ui::shell::Task& task) {
        Task t;
        for (const auto& o : task.ops) t.ops.emplace_back(ui_op::Pane{target, o});
        return t;
    }
    [[nodiscard]] static Task composer(ui::session::composer::Task task) {
        return task.empty() ? none() : op(ui_op::Composer{task});
    }
    [[nodiscard]] static Task chat(ui::session::transcript::Task task, std::optional<std::string> detached = {}) {
        return task.empty() ? none() : op(ui_op::Chat{std::move(detached), std::move(task)});
    }
    [[nodiscard]] static Task terminal(ui::terminal::Task task) {
        return task.empty() ? none() : op(ui_op::Terminal{std::move(task)});
    }

    void append(Task other) {
        for (auto& m : other.messages) messages.push_back(std::move(m));
        for (auto& j : other.jobs) jobs.push_back(std::move(j));
        for (auto& o : other.ops) ops.push_back(std::move(o));
    }
    [[nodiscard]] bool empty() const noexcept { return messages.empty() && jobs.empty() && ops.empty(); }
};

} // namespace loa::app
