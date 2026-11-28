#include "areas/models/session.hpp"

#include <string>

#include "loa/core/schema.hpp"

namespace loa::app::areas::models {

std::optional<std::pair<core::NodeId, loa::view::settings::models::SessionModel>> session_model(const core::Dom& dom) {
    const auto* root = dom.root();
    if (!root || root->kind != core::ElementKind::Session) return std::nullopt;
    auto read = [&](std::string_view name) -> std::optional<std::string> {
        auto value = root->str_attr(name);
        return value ? std::optional<std::string>(*value) : std::nullopt;
    };
    return std::pair{root->id, loa::view::settings::models::SessionModel{read(core::attr::MODEL), read(core::attr::MODEL_ROLE)}};
}

std::optional<core::Draft> role_draft(const core::Dom& dom, std::optional<std::string_view> role) {
    auto current = session_model(dom);
    if (!current) return std::nullopt;
    const auto& [session, model] = *current;
    const auto old = model.role ? std::optional<std::string_view>(*model.role) : std::nullopt;
    if (role == old) return std::nullopt;
    if (role) return core::Draft::set(session, std::string(core::attr::MODEL_ROLE), std::string(*role));
    return core::Draft::clear(session, std::string(core::attr::MODEL_ROLE));
}

} // namespace loa::app::areas::models
