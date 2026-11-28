#include "loa/core/dom.hpp"

#include <algorithm>
#include <format>
#include <unordered_set>

namespace loa::core {

namespace {
template <class... F>
struct overloaded : F... {
    using F::operator()...;
};
} // namespace

const Value* Node::attr(std::string_view name) const {
    auto it = attrs.find(name);
    return it == attrs.end() ? nullptr : &it->second;
}
std::optional<std::string_view> Node::str_attr(std::string_view name) const {
    const Value* v = attr(name);
    return v ? v->as_str() : std::nullopt;
}
std::optional<std::int64_t> Node::int_attr(std::string_view name) const {
    const Value* v = attr(name);
    return v ? v->as_int() : std::nullopt;
}
std::optional<bool> Node::bool_attr(std::string_view name) const {
    const Value* v = attr(name);
    return v ? v->as_bool() : std::nullopt;
}

std::string to_string(const ApplyError& e) {
    using namespace apply_error;
    return std::visit(overloaded{
                          [](const Sequence& v) {
                              return std::format("patch seq {} applied to a DOM that expects {}", v.got, v.expected);
                          },
                          [](const MissingNode& v) { return std::format("node {} doesn't exist", v.node.value); },
                          [](const IdReused& v) {
                              return std::format("node id {} was used before, the next free id is {}", v.got.value,
                                                 v.next.value);
                          },
                          [](const RootExists&) { return std::string("the document already has a root"); },
                          [](const Index& v) {
                              return std::format("index {} is past the {} children of node {}", v.index, v.len,
                                                 v.parent.value);
                          },
                          [](const Schema& v) { return to_string(v.error); },
                      },
                      e);
}

std::string to_string(const PartsError& e) {
    using namespace parts_error;
    return std::visit(overloaded{
                          [](const Duplicate& v) { return std::format("node {} appears twice", v.node.value); },
                          [](const Link& v) {
                              return std::format("node {} lists child {}, which is missing, listed twice or names "
                                                 "another parent",
                                                 v.parent.value, v.child.value);
                          },
                          [](const Unreachable& v) {
                              return std::format("node {} can't be reached from the root", v.node.value);
                          },
                          [](const Roots& v) { return std::format("expected one root, found {}", v.count); },
                          [](const NextId& v) {
                              return std::format("next id {} isn't above node {}", v.next.value, v.max.value);
                          },
                      },
                      e);
}

loa::Error to_error(const ApplyError& e) {
    const auto code = std::holds_alternative<apply_error::MissingNode>(e) ? ErrorCode::not_found
                                                                          : ErrorCode::invalid_argument;
    return {code, to_string(e)};
}

loa::Error to_error(const PartsError& e) { return {ErrorCode::parse, to_string(e)}; }

const Node* Descendants::next() {
    while (!stack_.empty()) {
        const NodeId id = stack_.back();
        stack_.pop_back();
        const Node* node = dom_->get(id);
        if (!node) return nullptr;
        stack_.insert(stack_.end(), node->children.rbegin(), node->children.rend());
        return node;
    }
    return nullptr;
}

std::expected<Dom, PartsError> Dom::from_parts(std::vector<Node> nodes, NodeId next_id, std::uint64_t seq) {
    Dom dom;
    dom.nodes_.reserve(nodes.size());
    for (Node& node : nodes) {
        const NodeId id = node.id;
        if (!dom.nodes_.try_emplace(id, std::move(node)).second) return std::unexpected(parts_error::Duplicate{id});
    }
    std::size_t roots = 0;
    for (const auto& [id, node] : dom.nodes_) {
        if (node.parent != NodeId::DOCUMENT) continue;
        if (roots++ == 0) dom.root_ = id;
    }
    if (roots > 1 || (roots == 0 && !dom.nodes_.empty())) return std::unexpected(parts_error::Roots{roots});
    // Every listed child must exist, name its lister and be listed once.
    std::unordered_set<NodeId> seen;
    seen.reserve(dom.nodes_.size());
    std::vector<NodeId> stack;
    if (dom.root_) {
        stack.push_back(*dom.root_);
        seen.insert(*dom.root_);
    }
    while (!stack.empty()) {
        const NodeId parent = stack.back();
        stack.pop_back();
        for (NodeId child : dom.nodes_.at(parent).children) {
            auto it = dom.nodes_.find(child);
            const bool linked = it != dom.nodes_.end() && it->second.parent == parent && seen.insert(child).second;
            if (!linked) return std::unexpected(parts_error::Link{child, parent});
            stack.push_back(child);
        }
    }
    NodeId max = NodeId::DOCUMENT;
    for (const auto& [id, node] : dom.nodes_) {
        if (!seen.contains(id)) return std::unexpected(parts_error::Unreachable{id});
        max = std::max(max, id);
    }
    if (next_id <= max) return std::unexpected(parts_error::NextId{next_id, max});
    dom.last_id_ = next_id.value - 1;
    dom.seq_ = seq;
    return dom;
}

const Node* Dom::root() const { return root_ ? get(*root_) : nullptr; }

const Node* Dom::get(NodeId id) const {
    auto it = nodes_.find(id);
    return it == nodes_.end() ? nullptr : &it->second;
}

std::vector<const Node*> Dom::children(NodeId id) const {
    std::vector<const Node*> out;
    if (const Node* n = get(id)) {
        out.reserve(n->children.size());
        for (NodeId c : n->children)
            if (const Node* child = get(c)) out.push_back(child);
    }
    return out;
}

std::vector<const Node*> Dom::children_of_kind(NodeId id, ElementKind kind) const {
    auto out = children(id);
    std::erase_if(out, [kind](const Node* n) { return n->kind != kind; });
    return out;
}

Descendants Dom::descendants(NodeId id) const {
    std::vector<NodeId> stack;
    if (contains(id)) stack.push_back(id);
    return {*this, std::move(stack)};
}

Descendants Dom::iter() const {
    std::vector<NodeId> stack;
    if (root_) stack.push_back(*root_);
    return {*this, std::move(stack)};
}

std::vector<const Node*> Dom::ancestors(NodeId id) const {
    std::vector<const Node*> out;
    const Node* n = get(id);
    while (n && (n = get(n->parent))) out.push_back(n);
    return out;
}

Patch Dom::prepare(Draft draft, std::int64_t ts) const {
    return std::visit(overloaded{
                          [&](Draft::Insert& d) {
                              return Patch{seq_ + 1, next_id(), ts,
                                           op::Insert{d.parent, d.index, d.kind, std::move(d.attrs), std::move(d.text)}};
                          },
                          [&](Draft::Set& d) {
                              return Patch{seq_ + 1, d.node, ts, op::Set{std::move(d.name), std::move(d.value)}};
                          },
                          [&](Draft::Append& d) { return Patch{seq_ + 1, d.node, ts, op::Append{std::move(d.text)}}; },
                          [&](Draft::Remove& d) { return Patch{seq_ + 1, d.node, ts, op::Remove{}}; },
                      },
                      draft.v);
}

std::expected<Patch, ApplyError> Dom::commit(Draft draft, std::int64_t ts) {
    Patch patch = prepare(std::move(draft), ts);
    if (auto ok = apply(patch); !ok) return std::unexpected(std::move(ok.error()));
    return patch;
}

std::expected<void, ApplyError> Dom::apply(const Patch& patch) {
    if (auto ok = check(patch); !ok) return ok;
    mutate(patch);
    return {};
}

std::expected<void, ApplyError> Dom::check(const Patch& patch) const {
    if (patch.seq != seq_ + 1) return std::unexpected(apply_error::Sequence{seq_ + 1, patch.seq});
    const NodeId id = patch.node;
    auto schema = [](std::expected<void, SchemaError> r) -> std::expected<void, ApplyError> {
        if (!r) return std::unexpected(apply_error::Schema{std::move(r.error())});
        return {};
    };
    return std::visit(
        overloaded{
            [&](const op::Insert& o) -> std::expected<void, ApplyError> {
                if (id.value <= last_id_) return std::unexpected(apply_error::IdReused{id, next_id()});
                std::optional<ElementKind> parent_kind;
                if (o.parent == NodeId::DOCUMENT) {
                    if (root_) return std::unexpected(apply_error::RootExists{});
                } else {
                    const Node* p = get(o.parent);
                    if (!p) return std::unexpected(apply_error::MissingNode{o.parent});
                    if (o.index && *o.index > p->children.size())
                        return std::unexpected(apply_error::Index{o.parent, *o.index, p->children.size()});
                    parent_kind = p->kind;
                }
                return schema(validate_insert(parent_kind, o.kind, o.attrs));
            },
            [&](const op::Set& o) -> std::expected<void, ApplyError> {
                const Node* n = get(id);
                if (!n) return std::unexpected(apply_error::MissingNode{id});
                return schema(validate_set(n->kind, o.name, o.value ? &*o.value : nullptr));
            },
            [&](const auto&) -> std::expected<void, ApplyError> {
                if (!contains(id)) return std::unexpected(apply_error::MissingNode{id});
                return {};
            },
        },
        patch.op);
}

void Dom::mutate(const Patch& patch) {
    const NodeId id = patch.node;
    std::visit(overloaded{
                   [&](const op::Insert& o) {
                       if (auto it = nodes_.find(o.parent); it != nodes_.end()) {
                           auto& ch = it->second.children;
                           ch.insert(ch.begin() + (o.index ? *o.index : ch.size()), id);
                       } else {
                           root_ = id;
                       }
                       nodes_.insert_or_assign(id, Node{id, o.kind, o.parent, {}, o.attrs, o.text});
                       last_id_ = id.value;
                   },
                   [&](const op::Set& o) {
                       Node& node = nodes_.at(id);
                       if (o.name == TEXT) {
                           auto text = o.value ? o.value->as_str() : std::nullopt;
                           if (text) node.text.assign(*text);
                           else node.text.clear();
                       } else if (o.value) {
                           node.attrs.insert_or_assign(o.name, *o.value);
                       } else {
                           node.attrs.erase(o.name);
                       }
                   },
                   [&](const op::Append& o) { nodes_.at(id).text += o.text; },
                   [&](const op::Remove&) {
                       const NodeId parent = nodes_.at(id).parent;
                       std::vector<NodeId> doomed;
                       for (const Node& n : descendants(id)) doomed.push_back(n.id);
                       for (NodeId gone : doomed) nodes_.erase(gone);
                       if (auto it = nodes_.find(parent); it != nodes_.end()) std::erase(it->second.children, id);
                       else root_.reset();
                   },
               },
               patch.op);
    seq_ = patch.seq;
}

} // namespace loa::core
