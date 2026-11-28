#pragma once

// The terminal tab: sub tabs, each a terminal on a PTY its `PtyBackend` opened. Plain C++, no Qt.
// Port of `crates/loa-ui-terminal/src/{state.rs,lib.rs}`. The work an update asks for comes back as a
// `Task`: clipboard operations the view runs, and PTY operations `Runner` drives on the executor.

#include <compare>
#include <cstdint>
#include <deque>
#include <expected>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/ui/terminal/canvas.hpp"
#include "loa/ui/terminal/emulator.hpp"
#include "loa/ui/terminal/local.hpp"
#include "loa/view/terminal.hpp"

namespace loa::ui::terminal {

namespace port = loa::view::terminal::port;
using port::ChannelRead;
using port::PtyChannel;

struct TabId {
    std::uint64_t value = 0;
    friend auto operator<=>(const TabId&, const TabId&) = default;
};

// The login shell on a host the SSH layer knows.
struct Remote {
    std::string host;
    friend bool operator==(const Remote&, const Remote&) = default;
};
// Where a terminal runs: a local program, or a remote host.
using Target = std::variant<LocalShell, Remote>;
// The directory's name (or the whole path) for local targets, the host for remote ones.
[[nodiscard]] std::string target_title(const Target& target);

// `Failed`: opening or driving the PTY failed; the screen stays for reading.
struct Status {
    enum class Kind : std::uint8_t { Connecting, Running, Failed };
    Kind kind = Kind::Connecting;
    std::string error;

    [[nodiscard]] static Status connecting() { return {}; }
    [[nodiscard]] static Status running() { return {Kind::Running, {}}; }
    [[nodiscard]] static Status failed(std::string e) { return {Kind::Failed, std::move(e)}; }
    friend bool operator==(const Status&, const Status&) = default;
};

// Port: opens a PTY for a target. The terminal state holds one and never matches on targets.
class PtyBackend {
  public:
    virtual ~PtyBackend() = default;
    [[nodiscard]] virtual loa::contracts::Awaitable<port::Result<PtyChannel>> open(Target target, GridSize size) const = 0;
};

// Local programs on a PTY; remote targets fail with `Error::no_remote`.
class LocalPtyBackend final : public PtyBackend {
  public:
    [[nodiscard]] loa::contracts::Awaitable<port::Result<PtyChannel>> open(Target target, GridSize size) const override;
};

// Remote targets through a `RemotePty` (the app's SSH layer); everything else goes to `fallback`.
class RemotePtyBackend final : public PtyBackend {
  public:
    RemotePtyBackend(std::shared_ptr<const port::RemotePty> remote, std::shared_ptr<const PtyBackend> fallback)
        : remote_(std::move(remote)), fallback_(std::move(fallback)) {}
    [[nodiscard]] loa::contracts::Awaitable<port::Result<PtyChannel>> open(Target target, GridSize size) const override;

  private:
    std::shared_ptr<const port::RemotePty> remote_;
    std::shared_ptr<const PtyBackend> fallback_;
};

// What a tab sends its program: bytes, or a new window size.
namespace inputs {
struct Bytes {
    terminal::Bytes bytes;
    friend bool operator==(const Bytes&, const Bytes&) = default;
};
struct Resize {
    GridSize size;
    friend bool operator==(const Resize&, const Resize&) = default;
};
} // namespace inputs
using Input = std::variant<inputs::Bytes, inputs::Resize>;

// A tab's input, queued until the write loop takes it. Closing it (the tab dropped) ends the write loop, which
// hangs up. Thread-safe: the GUI thread pushes, the executor pops.
class InputQueue {
  public:
    void push(Input item);
    [[nodiscard]] std::optional<Input> try_pop();
    void close();
    [[nodiscard]] bool closed() const;
    // Called (on the pushing thread) after each push and on close.
    void set_waker(std::function<void()> waker);

