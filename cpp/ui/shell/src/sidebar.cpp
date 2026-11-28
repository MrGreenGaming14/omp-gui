#include "loa/ui/shell/sidebar.hpp"

#include <algorithm>
#include <unordered_map>

#include "unicode.hpp"

namespace loa::ui::shell {

namespace {
template <class... F> struct Overloaded : F... {
    using F::operator()...;
};

std::vector<std::string_view> split_whitespace(std::string_view s) {
    std::vector<std::string_view> out;
    std::size_t i = 0;
    while (i < s.size()) {
        while (i < s.size() && (s[i] == ' ' || s[i] == '\t' || s[i] == '\n' || s[i] == '\r')) ++i;
        const auto start = i;
        while (i < s.size() && !(s[i] == ' ' || s[i] == '\t' || s[i] == '\n' || s[i] == '\r')) ++i;
        if (i > start) out.push_back(s.substr(start, i - start));
    }
    return out;
}
} // namespace

// ---- data.hpp ------------------------------------------------------------------------------

const loa::view::shell::NodeStatus* Nodes::get(std::string_view key) const {
    const auto it = std::ranges::find(list_, key, &loa::view::shell::NodeStatus::key);
    return it == list_.end() ? nullptr : &*it;
}

std::string_view Nodes::name_of(std::string_view key) const {
    if (const auto* n = get(key)) return n->name;
    std::size_t i = 0;
    for (int chars = 0; chars < 8 && i < key.size(); ++chars) (void)detail::next_char(key, i);
    return key.substr(0, i);
}

bool Nodes::is_online(std::string_view key) const {
    const auto* n = get(key);
    return n && n->online;
}

} // namespace loa::ui::shell

