#pragma once

// Flattening of the session DOM into transcript items: one virtual-list row each. Port of
// `crates/loa-ui-session/src/transcript/items.rs`. Plain C++.

#include <cstdint>
#include <functional>
#include <optional>
#include <string_view>
#include <vector>

#include "loa/core/dom.hpp"

namespace loa::ui::session::transcript {

using loa::core::NodeId;

// `type` of the `<ext>` where adapters record the harness's slash commands: composer data,
// not a transcript row.
inline constexpr std::string_view COMMANDS_EXT = "available_commands_update";

// What one transcript row shows; keys the `rows.hpp` renderer registry.
enum class ItemKind : std::uint8_t { User, Assistant, Thinking, System, Calls, Decision, Job, Todo, Surface, Compaction, Ext };

// The Rust variant name of `k` ("User", "Calls", …).
[[nodiscard]] std::string_view name(ItemKind k);

// One row: `key` is the node it starts at (a call group's first call), `top` the child of
// `<session>` it belongs to, `calls` every call of a call group.
struct Item {
    NodeId key;
    NodeId top;
    ItemKind kind = ItemKind::Assistant;
    std::vector<NodeId> calls;
    friend bool operator==(const Item&, const Item&) = default;
};

// Reports a visited node and the key of the row that shows it.
using Own = std::function<void(NodeId node, NodeId key)>;

// Appends the rows of the subtree at `node` (a descendant of `top`) to `out`, grouping
// consecutive calls of one tool, and reports every visited node through `own`.
void flatten(const loa::core::Dom& dom, NodeId top, const loa::core::Node& node, std::vector<Item>& out, const Own& own);

// The child of `<session>` holding `id`; none for the session itself or a missing node.
[[nodiscard]] std::optional<NodeId> top_of(const loa::core::Dom& dom, NodeId id);

} // namespace loa::ui::session::transcript
