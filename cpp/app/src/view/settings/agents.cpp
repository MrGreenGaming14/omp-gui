#include "loa/app/view/settings/agents.hpp"

#include <algorithm>
#include <array>
#include <string_view>
#include <utility>

namespace loa::app::view::settings {

namespace {

namespace domain = ::loa::settings::models::agents;
namespace shown = ::loa::view::settings::agents;

shown::ModelSpec model_to_view(domain::ModelSpec model) {
    switch (model.kind) {
    case domain::ModelSpec::Kind::One: return shown::ModelSpec{model.items.empty() ? std::string() : std::move(model.items.front())};
    case domain::ModelSpec::Kind::Chain: break;
    }
    return shown::ModelSpec{std::move(model.items)};
}

domain::ModelSpec model_from_view(shown::ModelSpec model) {
    if (auto* one = std::get_if<std::string>(&model.value)) return domain::ModelSpec::one(std::move(*one));
    return domain::ModelSpec::chain(std::get<std::vector<std::string>>(std::move(model.value)));
}

// The form's field names; the view borrows them for the life of the program.
constexpr std::array<std::string_view, 7> FIELDS{"name", "description", "model", "thinkingLevel", "tools", "spawns", "form"};

} // namespace

shown::AgentFields fields_to_view(const domain::AgentDefinition& def) {
    std::optional<shown::ModelSpec> model;
    if (auto m = def.model()) model = model_to_view(std::move(*m));
    std::optional<std::string> thinking_level;
    if (auto level = def.thinking_level()) thinking_level = std::string(*level);
    return shown::AgentFields{
        .name = std::string(def.name()),
        .description = std::string(def.description()),
        .model = std::move(model),
        .thinking_level = std::move(thinking_level),
        .tools = def.tools(),
        .spawns = def.spawns(),
        .body = def.body(),
    };
}

::loa::settings::models::Result<domain::AgentDefinition> fields_from_view(const domain::AgentDefinition& base, shown::AgentFields fields) {
    const shown::AgentFields before = fields_to_view(base);
    domain::AgentDefinition def = base;
    if (fields.name != before.name) {
        if (auto r = def.set_name(fields.name); !r) return std::unexpected(std::move(r.error()));
    }
    if (fields.description != before.description) {
        if (auto r = def.set_description(fields.description); !r) return std::unexpected(std::move(r.error()));
    }
    if (fields.model != before.model) {
        std::optional<domain::ModelSpec> model;
        if (fields.model) model = model_from_view(std::move(*fields.model));
        if (auto r = def.set_model(std::move(model)); !r) return std::unexpected(std::move(r.error()));
    }
    if (fields.thinking_level != before.thinking_level) {
        std::optional<std::string_view> level;
        if (fields.thinking_level) level = *fields.thinking_level;
        if (auto r = def.set_thinking_level(level); !r) return std::unexpected(std::move(r.error()));
    }
    if (fields.tools != before.tools) {
        if (auto r = def.set_tools(std::move(fields.tools)); !r) return std::unexpected(std::move(r.error()));
    }
    if (fields.spawns != before.spawns) {
        if (auto r = def.set_spawns(std::move(fields.spawns)); !r) return std::unexpected(std::move(r.error()));
    }
    if (fields.body != before.body) {
        def.set_body(std::move(fields.body));
    }
    return def;
}

shown::AgentEntry entry_to_view(const domain::AgentFile& file) {
    const shown::AgentSource source = file.source == domain::AgentSource::Project ? shown::AgentSource::Project : shown::AgentSource::User;
    return shown::AgentEntry{.path = file.path, .source = source, .enabled = file.enabled, .fields = fields_to_view(file.definition)};
}

shown::FieldError field_error_to_view(domain::FieldError error) {
    const auto* field = std::ranges::find(FIELDS, std::string_view(error.field));
    return shown::FieldError{.field = field == FIELDS.end() ? FIELDS.back() : *field, .message = std::move(error.message)};
}

} // namespace loa::app::view::settings
