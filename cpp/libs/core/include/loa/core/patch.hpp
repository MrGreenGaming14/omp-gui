#pragma once

// Patches: the only way a session DOM changes. A `Draft` is a wanted change;
// the session's single writer orders it into a `Patch` (seq, ts, new id).

#include <cstdint>
#include <optional>
#include <string>
#include <variant>

#include "loa/core/schema.hpp"
#include "loa/core/value.hpp"

namespace loa::core {

namespace op {
// Add a node under `parent`. `index` is the position among the parent's
// children, `nullopt` appends. `parent` is `NodeId::DOCUMENT` for the root.
struct Insert {
    NodeId parent;
    std::optional<std::uint32_t> index;
    ElementKind kind;
    Attrs attrs;
    std::string text;
    friend bool operator==(const Insert&, const Insert&) = default;
};
// Set an attribute, or clear it with `nullopt`. The name `TEXT` replaces the
// node text.
struct Set {
    std::string name;
    std::optional<Value> value;
    friend bool operator==(const Set&, const Set&) = default;
};
// Append to the node text. Used for streaming tokens.
struct Append {
    std::string text;
    friend bool operator==(const Append&, const Append&) = default;
};
// Remove the node and its subtree.
struct Remove {
    friend bool operator==(const Remove&, const Remove&) = default;
};
} // namespace op

using Op = std::variant<op::Insert, op::Set, op::Append, op::Remove>;

// One committed change. `seq` starts at 1 and has no gaps within a journal.
struct Patch {
    std::uint64_t seq = 0;
    // The node the patch acts on. For `Insert` it is the id of the new node.
    NodeId node;
    // Unix milliseconds.
    std::int64_t ts = 0;
    Op op;
    friend bool operator==(const Patch&, const Patch&) = default;
};

// A change that has not been ordered yet.
struct Draft {
    struct Insert {
        NodeId parent;
        std::optional<std::uint32_t> index;
        ElementKind kind;
        Attrs attrs;
        std::string text;
        friend bool operator==(const Insert&, const Insert&) = default;
    };
    struct Set {
        NodeId node;
        std::string name;
        std::optional<Value> value;
        friend bool operator==(const Set&, const Set&) = default;
    };
    struct Append {
        NodeId node;
        std::string text;
        friend bool operator==(const Append&, const Append&) = default;
    };
    struct Remove {
        NodeId node;
        friend bool operator==(const Remove&, const Remove&) = default;
    };

    std::variant<Insert, Set, Append, Remove> v;

    // Append a new `kind` node as the last child of `parent`.
    [[nodiscard]] static Draft insert(NodeId parent, ElementKind kind, Attrs attrs = {}, std::string text = {});
    // Like `insert` at a position among the parent's children.
    [[nodiscard]] static Draft insert_at(NodeId parent, std::uint32_t index, ElementKind kind, Attrs attrs = {},
                                         std::string text = {});
    [[nodiscard]] static Draft set(NodeId node, std::string name, Value value);
    [[nodiscard]] static Draft clear(NodeId node, std::string name);
    // Replace the node text.
    [[nodiscard]] static Draft set_text(NodeId node, std::string text);
    [[nodiscard]] static Draft append(NodeId node, std::string text);
    [[nodiscard]] static Draft remove(NodeId node);

    friend bool operator==(const Draft&, const Draft&) = default;
};

} // namespace loa::core
