#include "loa/core/rewind.hpp"

#include <algorithm>
#include <bit>
#include <format>
#include <ranges>
#include <set>
#include <unordered_set>

namespace loa::core::rewind {

namespace {

// Value equality where a float equals itself bit for bit, so `NaN` isn't a change.
bool same_value(const Value& a, const Value& b) {
    if (auto x = a.as_float(), y = b.as_float(); x && y) return std::bit_cast<std::uint64_t>(*x) == std::bit_cast<std::uint64_t>(*y);
    if (auto x = a.as_list(), y = b.as_list(); x && y) {
        if (x->size() != y->size()) return false;
        for (std::size_t i = 0; i < x->size(); ++i)
            if (!same_value((*x)[i], (*y)[i])) return false;
        return true;
    }
    return a == b;
}

bool same(const Node& a, const Node& b) { return a.kind == b.kind && a.parent == b.parent; }

bool live_in(const Dom& dom, NodeId id) {
    const Node* n = dom.get(id);
    return n && is_live(*n);
}

std::vector<NodeId> live_only_in(const Dom& dom, const Dom& other) {
    std::vector<NodeId> out;
    for (const Node& n : dom.iter())
        if (is_live(n) && !live_in(other, n.id)) out.push_back(n.id);
    return out;
}

} // namespace

std::string to_string(const Inexpressible& e) {
    if (auto* v = std::get_if<inexpressible::Added>(&e))
        return std::format("node {} exists only in the target state and ids are never reused", v->node.value);
    return std::format("node {} has an attribute named `{}`, which a set patch can't write",
                       std::get<inexpressible::TextAttr>(e).node.value, TEXT);
}

RewindPlan plan(const Dom& current, const Dom& target) {
    auto cancel = live_only_in(current, target);
    std::ranges::reverse(cancel);
    auto d = drafts(current, target);
    Apply apply = d ? Apply(Drafts{std::move(*d)}) : Apply(Fork{target.seq(), std::move(d.error())});
    return {std::move(cancel), live_only_in(target, current), std::move(apply)};
}

std::expected<std::vector<Draft>, Inexpressible> drafts(const Dom& old_dom, const Dom& new_dom) {
    for (const Node& n : new_dom.iter()) {
        const Node* o = old_dom.get(n.id);
        if (!o || !same(*o, n)) return std::unexpected(inexpressible::Added{n.id});
    }
    std::unordered_set<NodeId> removed;
    std::vector<Draft> out, sets;
    for (const Node& o : old_dom.iter()) {
        const Node* n = new_dom.get(o.id);
        if (!n || !same(o, *n)) {
            // Parents come first in document order, so only tops are drafted.
            if (!removed.contains(o.parent)) out.push_back(Draft::remove(o.id));
            removed.insert(o.id);
            continue;
        }
        std::set<std::string_view> names;
        for (const auto& [k, _] : o.attrs) names.insert(k);
        for (const auto& [k, _] : n->attrs) names.insert(k);
        for (std::string_view name : names) {
            const Value* was = o.attr(name);
            const Value* now = n->attr(name);
            if (was && now && same_value(*was, *now)) continue;
            if (name == TEXT) return std::unexpected(inexpressible::TextAttr{o.id});
            sets.push_back({Draft::Set{o.id, std::string(name), now ? std::optional<Value>(*now) : std::nullopt}});
        }
        if (o.text != n->text) {
            if (!o.text.empty() && n->text.starts_with(o.text))
                sets.push_back(Draft::append(o.id, n->text.substr(o.text.size())));
            else
                sets.push_back(Draft::set_text(o.id, n->text));
        }
    }
    out.insert(out.end(), std::make_move_iterator(sets.begin()), std::make_move_iterator(sets.end()));
    return out;
}

bool is_live(const Node& node) {
    const auto status = node.str_attr(attr::STATUS);
    if (!status) return false;
    switch (node.kind) {
    case ElementKind::Turn: {
        auto s = parse<TurnStatus>(*status);
        return s == TurnStatus::Running || s == TurnStatus::Waiting;
    }
    case ElementKind::Message: return parse<MessageStatus>(*status) == MessageStatus::Streaming;
    case ElementKind::Call: {
        auto s = parse<CallStatus>(*status);
        return s == CallStatus::Pending || s == CallStatus::Running;
    }
    case ElementKind::Job: return parse<JobStatus>(*status) == JobStatus::Running;
    case ElementKind::Decision: return parse<DecisionStatus>(*status) == DecisionStatus::Waiting;
    default: return false;
    }
}

} // namespace loa::core::rewind
