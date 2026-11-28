// The session tree state. Port of `crates/loa-ui-panel/src/session_tree.rs` (`State`, `Tree`).

#include <algorithm>
#include <unordered_map>

#include "loa/ui/panel/session_tree.hpp"

namespace loa::ui::panel::session_tree {

namespace {

using Layout = std::vector<std::pair<std::uint16_t, State::Slot>>;
using Slot = State::Slot;

// Resolved fork links for laying the family out.
struct Tree {
    const std::vector<SessionInfo>& sessions;
    std::vector<std::vector<std::size_t>> children;
    std::optional<std::size_t> current;

    // Where child `c` forked: the parent turn it started after (none before the first turn) and that turn's sequence.
    [[nodiscard]] std::pair<std::optional<std::size_t>, std::uint64_t> point(std::size_t session, std::size_t c) const {
        const auto& turns = sessions[session].turns;
        const auto at = sessions[c].forked_at.value_or(0);
        const auto after = static_cast<std::size_t>(
            std::ranges::partition_point(turns, [&](const auto& t) { return t.seq <= at; }) - turns.begin());
        if (after == 0) return {std::nullopt, at};
        return {after - 1, turns[after - 1].seq};
    }

    // Appends the rows of `session` and its subtree. The current session lists every turn with its
    // fork point after it (forks before the first turn first); others only fork points.
    void emit(std::size_t session, std::uint16_t depth, Layout& out) const {
        out.push_back({depth, Slot{Slot::Kind::Session, session}});
        const auto& kids = children[session];
        // Children grouped by the point they forked from, in order.
        struct Group {
            std::pair<std::optional<std::size_t>, std::uint64_t> point;
            std::size_t begin, end;
        };
        std::vector<Group> groups;
        for (std::size_t k = 0; k < kids.size(); ++k) {
            const auto p = point(session, kids[k]);
            if (!groups.empty() && groups.back().point == p) ++groups.back().end;
            else groups.push_back({p, k, k + 1});
        }
        auto group = [&](const Group& g) {
            out.push_back({static_cast<std::uint16_t>(depth + 1), Slot{Slot::Kind::ForkPoint, session, g.point.second, g.point.first}});
            for (auto k = g.begin; k < g.end; ++k) emit(kids[k], static_cast<std::uint16_t>(depth + 2), out);
        };
        std::size_t next = 0;
        if (current != session) {
            for (const auto& g : groups) group(g);
            return;
        }
        while (next < groups.size() && !groups[next].point.first) group(groups[next++]);
        for (std::size_t turn = 0; turn < sessions[session].turns.size(); ++turn) {
            out.push_back({static_cast<std::uint16_t>(depth + 1), Slot{Slot::Kind::Turn, session, 0, turn}});
            if (next < groups.size() && groups[next].point.first == turn) group(groups[next++]);
        }
    }
};

} // namespace

void State::set_family(std::vector<SessionInfo> sessions, std::string_view current) {
    const auto n = sessions.size();
    std::unordered_map<std::string_view, std::size_t> by_id;
    by_id.reserve(n);
    for (std::size_t i = 0; i < n; ++i) by_id.try_emplace(sessions[i].id, i);
    auto find = [&](std::string_view id) -> std::optional<std::size_t> {
        const auto it = by_id.find(id);
        return it == by_id.end() ? std::nullopt : std::optional(it->second);
    };
    std::vector<std::optional<std::size_t>> declared(n), parent(n);
    for (std::size_t i = 0; i < n; ++i)
        if (sessions[i].parent) declared[i] = find(*sessions[i].parent);
    // A session whose ancestry still continues after more steps than there are sessions reaches a cycle.
    for (std::size_t i = 0; i < n; ++i) {
        std::optional<std::size_t> at = i;
        for (std::size_t step = 0; step <= n && at; ++step) at = declared[*at];
        parent[i] = at ? std::nullopt : declared[i];
    }
    std::vector<std::vector<std::size_t>> children(n);
    for (std::size_t i = 0; i < n; ++i)
        if (parent[i]) children[*parent[i]].push_back(i);
    for (auto& l : children)
        std::ranges::stable_sort(l, [&](std::size_t a, std::size_t b) {
            const auto fa = sessions[a].forked_at.value_or(0), fb = sessions[b].forked_at.value_or(0);
            return fa != fb ? fa < fb : sessions[a].id < sessions[b].id;
        });
    const auto cur = find(current);
    std::vector<bool> on_path(n, false);
    for (auto at = cur; at; at = parent[*at]) on_path[*at] = true;
    Layout layout;
    layout.reserve(n);
    {
        const Tree tree{sessions, std::move(children), cur};
        for (std::size_t r = 0; r < n; ++r)
            if (!parent[r]) tree.emit(r, 0, layout);
    }
    on_path_ = std::move(on_path);
    current_ = cur;
    layout_ = std::move(layout);
    sessions_ = std::move(sessions);
    selected_.reset();
    std::erase_if(collapsed_, [&](const std::string& id) {
        return std::ranges::none_of(sessions_, [&](const SessionInfo& s) { return s.id == id; });
    });
    refresh_visible();
}

std::vector<Row> State::rows() const {
    std::vector<Row> out;
    out.reserve(visible_.size());
    for (const auto i : visible_) out.push_back(row(i));
    return out;
}

bool State::has_subtree(std::size_t index) const noexcept {
    return index + 1 < layout_.size() && index < layout_.size() && layout_[index + 1].first > layout_[index].first;
}

std::pair<Task, std::optional<Event>> State::update(Message message) {
    namespace intent = loa::view::panel::session_tree_intent;
    std::optional<Event> event;
    if (const auto* m = std::get_if<msg::Toggle>(&message)) {
        if (m->index < layout_.size() && layout_[m->index].second.kind == Slot::Kind::Session) {
            const auto& id = sessions_[layout_[m->index].second.session].id;
            if (collapsed_.erase(id) == 0) collapsed_.insert(id);
        }
        refresh_visible();
    } else if (const auto* m = std::get_if<msg::Rewind>(&message)) {
        if (auto t = current_turn(m->index)) event = intent::Rewind{std::move(t->first), t->second};
    } else if (const auto* m = std::get_if<msg::Fork>(&message)) {
        if (auto t = current_turn(m->index)) event = intent::Fork{std::move(t->first), t->second};
    } else if (const auto* m = std::get_if<msg::Open>(&message)) {
        if (m->index < layout_.size() && layout_[m->index].second.kind == Slot::Kind::Session) {
            const auto s = layout_[m->index].second.session;
            if (current_ != s) event = intent::Open{sessions_[s].id};
        }
    } else if (const auto* m = std::get_if<msg::Select>(&message)) {
        if (m->index < layout_.size()) selected_ = m->index;
    }
    return {Task::none(), std::move(event)};
}

std::optional<std::pair<std::string, loa::core::NodeId>> State::current_turn(std::size_t index) const {
    if (index >= layout_.size()) return std::nullopt;
    const auto& slot = layout_[index].second;
    if (slot.kind != Slot::Kind::Turn || current_ != slot.session) return std::nullopt;
    const auto& s = sessions_[slot.session];
    return std::pair{s.id, s.turns[*slot.turn].node};
}

void State::refresh_visible() {
    visible_.clear();
    std::optional<std::uint16_t> hidden_below;
    for (std::size_t i = 0; i < layout_.size(); ++i) {
        const auto& [depth, slot] = layout_[i];
        if (hidden_below && depth > *hidden_below) continue;
        visible_.push_back(i);
        hidden_below.reset();
        if (slot.kind == Slot::Kind::Session && collapsed_.contains(sessions_[slot.session].id)) hidden_below = depth;
    }
}

Row State::row(std::size_t index) const {
    const auto& [depth, slot] = layout_[index];
    const auto& s = sessions_[slot.session];
    RowKind k;
    switch (slot.kind) {
    case Slot::Kind::Session:
        k = kind::Session{s.id, s.title, current_ == slot.session, on_path_[slot.session], collapsed_.contains(s.id)};
        break;
    case Slot::Kind::ForkPoint: {
        kind::ForkPoint f{s.id, slot.seq, std::nullopt};
        if (slot.turn) f.first_line = s.turns[*slot.turn].first_line;
        k = f;
        break;
    }
    case Slot::Kind::Turn: k = kind::Turn{s.id, s.turns[*slot.turn].node, s.turns[*slot.turn].first_line}; break;
    }
    return Row{index, depth, k};
}

} // namespace loa::ui::panel::session_tree
