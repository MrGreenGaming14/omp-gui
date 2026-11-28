#pragma once

// The widget operations a shell `update` asks the view to run (iced `Task` over
// `widget::operation`): focus a field, scroll or snap a list, query the system theme.
// Plain data, so state tests can assert what an update requested.

#include <initializer_list>
#include <optional>
#include <string>
#include <utility>
#include <variant>
#include <vector>

namespace loa::ui::shell {

namespace op {
// Focus the input with this id.
struct Focus {
    std::string id;
    friend bool operator==(const Focus&, const Focus&) = default;
};
// Scroll the scrollable with this id to an absolute offset; an unset axis stays.
struct ScrollTo {
    std::string id;
    std::optional<float> x, y;
    friend bool operator==(const ScrollTo&, const ScrollTo&) = default;
};
// Scroll by this many pixels.
struct ScrollBy {
    std::string id;
    float x = 0, y = 0;
    friend bool operator==(const ScrollBy&, const ScrollBy&) = default;
};
// Snap to a relative offset (0 to 1); an unset axis stays.
struct SnapTo {
    std::string id;
    std::optional<float> x, y;
    friend bool operator==(const SnapTo&, const SnapTo&) = default;
};
// Ask the system for its light/dark mode; the app answers with `layout::msg::SystemTheme`.
struct QuerySystemTheme {
    friend bool operator==(const QuerySystemTheme&, const QuerySystemTheme&) = default;
};
} // namespace op

using Op = std::variant<op::Focus, op::ScrollTo, op::ScrollBy, op::SnapTo, op::QuerySystemTheme>;

struct Task {
    std::vector<Op> ops;

    [[nodiscard]] static Task none() { return {}; }
    [[nodiscard]] static Task of(Op o) { return Task{{std::move(o)}}; }
    [[nodiscard]] static Task batch(std::initializer_list<Task> tasks) {
        Task t;
        for (const auto& each : tasks) t.ops.insert(t.ops.end(), each.ops.begin(), each.ops.end());
        return t;
    }
    [[nodiscard]] bool empty() const { return ops.empty(); }

    friend bool operator==(const Task&, const Task&) = default;
};

} // namespace loa::ui::shell