  private:
    mutable std::mutex mutex_;
    std::deque<Input> items_;
    bool closed_ = false;
    std::function<void()> waker_;
};

// The app sends `NewTab`, `Select` and `Close`; the rest come from the tab's widgets and tasks.
namespace msg {
struct NewTab {};
struct Select {
    TabId tab;
};
struct Close {
    TabId tab;
};
struct Canvas {
    TabId tab;
    CanvasEvent event;
};
struct Paste {
    TabId tab;
    std::optional<std::string> text;
};
// The opened channel, moved out of the shared message exactly once.
struct Opened {
    TabId tab;
    std::shared_ptr<PtyChannel> channel;
};
// Program output, or why opening, reading or writing failed; the write side reports `Closed` once its tab is gone.
struct Output {
    TabId tab;
    std::expected<ChannelRead, std::string> read;
};
} // namespace msg
using Message = std::variant<msg::NewTab, msg::Select, msg::Close, msg::Canvas, msg::Paste, msg::Opened, msg::Output>;

// Work an update asks for (iced `Task`).
namespace op {
// Open a PTY for `target` through the state's backend; reports `Opened` or `Output` with the error.
struct Open {
    TabId tab;
    Target target;
    GridSize size;
};
// Read `channel` until the program ends and forward `input` to it until the queue closes.
struct Drive {
    TabId tab;
    std::shared_ptr<PtyChannel> channel;
    std::shared_ptr<InputQueue> input;
};
// Write the clipboard (or the primary selection).
struct WriteClipboard {
    std::string text;
    bool primary = false;
};
// Read the clipboard and send it back as `Paste`.
struct ReadClipboard {
    TabId tab;
};
} // namespace op
using Op = std::variant<op::Open, op::Drive, op::WriteClipboard, op::ReadClipboard>;

struct Task {
    std::vector<Op> ops;
    [[nodiscard]] static Task none() { return {}; }
    [[nodiscard]] bool empty() const { return ops.empty(); }
    void append(Task other) {
        for (auto& o : other.ops) ops.push_back(std::move(o));
    }
};

class State {
  public:
    // An empty tab on local PTYs; `new_tab` is what the + button opens.
    State(Metrics metrics, LocalShell new_tab);
    ~State();
    State(State&&) noexcept;
    State& operator=(State&&) noexcept;

    // Opens remote targets through `remote`, everything else as before.
    void with_remote(std::shared_ptr<const port::RemotePty> remote);
    [[nodiscard]] const std::shared_ptr<const PtyBackend>& backend() const noexcept { return backend_; }
    void set_new_tab(LocalShell shell) { new_tab_ = std::move(shell); }
    [[nodiscard]] const LocalShell& new_tab() const noexcept { return new_tab_; }
    [[nodiscard]] const Metrics& metrics() const noexcept { return metrics_; }
    void set_metrics(Metrics m) { metrics_ = m; }

    // Opens a terminal on `target` in a new sub tab and shows it.
    std::pair<TabId, Task> open(Target target);
    [[nodiscard]] std::vector<TabId> tabs() const;
    [[nodiscard]] std::optional<TabId> active() const noexcept { return active_; }
    [[nodiscard]] bool is_empty() const noexcept { return tabs_.empty(); }
    // The program's title if it set one, else the directory or host.
    [[nodiscard]] std::optional<std::string_view> title(TabId id) const;
    [[nodiscard]] const Status* status(TabId id) const;
    [[nodiscard]] const Target* target(TabId id) const;
    [[nodiscard]] const Emulator* emulator(TabId id) const;
    // The tab's input queue; input stays here until its channel opens.
    [[nodiscard]] std::shared_ptr<InputQueue> input(TabId id) const;

    Task update(Message message);

  private:
    struct Tab;
    [[nodiscard]] Tab* tab(TabId id) const;
    void show(TabId id);

    std::vector<std::unique_ptr<Tab>> tabs_;
    std::optional<TabId> active_;
    std::uint64_t next_id_ = 1;
    Metrics metrics_;
    GridSize grid_;
    LocalShell new_tab_;
    std::shared_ptr<const PtyBackend> backend_;
};

} // namespace loa::ui::terminal
