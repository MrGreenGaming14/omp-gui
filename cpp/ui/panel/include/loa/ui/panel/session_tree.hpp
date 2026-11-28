#pragma once

// The session tree tab: a fork family with forks under the parent turn they started after, and
// the current session's turns. Port of `crates/loa-ui-panel/src/session_tree.rs`. Plain C++.

#include <cstddef>
#include <cstdint>
#include <optional>
#include <set>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/shell/task.hpp"
#include "loa/view/panel.hpp"

namespace loa::ui::panel::session_tree {

using Task = loa::ui::shell::Task;
using loa::view::panel::SessionInfo;

// A session; where the children below forked (a turn's first line, or `at #seq`); or a current session's turn.
namespace kind {
struct Session {
    std::string_view id, title;
    bool current = false, on_path = false, collapsed = false;
    friend bool operator==(const Session&, const Session&) = default;
};
struct ForkPoint {
    std::string_view session;
    std::uint64_t seq = 0;
    std::optional<std::string_view> first_line;
    friend bool operator==(const ForkPoint&, const ForkPoint&) = default;
};
struct Turn {
    std::string_view session;
    loa::core::NodeId node;
    std::string_view first_line;
    friend bool operator==(const Turn&, const Turn&) = default;
};
} // namespace kind
using RowKind = std::variant<kind::Session, kind::ForkPoint, kind::Turn>;

// One visible row; `index` is stable under collapsing and is what messages carry.
struct Row {
    std::size_t index = 0;
    std::uint16_t depth = 0;
    RowKind kind;
    friend bool operator==(const Row&, const Row&) = default;
};

// Every index is a `Row::index`.
namespace msg {
struct Toggle {
    std::size_t index = 0;
};
struct Rewind {
    std::size_t index = 0;
};
struct Fork {
    std::size_t index = 0;
};
struct Open {
    std::size_t index = 0;
};
struct Select {
    std::size_t index = 0;
};
} // namespace msg
using Message = std::variant<msg::Toggle, msg::Rewind, msg::Fork, msg::Open, msg::Select>;
using Event = loa::view::panel::SessionTreeIntent;

class State {
  public:
    // Replaces the family; `current` is the session open in the chat. Collapsed ids that remain are kept.
    void set_family(std::vector<SessionInfo> sessions, std::string_view current);
    // The visible rows, top to bottom; they view the family.
    [[nodiscard]] std::vector<Row> rows() const;
    [[nodiscard]] std::optional<std::size_t> selected() const noexcept { return selected_; }
    // Whether the layout row after `index` is deeper, i.e. the session at `index` has a subtree.
    [[nodiscard]] bool has_subtree(std::size_t index) const noexcept;

    std::pair<Task, std::optional<Event>> update(Message message);

    // A layout row as indices into the family.
    struct Slot {
        enum class Kind : std::uint8_t { Session, ForkPoint, Turn };
        Kind kind = Kind::Session;
        std::size_t session = 0;
        std::uint64_t seq = 0;            // ForkPoint
        std::optional<std::size_t> turn;  // ForkPoint (may be none), Turn
    };

  private:
    [[nodiscard]] Row row(std::size_t index) const;
    [[nodiscard]] std::optional<std::pair<std::string, loa::core::NodeId>> current_turn(std::size_t index) const;
    void refresh_visible();

    std::vector<SessionInfo> sessions_;
    std::optional<std::size_t> current_;
    std::vector<bool> on_path_;
    std::vector<std::pair<std::uint16_t, Slot>> layout_;
    std::vector<std::size_t> visible_;
    std::set<std::string, std::less<>> collapsed_;
    std::optional<std::size_t> selected_;
};

} // namespace loa::ui::panel::session_tree
