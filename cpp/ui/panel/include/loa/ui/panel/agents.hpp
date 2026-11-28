#pragma once

// The agents and background jobs panes: one session's `<job>`s, filtered by kind; a pending or
// running `<call tool="wait">` awaits every running job. Port of
// `crates/loa-ui-panel/src/agents.rs` (state half). Plain C++.

#include <array>
#include <cstddef>
#include <cstdint>
#include <map>
#include <optional>
#include <set>
#include <string>
#include <string_view>
#include <utility>
#include <variant>

#include "loa/core/dom.hpp"
#include "loa/ui/shell/task.hpp"
#include "loa/view/panel.hpp"

namespace loa::ui::panel::agents {

using Task = loa::ui::shell::Task;
using loa::core::JobKind;
using loa::core::JobStatus;
using loa::core::NodeId;

// Output lines a job shows while not in full; longer output shows only its tail.
inline constexpr std::size_t TAIL_LINES = 200;
inline constexpr std::string_view WAIT_TOOL = "wait";

// Which `<job>` kind a shared pane displays.
enum class PaneKind : std::uint8_t { Agents, Jobs };

// One `<job>` as the panel shows it.
class Job {
  public:
    // A job of a known kind, else none.
    [[nodiscard]] static std::optional<Job> from_node(const loa::core::Node& node);
    // Copies every attribute; a missing status reads as running.
    void read_attrs(const loa::core::Node& node);

    [[nodiscard]] NodeId node() const noexcept { return node_; }
    [[nodiscard]] JobKind kind() const noexcept { return kind_; }
    [[nodiscard]] JobStatus status() const noexcept { return status_; }
    // The harness's job or subagent id.
    [[nodiscard]] std::optional<std::string_view> id() const;
    // The child session file of a subagent.
    [[nodiscard]] std::optional<std::string_view> session() const;
    // Title, else agent, else command, else kind.
    [[nodiscard]] std::string_view label() const;
    [[nodiscard]] const std::string& output() const noexcept { return output_; }
    // The last non-blank output line, trimmed.
    [[nodiscard]] std::string_view last_line() const;
    [[nodiscard]] bool running() const noexcept { return status_ == JobStatus::Running; }
    [[nodiscard]] bool expanded() const noexcept { return expanded_; }
    [[nodiscard]] std::optional<std::int64_t> exit_code() const noexcept { return exit_code_; }
    // The raw `id` attribute (possibly empty).
    [[nodiscard]] const std::optional<std::string>& raw_id() const noexcept { return attrs_[0]; }
    // Byte offset where the last `TAIL_LINES` lines of the output start.
    [[nodiscard]] std::size_t tail_start() const;
    // Lines in the output, counting an unterminated last line.
    [[nodiscard]] std::size_t line_count() const;

    void set_output(std::string_view text);
    void append_output(std::string_view chunk);
    void set_expanded(bool e) noexcept { expanded_ = e; }

    friend bool operator==(const Job&, const Job&) = default;

  private:
    friend class State;
    NodeId node_;
    JobKind kind_ = JobKind::Shell;
    JobStatus status_ = JobStatus::Running;
    // id, title, command, agent, session.
    std::array<std::optional<std::string>, 5> attrs_;
    std::optional<std::int64_t> started_, ended_, exit_code_;
    std::string output_;
    bool expanded_ = false;
    std::size_t newlines_ = 0;
};

// `Tick` carries unix ms; `Toggle` shows or hides a job's output; `ToggleFull` shows all of it
// or only its last lines; `Open` opens a subagent's child subtab; `Detach` opens a subagent's
// transcript in a read-only window.
namespace msg {
struct Tick {
    std::int64_t now = 0;
};
struct Toggle {
    NodeId node;
};
struct ToggleFull {
    NodeId node;
};
struct Open {
    NodeId node;
};
struct Detach {
    NodeId node;
};
struct Cancel {
    NodeId node;
};
} // namespace msg
using Message = std::variant<msg::Tick, msg::Toggle, msg::ToggleFull, msg::Open, msg::Detach, msg::Cancel>;
using Event = loa::view::panel::AgentsIntent;

// Jobs of one session in document order, the active `wait` calls, the jobs shown in full and the
// last tick in unix ms.
class State {
  public:
    State() = default;
    [[nodiscard]] static State agents() { return State(PaneKind::Agents); }
    [[nodiscard]] static State background_jobs() { return State(PaneKind::Jobs); }
    [[nodiscard]] static State from_dom(const loa::core::Dom& dom, PaneKind kind = PaneKind::Agents);

    // Rebuilds from `dom`, keeping rows that were expanded.
    void reset(const loa::core::Dom& dom);
    // Follows `patch`, already applied to `dom`.
    void apply(const loa::core::Dom& dom, const loa::core::Patch& patch);
    std::pair<Task, std::optional<Event>> update(Message message);

    [[nodiscard]] PaneKind kind() const noexcept { return kind_; }
    // In node (document) order.
    [[nodiscard]] const std::map<NodeId, Job>& jobs() const noexcept { return jobs_; }
    [[nodiscard]] const Job* job(NodeId node) const;
    // A `wait` call blocks on the running jobs.
    [[nodiscard]] bool waiting() const noexcept { return !waits_.empty(); }
    [[nodiscard]] bool is_awaited(NodeId node) const;
    [[nodiscard]] bool is_full(NodeId node) const { return full_.contains(node); }
    // How long the job ran: to `ended`, or to the last tick while it runs.
    [[nodiscard]] std::optional<std::int64_t> elapsed_ms(NodeId node) const;

  private:
    explicit State(PaneKind kind) : kind_(kind) {}
    [[nodiscard]] bool accepts(JobKind kind) const noexcept;
    void refresh_wait(const loa::core::Node& node);

    PaneKind kind_ = PaneKind::Agents;
    std::map<NodeId, Job> jobs_;
    std::set<NodeId> waits_;
    std::set<NodeId> full_;
    std::optional<std::int64_t> now_;
};

// A pending or running `<call tool="wait">`.
[[nodiscard]] bool is_active_wait(const loa::core::Node& node);

} // namespace loa::ui::panel::agents
