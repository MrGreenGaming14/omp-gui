#include "loa/projects/source.hpp"

#include <algorithm>

#include "loa/core/schema.hpp"
#include "loa/projects/text.hpp"
#include "loa/runtime/text.hpp"

namespace loa::projects {

namespace attr = core::attr;

std::optional<SessionInfo> SessionInfo::from_dom(const core::Dom& dom) {
    const auto* root = dom.root();
    if (!root) return std::nullopt;
    std::optional<std::string> title;
    if (auto t = root->str_attr(attr::TITLE); t && !text::trim(*t).empty()) {
        title = std::string(text::trim(*t));
    } else {
        const auto user = core::as_str(core::Role::User);
        for (const auto& n : dom.iter()) {
            if (n.kind == core::ElementKind::Message && n.str_attr(attr::ROLE) == user) {
                auto line = runtime::text::first_line(n.text, 60);
                if (!line.empty()) title = std::move(line);
                break;
            }
        }
    }
    auto id = root->str_attr(attr::ID);
    if (!id) return std::nullopt;
    auto project = session_project(dom);
    if (!project) return std::nullopt;
    return SessionInfo{std::string(*id), std::move(*project), std::string(root->str_attr(attr::NODE).value_or("")),
                       root->int_attr(attr::CREATED).value_or(0), std::move(title)};
}

std::optional<std::string> session_project(const core::Dom& dom) {
    const auto* root = dom.root();
    if (!root) return std::nullopt;
    auto project = root->str_attr(attr::PROJECT);
    if (!project) return std::nullopt;
    auto trimmed = text::trim(*project);
    if (trimmed.empty()) return std::nullopt;
    return std::string(trimmed);
}

Result<std::vector<SessionInfo>> visible_sessions(const SessionSource& source, const Access& access,
                                                  std::string_view project) {
    auto sessions = source.list(project);
    if (!sessions) return std::unexpected(sessions.error());
    std::erase_if(*sessions, [&](const SessionInfo& s) { return s.project != project || !access.can_open(s); });
    std::ranges::stable_sort(*sessions, [](const SessionInfo& a, const SessionInfo& b) {
        if (a.created != b.created) return a.created > b.created;
        return a.id < b.id;
    });
    return sessions;
}

Result<std::shared_ptr<const core::Dom>> open_visible(const SessionSource& source, const Access& access,
                                                      std::string_view project, std::string_view id) {
    auto visible = visible_sessions(source, access, project);
    if (!visible) return std::unexpected(visible.error());
    if (std::ranges::none_of(*visible, [&](const SessionInfo& s) { return s.id == id; }))
        return std::unexpected(Error::session_not_found(std::string(id)));
    auto dom = source.load(id);
    if (!dom) return std::unexpected(dom.error());
    if (!*dom) return std::unexpected(Error::session_not_found(std::string(id)));
    return dom;
}

} // namespace loa::projects
