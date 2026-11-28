#pragma once

// The Usage window: provider limits and saved resets from `omp usage --json` (port of the
// `State` of `loa-app/src/app/usage.rs`).

#include <expected>
#include <memory>
#include <optional>
#include <set>
#include <string>
#include <utility>
#include <variant>

#include "loa/app/message.hpp"
#include "loa/app/task.hpp"
#include "loa/app/usage.hpp"

class QWidget;

namespace loa::app::usage {

// What the app must act on.
namespace event {
struct Close {};
struct Meter {
    std::optional<loa::view::shell::UsageMeter> meter;
};
} // namespace event
using Event = std::variant<event::Close, event::Meter>;

class State {
  public:
    explicit State(std::shared_ptr<const UsageSource> source) : source_(std::move(source)) {}

    // Starts a fetch.
    Task refresh();
    std::pair<Task, std::optional<Event>> update(Message message);

    // The last read, or why it failed; `nullopt` before the first one ends.
    [[nodiscard]] const std::optional<std::expected<Usage, std::string>>& usage() const noexcept { return usage_; }
    [[nodiscard]] bool loading() const noexcept { return loading_; }
    // Whether provider `id`'s accounts are expanded.
    [[nodiscard]] bool is_open(std::string_view id) const { return open_.contains(id); }
    // The reset target being spent.
    [[nodiscard]] const std::optional<std::string>& spending() const noexcept { return spending_; }

  private:
    std::shared_ptr<const UsageSource> source_;
    std::optional<std::expected<Usage, std::string>> usage_;
    bool loading_ = false;
    std::set<std::string, std::less<>> open_;
    std::optional<std::string> spending_;
};

// The Usage window's body over `state` at `now_ms`; input goes to `send` as `msg::Usage`.
[[nodiscard]] QWidget* view(const State& state, std::int64_t now_ms, const Task::Emit& send);

} // namespace loa::app::usage
