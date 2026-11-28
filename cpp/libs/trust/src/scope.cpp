#include "loa/trust/scope.hpp"

#include <algorithm>
#include <cctype>
#include <format>
#include <limits>

#include "loa/core/dom.hpp"
#include "loa/core/schema.hpp"

namespace loa::trust {
namespace {

constexpr std::array<std::string_view, 7> ACTION_NAMES = {"view", "suggest", "drive", "fork", "read", "write", "exec"};

constexpr Action DRIVE_IMPLIES[] = {Action::suggest, Action::view};
constexpr Action VIEW_ONLY[] = {Action::view};
constexpr Action READ_ONLY[] = {Action::read};

constexpr Action SESSION_ACTIONS[] = {Action::view, Action::suggest, Action::drive, Action::fork};
constexpr Action SSH_ACTIONS[] = {Action::read, Action::write, Action::exec};
constexpr Action SURFACE_ACTIONS[] = {Action::view, Action::drive};
constexpr Action SANDBOX_ACTIONS[] = {Action::exec};
constexpr Action FOLDER_ACTIONS[] = {Action::read, Action::write};

} // namespace

std::string_view as_str(Action action) noexcept { return ACTION_NAMES[static_cast<std::size_t>(action)]; }

std::optional<Action> parse_action(std::string_view text) noexcept {
    for (std::size_t i = 0; i < ACTION_NAMES.size(); ++i)
        if (ACTION_NAMES[i] == text) return static_cast<Action>(i);
    return std::nullopt;
}

std::span<const Action> implied(Action action) noexcept {
    switch (action) {
        case Action::drive: return DRIVE_IMPLIES;
        case Action::suggest:
        case Action::fork: return VIEW_ONLY;
        case Action::write: return READ_ONLY;
        case Action::view:
        case Action::read:
        case Action::exec: return {};
    }
    return {};
}

const std::array<ResourceKind, 8> KINDS = {{
    {"session", SESSION_ACTIONS, "session_right", "session(#0)"},
    {"prompt", SESSION_ACTIONS, "prompt_right", "session(#0), turn(#1)"},
    {"project", SESSION_ACTIONS, "project_right", "project(#0)"},
    {"future sessions", SESSION_ACTIONS, "future_right", "created($c), $c >= #0"},
    {"ssh", SSH_ACTIONS, "ssh_right", "ssh_host(#0)"},
    {"surface", SURFACE_ACTIONS, "surface_right", "surface(#0)"},
    {"sandbox", SANDBOX_ACTIONS, "sandbox_right", "sandbox(#0)"},
    {"folder", FOLDER_ACTIONS, "folder_right", "folder(#0)"},
}};

std::size_t ResourceKind::arity() const noexcept { return static_cast<std::size_t>(std::ranges::count(covers, '#')); }

std::string ResourceKind::vars() const {
    std::string out;
    for (std::size_t i = 0; i < arity(); ++i) out += std::format("$v{}, ", i);
    return out;
}

bool ResourceKind::takes(Action action) const noexcept { return std::ranges::find(actions, action) != actions.end(); }

const ResourceKind& kind_of(const Resource& resource) noexcept { return KINDS[resource.index()]; }

Result<Scope> Scope::make(Resource resource, std::span<const Action> actions) {
    const auto& kind = kind_of(resource);
    ActionSet set;
    for (const auto action : actions) {
        set.insert(action);
        for (const auto more : implied(action))
            if (kind.takes(more)) set.insert(more);
    }
    Scope scope{std::move(resource), std::move(set)};
    if (auto valid = scope.validate(); !valid) return std::unexpected(std::move(valid.error()));
    return scope;
}

Result<void> Scope::validate() const {
    const auto& kind = kind_of(resource);
    if (actions.empty()) return std::unexpected(TrustError::no_actions());
    for (const auto action : actions)
        if (!kind.takes(action)) return std::unexpected(TrustError::action(std::string(kind.name), std::string(as_str(action))));
    if (const auto* prompt = std::get_if<resource::Prompt>(&resource)) {
        if (prompt->turn.value > static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max()))
            return std::unexpected(TrustError::node_id(prompt->turn.value));
    }
    if (const auto* folder = std::get_if<resource::Folder>(&resource)) {
        const auto ancestors = folder_ancestors(folder->path);
        if (!ancestors || ancestors->empty() || ancestors->back() != folder->path)
            return std::unexpected(TrustError::folder_path(folder->path));
    }
    return {};
}

