#include "loa/harness/kit/ledger.hpp"

namespace loa::harness::kit {

loa::contracts::Result<loa::core::NodeId> insert_root(const loa::core::DomWriter& w, std::string_view harness,
                                                      const loa::contracts::SessionConfig& config) {
    using namespace loa::core;
    Attrs attrs{
        {std::string(attr::ID), Value(config.session_id)},
        {std::string(attr::HARNESS), Value(harness)},
        {std::string(attr::NODE), Value(config.node)},
        {std::string(attr::CREATED), Value(w.now())},
        {std::string(attr::SCHEMA), Value(static_cast<std::int64_t>(SCHEMA_VERSION))},
        {std::string(attr::CWD), Value(config.cwd.string())},
    };
    const std::pair<std::string_view, const std::optional<std::string>*> optional[] = {
        {attr::PROJECT, &config.project}, {attr::MODEL, &config.model},
        {attr::MODEL_ROLE, &config.model_role}, {attr::EFFORT, &config.effort}};
    for (const auto& [name, value] : optional)
        if (*value) attrs.insert_or_assign(std::string(name), Value(**value));
    if (config.mode) attrs.insert_or_assign(std::string(attr::MODE), Value(*config.mode));
    auto node = w.insert(NodeId::DOCUMENT, ElementKind::Session, std::move(attrs));
    if (!node) return std::unexpected(loa::contracts::AdapterError(loa::contracts::AdapterError::Commit{std::move(node).error()}));
    return *node;
}

} // namespace loa::harness::kit
