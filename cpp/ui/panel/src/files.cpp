// The files tab state. Port of `crates/loa-ui-panel/src/files.rs` (`State`).

#include <algorithm>
#include <map>

#include "loa/ui/panel/files.hpp"

namespace loa::ui::panel::files {

namespace {

Node root_node() { return Node{.is_dir = true, .expanded = true}; }

std::string lower(std::string_view s) {
    std::string out(s);
    for (auto& c : out)
        if (c >= 'A' && c <= 'Z') c = static_cast<char>(c - 'A' + 'a');
    return out;
}

} // namespace

State::State(Path root, std::shared_ptr<const FileSystem> fs) : root_(std::move(root)), fs_(std::move(fs)) {
    nodes_.push_back(root_node());
    load(0);
    rebuild_rows();
}

void State::set_root(Path root) { *this = State(std::move(root), std::move(fs_)); }

std::vector<Path> State::visible() const {
    std::vector<Path> out;
    for (const auto& r : rows_)
        if (r.kind == Row::Kind::Node) out.push_back(nodes_[r.node].rel);
    return out;
}

void State::refresh() {
    auto old = std::exchange(nodes_, std::vector<Node>{root_node()});
    std::optional<Path> selected;
    if (selected_) selected = old[*selected_].rel;
    selected_.reset();
    std::map<Path, bool> dirs;
    for (std::size_t i = 1; i < old.size(); ++i)
        if (old[i].is_dir && (old[i].expanded || old[i].children)) dirs.emplace(old[i].rel, old[i].expanded);
    load(0);
    for (std::size_t i = 1; i < nodes_.size(); ++i) {
        if (nodes_[i].is_dir) {
            if (const auto it = dirs.find(nodes_[i].rel); it != dirs.end()) {
                nodes_[i].expanded = it->second;
                dirs.erase(it);
                load(i);
            }
        }
        if (selected && *selected == nodes_[i].rel) selected_ = i;
    }
    rebuild_rows();
}

bool State::shown(std::size_t node) const { return std::ranges::find(rows_, Row{Row::Kind::Node, node}) != rows_.end(); }

std::pair<Task, std::optional<Event>> State::update(Message message) {
    const auto i = std::get<msg::Click>(message).node;
    if (i == 0 || !shown(i)) return {Task::none(), std::nullopt};
    selected_ = i;
    if (!nodes_[i].is_dir) return {Task::none(), Event{loa::view::panel::files_intent::Open{root_ / nodes_[i].rel}}};
    if (!nodes_[i].expanded && !nodes_[i].children) load(i);
    nodes_[i].expanded = !nodes_[i].expanded;
    rebuild_rows();
    return {Task::none(), std::nullopt};
}

// Lists a directory node and appends its children, directories first, by name ignoring case.
void State::load(std::size_t dir) {
    const auto rel = nodes_[dir].rel;
    auto listed = fs_->list(rel.empty() ? root_ : root_ / rel);
    if (!listed) {
        nodes_[dir].children.reset();
        nodes_[dir].error = std::move(listed.error());
        return;
    }
    auto& entries = *listed;
    std::ranges::sort(entries, [](const Entry& a, const Entry& b) {
        if (a.is_dir != b.is_dir) return a.is_dir;
        const auto la = lower(a.name), lb = lower(b.name);
        if (la != lb) return la < lb;
        return a.name < b.name;
    });
    const auto first = nodes_.size();
    const auto depth = nodes_[dir].depth + 1;
    for (auto& e : entries) {
        auto path = rel / e.name;
        nodes_.push_back(Node{.depth = depth, .name = std::move(e.name), .rel = std::move(path), .is_dir = e.is_dir});
    }
    nodes_[dir].children = std::pair{first, nodes_.size()};
    nodes_[dir].error.reset();
}

void State::rebuild_rows() {
    rows_.clear();
    std::vector<std::size_t> stack{0};
    while (!stack.empty()) {
        const auto i = stack.back();
        stack.pop_back();
        const auto& n = nodes_[i];
        if (i != 0) rows_.push_back({Row::Kind::Node, i});
        if (n.is_dir && n.expanded) {
            if (n.error) rows_.push_back({Row::Kind::Error, i});
            if (n.children)
                for (auto c = n.children->second; c-- > n.children->first;) stack.push_back(c);
        }
    }
    if (selected_ && !shown(*selected_)) selected_.reset();
}

} // namespace loa::ui::panel::files
