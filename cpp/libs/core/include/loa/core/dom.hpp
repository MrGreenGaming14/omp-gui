#pragma once

// The session DOM: a typed tree that only changes by applying patches.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <iterator>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_map>
#include <variant>
#include <vector>

#include "loa/core/patch.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/value.hpp"
#include "loa/runtime/error.hpp"

namespace loa::core {

// One element. Read only outside this library: a `Dom` hands out `const Node*`.
struct Node {
    NodeId id;
    ElementKind kind;
    // `NodeId::DOCUMENT` for the root.
    NodeId parent;
    std::vector<NodeId> children;
    Attrs attrs;
    std::string text;

    [[nodiscard]] const Value* attr(std::string_view name) const;
    [[nodiscard]] std::optional<std::string_view> str_attr(std::string_view name) const;
    [[nodiscard]] std::optional<std::int64_t> int_attr(std::string_view name) const;
    [[nodiscard]] std::optional<bool> bool_attr(std::string_view name) const;

    friend bool operator==(const Node&, const Node&) = default;
};

namespace apply_error {
struct Sequence {
    std::uint64_t expected;
    std::uint64_t got;
    friend bool operator==(const Sequence&, const Sequence&) = default;
};
struct MissingNode {
    NodeId node;
    friend bool operator==(const MissingNode&, const MissingNode&) = default;
};
struct IdReused {
    NodeId got;
    NodeId next;
    friend bool operator==(const IdReused&, const IdReused&) = default;
};
struct RootExists {
    friend bool operator==(const RootExists&, const RootExists&) = default;
};
struct Index {
    NodeId parent;
    std::uint32_t index;
    std::size_t len;
    friend bool operator==(const Index&, const Index&) = default;
};
struct Schema {
    SchemaError error;
    friend bool operator==(const Schema&, const Schema&) = default;
};
} // namespace apply_error

using ApplyError = std::variant<apply_error::Sequence, apply_error::MissingNode, apply_error::IdReused,
                                apply_error::RootExists, apply_error::Index, apply_error::Schema>;

namespace parts_error {
struct Duplicate {
    NodeId node;
    friend bool operator==(const Duplicate&, const Duplicate&) = default;
};
struct Link {
    NodeId child;
    NodeId parent;
    friend bool operator==(const Link&, const Link&) = default;
};
struct Unreachable {
    NodeId node;
    friend bool operator==(const Unreachable&, const Unreachable&) = default;
};
struct Roots {
    std::size_t count;
    friend bool operator==(const Roots&, const Roots&) = default;
};
struct NextId {
    NodeId next;
    NodeId max;
    friend bool operator==(const NextId&, const NextId&) = default;
};
} // namespace parts_error

using PartsError = std::variant<parts_error::Duplicate, parts_error::Link, parts_error::Unreachable,
                                parts_error::Roots, parts_error::NextId>;

// The Rust `Display` texts.
[[nodiscard]] std::string to_string(const ApplyError& e);
[[nodiscard]] std::string to_string(const PartsError& e);
[[nodiscard]] loa::Error to_error(const ApplyError& e);
[[nodiscard]] loa::Error to_error(const PartsError& e);

class Dom;

// `Dom::descendants` and `Dom::iter`: parents before children, document order.
class Descendants {
  public:
    class iterator {
      public:
        using value_type = Node;
        using difference_type = std::ptrdiff_t;
        using reference = const Node&;
        iterator() = default;
        reference operator*() const { return *cur_; }
        const Node* operator->() const { return cur_; }
        iterator& operator++() {
            cur_ = owner_->next();
            return *this;
        }
        void operator++(int) { ++*this; }
        friend bool operator==(const iterator& it, std::default_sentinel_t) { return it.cur_ == nullptr; }

      private:
        friend class Descendants;
        iterator(Descendants* owner, const Node* cur) : owner_(owner), cur_(cur) {}
        Descendants* owner_ = nullptr;
        const Node* cur_ = nullptr;
    };

    // Single pass: `begin` starts consuming.
    iterator begin() { return {this, next()}; }
    std::default_sentinel_t end() const { return {}; }
    // The next node, or `nullptr` when done.
    const Node* next();

  private:
    friend class Dom;
    Descendants(const Dom& dom, std::vector<NodeId> stack) : dom_(&dom), stack_(std::move(stack)) {}
    const Dom* dom_;
    std::vector<NodeId> stack_;
};

// A session tree. Build one by applying patches from sequence 1, or from a
// snapshot with `Dom::from_parts`.
class Dom {
  public:
    Dom() = default;

    // Rebuilds a DOM from snapshot data: every node, the next free id and the
    // sequence of the last applied patch.
    [[nodiscard]] static std::expected<Dom, PartsError> from_parts(std::vector<Node> nodes, NodeId next_id,
                                                                   std::uint64_t seq);

    // Sequence number of the last applied patch. 0 for a new DOM.
    [[nodiscard]] std::uint64_t seq() const noexcept { return seq_; }
    // The lowest id an insert may use.
    [[nodiscard]] NodeId next_id() const noexcept { return NodeId{last_id_ + 1}; }
    [[nodiscard]] const Node* root() const;
    [[nodiscard]] const Node* get(NodeId id) const;
    [[nodiscard]] bool contains(NodeId id) const { return nodes_.contains(id); }
    // Number of nodes.
    [[nodiscard]] std::size_t size() const noexcept { return nodes_.size(); }
    [[nodiscard]] bool empty() const noexcept { return nodes_.empty(); }

    // Children of `id` in order. Empty when `id` doesn't exist.
    [[nodiscard]] std::vector<const Node*> children(NodeId id) const;
    // Children of `id` of one kind, in order.
    [[nodiscard]] std::vector<const Node*> children_of_kind(NodeId id, ElementKind kind) const;
    // `id` and everything under it, parents before children, in document order.
    [[nodiscard]] Descendants descendants(NodeId id) const;
    // Every node in document order.
    [[nodiscard]] Descendants iter() const;
    // Parents of `id` from the nearest up to the root.
    [[nodiscard]] std::vector<const Node*> ancestors(NodeId id) const;

    // Orders `draft` as the next patch without applying it.
    [[nodiscard]] Patch prepare(Draft draft, std::int64_t ts) const;
    // Orders `draft` as the next patch and applies it.
    [[nodiscard]] std::expected<Patch, ApplyError> commit(Draft draft, std::int64_t ts);
    // Applies the next patch. On error the DOM is unchanged.
    [[nodiscard]] std::expected<void, ApplyError> apply(const Patch& patch);
    // Every rule `apply` enforces, without applying, so a writer can reject a
    // patch before persisting it.
    [[nodiscard]] std::expected<void, ApplyError> check(const Patch& patch) const;

    friend bool operator==(const Dom&, const Dom&) = default;

  private:
    void mutate(const Patch& patch);

    std::unordered_map<NodeId, Node> nodes_;
    std::optional<NodeId> root_;
    // Highest id ever inserted. Ids at or below it are never accepted again.
    std::uint64_t last_id_ = 0;
    std::uint64_t seq_ = 0;
};

} // namespace loa::core
