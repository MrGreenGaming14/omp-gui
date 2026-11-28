// The Session tree tab's data (port of `loa-app/src/areas/sessions/tree.rs`): the fork family
// of the active session, built from the `parent_session` / `forked_at` links on each session's
// root and the turn rows of the journal.

#include "sessions.hpp"

#include <algorithm>
#include <set>

#include "loa/app/perform.hpp"
#include "loa/core/schema.hpp"

namespace loa::app::areas::sessions::tree {

namespace {

namespace attr = core::attr;
using loa::view::panel::SessionInfo;
using loa::view::panel::TurnInfo;

// Journal reads for `Tree::refresh`: the links of every stored session when `scan` is set, and
// the turn rows of `missing`. Sessions in `open` are read from memory by the caller, so their
// links and lines are skipped here.
Loaded read(const session::Library& library, bool scan, const std::vector<std::string>& missing,
            const std::set<std::string, std::less<>>& open) {
    Loaded loaded;
    loaded.scanned = scan;
    if (scan) {
        auto listings = library.store().list();
        if (listings)
            for (const auto& listing : *listings) {
                const auto* info = std::get_if<bus::SessionInfo>(&listing);
                if (!info || open.contains(info->id)) continue;
                if (auto dom = library.load(info->id)) loaded.links.insert_or_assign(info->id, Link::of(*dom));
            }
    }
    for (const auto& id : missing) {
        auto seqs = library.turn_seqs(id).value_or(Seqs{});
        std::optional<std::map<NodeId, std::string>> lines;
        if (!open.contains(id)) {
            lines.emplace();
            if (auto dom = library.load(id))
                for (const auto& [node, _] : seqs) lines->emplace(node, first_line(*dom, node));
        }
        loaded.turns.insert_or_assign(id, Turns{std::move(seqs), std::move(lines)});
    }
    return loaded;
}

} // namespace

Link Link::of(const core::Dom& dom) {
    Link link;
    if (const auto* root = dom.root()) {
        if (auto parent = root->str_attr(attr::PARENT_SESSION)) link.parent = std::string(*parent);
        if (auto seq = root->int_attr(attr::FORKED_AT); seq && *seq >= 0) link.forked_at = static_cast<std::uint64_t>(*seq);
    }
    return link;
}

std::string first_line(const core::Dom& dom, NodeId turn) {
    for (const auto* message : dom.children_of_kind(turn, core::ElementKind::Message)) {
        if (message->str_attr(attr::ROLE) != std::optional(core::as_str(core::Role::User))) continue;
        std::string_view text = message->text;
        while (!text.empty()) {
            const auto end = text.find('\n');
            auto line = text.substr(0, end);
            const auto first = line.find_first_not_of(" \t\r");
            if (first != std::string_view::npos) {
                const auto last = line.find_last_not_of(" \t\r");
                return std::string(line.substr(first, last - first + 1));
            }
            if (end == std::string_view::npos) break;
            text.remove_prefix(end + 1);
        }
        return {};
    }
    return {};
}

void Tree::forget(std::string_view id) {
    if (auto it = links_.find(id); it != links_.end()) links_.erase(it);
    if (auto it = seqs_.find(id); it != seqs_.end()) seqs_.erase(it);
    if (auto it = closed_lines_.find(id); it != closed_lines_.end()) closed_lines_.erase(it);
    if (auto it = pending_.find(id); it != pending_.end()) pending_.erase(it);
}

void Tree::record_turn(std::string_view id, NodeId turn, std::uint64_t seq) {
    if (auto it = closed_lines_.find(id); it != closed_lines_.end()) closed_lines_.erase(it);
    if (auto it = seqs_.find(id); it != seqs_.end())
        if (std::ranges::none_of(it->second, [&](const auto& row) { return row.first == turn; }))
            it->second.emplace_back(turn, seq);
    if (loading_) pending_[std::string(id)].emplace_back(turn, seq);
}

Task Tree::refresh(const Ctx& ctx) {
    const auto& sessions = ctx.runtime.sessions();
    const auto* active = sessions.active();
    if (!active) {
        view.set_family({}, "");
        return Task::none();
    }
    const std::string current = active->id().str();
    links_.insert_or_assign(current, Link::of(active->dom()));
    for (const auto& [id, session] : sessions.iter()) links_.insert_or_assign(id.str(), Link::of(session.dom()));

    // The family's root: the oldest ancestor still stored.
    std::string top = current;
    std::set<std::string, std::less<>> seen{top};
    for (;;) {
        auto it = links_.find(top);
        if (it == links_.end() || !it->second.parent) break;
        const auto parent = *it->second.parent;
        if (!links_.contains(parent) || !seen.insert(parent).second) break;
        top = parent;
    }
    std::map<std::string_view, std::vector<std::string_view>> children;
    for (const auto& [id, link] : links_)
        if (link.parent) children[*link.parent].push_back(id);
    std::vector<std::string_view> family;
    std::vector<std::string_view> queue{top};
    std::set<std::string_view> visited;
    while (!queue.empty()) {
        auto id = queue.back();
        queue.pop_back();
        if (!visited.insert(id).second) continue;
        family.push_back(id);
        if (auto it = children.find(id); it != children.end()) queue.insert(queue.end(), it->second.begin(), it->second.end());
    }
    std::vector<std::string> missing;
    std::vector<SessionInfo> infos;
    infos.reserve(family.size());
    for (auto id : family) {
        auto it = links_.find(id);
        Link link = it == links_.end() ? Link{} : it->second;
        std::vector<TurnInfo> turns;
        if (id == current || children.contains(id)) {
            if (needs_read(ctx, id)) missing.emplace_back(id);
            turns = turns_of(ctx, id);
        }
        const auto* summary = ctx.shell.sidebar().session(id);
        infos.push_back(SessionInfo{std::string(id), std::move(link.parent), link.forked_at,
                                    summary ? summary->title : std::string(), std::move(turns)});
    }
    view.set_family(std::move(infos), current);

    if ((!scanned_ || !missing.empty()) && !loading_) {
        loading_ = true;
        std::set<std::string, std::less<>> open;
        for (const auto& [id, _] : sessions.iter()) open.insert(id.str());
        return blocking(
            [library = ctx.runtime.library(), scan = !scanned_, missing = std::move(missing), open = std::move(open)] {
                return read(*library, scan, missing, open);
            },
            [](Loaded loaded) -> loa::app::Message {
                return State::message(msg::TreeLoaded{std::make_shared<const Loaded>(std::move(loaded))});
            });
    }
    return Task::none();
}

void Tree::merge(Loaded loaded) {
    loading_ = false;
    if (loaded.scanned) {
        scanned_ = true;
        for (auto& [id, link] : loaded.links) links_.try_emplace(id, std::move(link));
    }
    auto pending = std::exchange(pending_, {});
    for (auto& [id, turns] : loaded.turns) {
        if (!links_.contains(id)) continue;
        auto seqs = std::move(turns.seqs);
        auto lines = std::move(turns.lines);
        if (auto recorded = pending.find(id); recorded != pending.end()) {
            seqs.insert(seqs.end(), recorded->second.begin(), recorded->second.end());
            pending.erase(recorded);
            std::set<NodeId> known;
            std::erase_if(seqs, [&](const auto& row) { return !known.insert(row.first).second; });
            std::ranges::stable_sort(seqs, {}, [](const auto& row) { return row.second; });
            // The lines were read before these turns existed; a later read repairs them.
            lines.reset();
        }
        if (lines) closed_lines_.insert_or_assign(id, std::move(*lines));
        else if (auto it = closed_lines_.find(id); it != closed_lines_.end()) closed_lines_.erase(it);
        seqs_.insert_or_assign(id, std::move(seqs));
    }
}

bool Tree::needs_read(const Ctx& ctx, std::string_view id) const {
    return !seqs_.contains(id) || (!ctx.runtime.sessions().get(id) && !closed_lines_.contains(id));
}

std::vector<TurnInfo> Tree::turns_of(const Ctx& ctx, std::string_view id) const {
    static const Seqs none;
    auto found = seqs_.find(id);
    const auto& seqs = found == seqs_.end() ? none : found->second;
    std::vector<TurnInfo> turns;
    if (const auto* session = ctx.runtime.sessions().get(id)) {
        const auto& dom = session->dom();
        for (const auto& [node, seq] : seqs)
            if (dom.contains(node)) turns.push_back(TurnInfo{node, seq, first_line(dom, node)});
        return turns;
    }
    auto lines = closed_lines_.find(id);
    for (const auto& [node, seq] : seqs) {
        std::string line;
        if (lines != closed_lines_.end())
            if (auto it = lines->second.find(node); it != lines->second.end()) line = it->second;
        turns.push_back(TurnInfo{node, seq, std::move(line)});
    }
    return turns;
}

} // namespace loa::app::areas::sessions::tree