namespace loa::ui::shell::sidebar {

namespace is = loa::view::shell::sidebar_intent;

void State::set_sessions(std::vector<SessionSummary> sessions) {
    sessions_.clear();
    sessions_.reserve(sessions.size());
    for (auto& s : sessions) sessions_.push_back(entry(std::move(s)));
    std::erase_if(running_, [&](const std::string& id) { return !session(id); });
    reorder();
}

void State::upsert_session(SessionSummary summary) {
    put(std::move(summary));
    reorder();
}

void State::upsert_sessions(std::vector<SessionSummary> summaries) {
    std::unordered_map<std::string, std::size_t> index;
    for (std::size_t at = 0; at < sessions_.size(); ++at) index.emplace(sessions_[at].summary.id, at);
    for (auto& summary : summaries) {
        auto e = entry(std::move(summary));
        if (const auto it = index.find(e.summary.id); it != index.end()) {
            sessions_[it->second] = std::move(e);
        } else {
            index.emplace(e.summary.id, sessions_.size());
            sessions_.push_back(std::move(e));
        }
    }
    reorder();
}

void State::put(SessionSummary summary) {
    auto e = entry(std::move(summary));
    const auto it = std::ranges::find_if(sessions_, [&](const Entry& x) { return x.summary.id == e.summary.id; });
    if (it != sessions_.end()) *it = std::move(e);
    else sessions_.push_back(std::move(e));
}

State::Entry State::entry(SessionSummary s) {
    if (s.pinned) pins_.insert(s.id);
    else s.pinned = pins_.contains(s.id);
    std::string hay = s.title;
    if (s.project) hay += "\n" + *s.project;
    hay += "\n";
    hay += nodes_.name_of(s.node);
    return Entry{std::move(s), detail::lowercase(hay)};
}

void State::set_pins(std::vector<std::string> pins) {
    pins_ = {std::make_move_iterator(pins.begin()), std::make_move_iterator(pins.end())};
    for (auto& e : sessions_) e.summary.pinned = pins_.contains(e.summary.id);
    reorder();
}

void State::remove_session(std::string_view id) {
    std::erase_if(sessions_, [&](const Entry& e) { return e.summary.id == id; });
    if (const auto it = running_.find(id); it != running_.end()) running_.erase(it);
    reorder();
}

void State::set_running(std::string_view id, bool running) {
    if (running) running_.emplace(id);
    else if (const auto it = running_.find(id); it != running_.end()) running_.erase(it);
}

bool State::shows_running(std::string_view id) const {
    const auto* s = session(id);
    return s && row_running(*s);
}

const SessionSummary* State::session(std::string_view id) const {
    for (const auto& e : sessions_)
        if (e.summary.id == id) return &e.summary;
    return nullptr;
}

void State::set_pinned_projects(std::vector<std::string> pinned) {
    pinned_projects_ = {std::make_move_iterator(pinned.begin()), std::make_move_iterator(pinned.end())};
    refilter_projects();
}

void State::set_projects(std::vector<std::string> projects) {
    projects_ = std::move(projects);
    refilter_projects();
}

void State::set_nodes(std::vector<NodeStatus> nodes) {
    nodes_ = Nodes(std::move(nodes));
    auto old = std::move(sessions_);
    sessions_.clear();
    for (auto& e : old) sessions_.push_back(entry(std::move(e.summary)));
    reorder();
}

std::vector<const SessionSummary*> State::pinned() const {
    std::vector<const SessionSummary*> out;
    for (const auto i : pinned_order_) out.push_back(&sessions_[i].summary);
    return out;
}

std::vector<const SessionSummary*> State::recents() const {
    std::vector<const SessionSummary*> out;
    for (const auto i : recent_order_) out.push_back(&sessions_[i].summary);
    return out;
}

std::vector<std::string_view> State::projects() const {
    std::vector<std::string_view> out;
    for (const auto i : visible_projects_) out.emplace_back(projects_[i]);
    return out;
}

std::pair<std::vector<const SessionSummary*>, std::size_t> State::project_sessions(std::string_view project) const {
    std::vector<const SessionSummary*> shown;
    std::size_t more = 0;
    const auto visit = [&](std::size_t i) {
        const auto& s = sessions_[i].summary;
        if (s.project != project) return;
        if (shown.size() < PROJECT_SESSIONS) shown.push_back(&s);
        else ++more;
    };
    for (const auto i : pinned_order_) visit(i);
    for (const auto i : recent_order_) visit(i);
    return {std::move(shown), more};
}

std::optional<std::string_view> State::highlighted_project() const {
    if (shown_project) return *shown_project;
    if (active_project) return *active_project;
    if (active_) {
        if (const auto* s = session(*active_); s && s->project) return *s->project;
    }
    return std::nullopt;
}

State::Machine State::describe(const NodeFilter& filter) const {
    if (!filter.node) return {"All machines", "Recents from every machine", true};
    if (const auto* n = nodes_.get(*filter.node)) {
        const char* sub = n->local ? "This computer" : n->online ? "Peer · online" : "Peer · offline";
        return {n->name, sub, n->local || n->online};
    }
    return {std::string(nodes_.name_of(*filter.node)), "Unknown machine", false};
}

std::pair<std::size_t, std::size_t> State::tally(const NodeFilter& filter) const {
    std::size_t count = 0, waiting = 0;
    for (const auto& e : sessions_) {
        if (!filter.matches(e.summary.node)) continue;
        ++count;
        waiting += e.summary.waiting ? 1 : 0;
    }
    return {count, waiting};
}

// Resolves the listed machine (the user's pick, else this computer, else every machine) and
// rebuilds the Pinned and Recents orders. A different machine starts Recents at one page again.
void State::reorder() {
    NodeFilter filter;
    if (machine_) {
        filter = *machine_;
    } else {
        const auto& list = nodes_.list();
        const auto local = std::ranges::find_if(list, &NodeStatus::local);
        if (local != list.end()) filter.node = local->key;
    }
    if (filter != filter_) {
        filter_ = std::move(filter);
        recents_pages_ = 0;
    }
    const auto terms = split_whitespace(lower_query_);
    std::vector<std::size_t> pinned, recent;
    for (std::size_t i = 0; i < sessions_.size(); ++i) {
        const auto& [s, hay] = sessions_[i];
        if (!filter_.matches(s.node)) continue;
        if (!std::ranges::all_of(terms, [&](std::string_view t) { return hay.find(t) != std::string::npos; })) continue;
        (s.pinned ? pinned : recent).push_back(i);
    }
    const auto newest = [&](std::size_t a, std::size_t b) {
        const auto& x = sessions_[a].summary;
        const auto& y = sessions_[b].summary;
        if (x.updated != y.updated) return x.updated > y.updated;
        return x.id < y.id;
    };
    std::ranges::stable_sort(pinned, newest);
    std::ranges::stable_sort(recent, newest);
    pinned_order_ = std::move(pinned);
    recent_order_ = std::move(recent);
}

// Rebuilds the listed projects from the names, pins and search.
void State::refilter_projects() {
    const auto terms = split_whitespace(lower_query_);
    const auto is_pinned = [&](std::size_t i) { return pinned_projects_.contains(projects_[i]); };
    std::vector<std::size_t> order;
    for (std::size_t i = 0; i < projects_.size(); ++i)
        if (is_pinned(i)) order.push_back(i);
    std::ranges::stable_sort(order, [&](std::size_t a, std::size_t b) { return projects_[a] < projects_[b]; });
    for (std::size_t i = 0; i < projects_.size(); ++i)
        if (!is_pinned(i)) order.push_back(i);
    visible_projects_.clear();
    for (const auto i : order) {
        const auto name = detail::lowercase(projects_[i]);
        if (std::ranges::all_of(terms, [&](std::string_view t) { return name.find(t) != std::string::npos; })) visible_projects_.push_back(i);
    }
}

std::pair<Task, std::optional<Event>> State::update(Message message) {
    auto event = std::visit(
        Overloaded{
            [&](msg::Emit& m) -> std::optional<Event> { return std::move(m.intent); },
            [&](msg::Search& m) -> std::optional<Event> {
                lower_query_ = detail::lowercase(m.query);
                query_ = std::move(m.query);
                reorder();
                refilter_projects();
                return std::nullopt;
            },
            [&](msg::ToggleMachines) -> std::optional<Event> {
                machines_open_ = !machines_open_;
                return std::nullopt;
            },
            [&](msg::PickMachine& m) -> std::optional<Event> {
                machine_ = std::move(m.filter);
                machines_open_ = false;
                reorder();
                return std::nullopt;
            },
            [&](msg::OpenPeers) -> std::optional<Event> {
                machines_open_ = false;
                return is::OpenPeers{};
            },
            [&](msg::OpenSession& m) -> std::optional<Event> {
                active_ = m.id;
                return is::OpenSession{std::move(m.id)};
            },
            [&](msg::ToggleProjectPin& m) -> std::optional<Event> {
                const bool pinned = pinned_projects_.insert(m.project).second || !pinned_projects_.erase(m.project);
                refilter_projects();
                return is::SetProjectPinned{std::move(m.project), pinned};
            },
            [&](msg::TogglePin& m) -> std::optional<Event> {
                std::optional<Event> out;
                for (auto& e : sessions_) {
                    if (e.summary.id != m.id) continue;
                    e.summary.pinned = !e.summary.pinned;
                    if (e.summary.pinned) pins_.insert(m.id);
                    else pins_.erase(m.id);
                    out = is::SetPinned{m.id, e.summary.pinned};
                    break;
                }
                reorder();
                return out;
            },
            [&](msg::ShowMoreRecents) -> std::optional<Event> {
                ++recents_pages_;
                return std::nullopt;
            },
            [&](msg::ToggleMoveMenu& m) -> std::optional<Event> {
                if (move_menu_ == m.id) move_menu_.reset();
                else move_menu_ = std::move(m.id);
                return std::nullopt;
            },
            [&](msg::MoveTo& m) -> std::optional<Event> {
                move_menu_.reset();
                return is::MoveSession{std::move(m.id), std::move(m.project)};
            },
        },
        message);
    return {Task::none(), std::move(event)};
}

} // namespace loa::ui::shell::sidebar