// Mirrors Rust `Path::components`: repeated separators and inner `.` vanish, `..` fails.
std::optional<std::vector<std::string>> folder_ancestors(std::string_view path) {
#ifdef _WIN32
    const auto is_sep = [](char c) { return c == '\\' || c == '/'; };
    constexpr char SEP = '\\';
    std::string current;
    std::vector<std::string> out;
    std::size_t pos = 0;
    // A drive prefix comes before its root: the first folder is both.
    if (path.size() >= 2 && path[1] == ':' && std::isalpha(static_cast<unsigned char>(path[0]))) {
        current.assign(path.substr(0, 2));
        pos = 2;
    }
    const bool prefixed = !current.empty();
    const bool rooted = pos < path.size() && is_sep(path[pos]);
    if (rooted) {
        current += SEP;
        out.push_back(current);
    }
#else
    const auto is_sep = [](char c) { return c == '/'; };
    constexpr char SEP = '/';
    std::string current;
    std::vector<std::string> out;
    std::size_t pos = 0;
    const bool rooted = !path.empty() && path[0] == '/';
    if (rooted) {
        current = "/";
        out.push_back(current);
    }
#endif
    bool first = true;
    while (pos < path.size()) {
        while (pos < path.size() && is_sep(path[pos])) ++pos;
        if (pos >= path.size()) break;
        auto end = pos;
        while (end < path.size() && !is_sep(path[end])) ++end;
        const auto part = path.substr(pos, end - pos);
        pos = end;
        if (part == "..") return std::nullopt;
        // A `.` counts only at the start of a relative path.
        if (part == ".") {
            if (first && !rooted) return std::nullopt;
            first = false;
            continue;
        }
        first = false;
        if (!current.empty() && !is_sep(current.back())) current += SEP;
        current += part;
        out.push_back(current);
    }
#ifdef _WIN32
    if (!(prefixed && rooted)) return std::nullopt;
#else
    if (!rooted) return std::nullopt;
#endif
    return out;
}

std::optional<SessionFacts> SessionFacts::from_dom(const core::Dom& dom) {
    const auto* root = dom.root();
    if (root == nullptr) return std::nullopt;
    const auto id = root->str_attr(core::attr::ID);
    const auto created = root->int_attr(core::attr::CREATED);
    if (!id || !created) return std::nullopt;
    std::optional<std::string> project;
    if (const auto p = root->str_attr(core::attr::PROJECT)) project.emplace(*p);
    return SessionFacts{std::string(*id), *created, std::move(project)};
}

std::optional<core::NodeId> turn_of(const core::Dom& dom, core::NodeId node) {
    const auto* found = dom.get(node);
    if (found == nullptr) return std::nullopt;
    if (found->kind == core::ElementKind::Turn) return node;
    for (const auto* ancestor : dom.ancestors(node))
        if (ancestor->kind == core::ElementKind::Turn) return ancestor->id;
    return std::nullopt;
}

std::string Request::debug() const {
    struct Visit {
        std::string operator()(const target::Session& s) const {
            auto out = std::format("Session {{ session: {}", s.session ? s.session->id : std::string("?"));
            if (s.turn) out += std::format(", turn: {}", s.turn->value);
            return out + " }";
        }
        std::string operator()(const target::Ssh& s) const { return std::format("Ssh {{ host: \"{}\" }}", s.host); }
        std::string operator()(const target::Surface& s) const { return std::format("Surface {{ uri: \"{}\" }}", s.uri); }
        std::string operator()(const target::Sandbox&) const { return "Sandbox"; }
        std::string operator()(const target::Folder& s) const { return std::format("Folder {{ path: \"{}\" }}", s.path); }
    };
    return std::format("Request {{ action: {}, target: {} }}", as_str(action), std::visit(Visit{}, target));
}

} // namespace loa::trust
