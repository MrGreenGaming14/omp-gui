#include "loa/app/view/settings/workflows.hpp"

#include <type_traits>
#include <utility>
#include <variant>

namespace loa::app::view::settings {

namespace domain = ::loa::settings::models::workflows;
namespace shown = ::loa::view::settings::workflows;

shown::Workflow to_view(const domain::Workflow& workflow) {
    shown::Workflow out{.role = workflow.role, .models = {}, .rules = shown::rules::None{}};
    out.models.reserve(workflow.models.size());
    for (const auto& model : workflow.models) out.models.push_back(model.to_string());
    const domain::Rules& rules = workflow.rules;
    switch (rules.kind) {
    case domain::Rules::Kind::Managed: {
        shown::ManagedFields fields{.description = rules.fields.description, .routing = {}, .steps = rules.fields.steps, .blocks = {}};
        for (const auto& entry : rules.fields.routing) fields.routing.push_back(shown::RoutingNote{entry.agent, entry.note});
        for (const auto& block : rules.fields.blocks) fields.blocks.push_back(shown::RuleBlock{block.heading, block.text});
        out.rules = shown::rules::Managed{rules.name, rules.allowed, rules.disallowed, rules.notes, std::move(fields)};
        break;
    }
    case domain::Rules::Kind::External: out.rules = shown::rules::External{rules.text}; break;
    case domain::Rules::Kind::HandWritten: out.rules = shown::rules::HandWritten{rules.text}; break;
    case domain::Rules::Kind::None: break;
    }
    return out;
}

::loa::settings::models::Result<domain::Workflow> from_view(shown::Workflow workflow) {
    domain::Workflow out{.role = std::move(workflow.role), .models = {}, .rules = domain::Rules::none()};
    out.models.reserve(workflow.models.size());
    for (const auto& model : workflow.models) {
        auto selector = ::loa::settings::models::selector::Selector::parse(model);
        if (!selector) return std::unexpected(std::move(selector.error()));
        out.models.push_back(std::move(*selector));
    }
    out.rules = std::visit(
        [](auto&& rules) -> domain::Rules {
            using R = std::decay_t<decltype(rules)>;
            if constexpr (std::is_same_v<R, shown::rules::Managed>) {
                domain::ManagedFields fields{.description = std::move(rules.fields.description), .routing = {}, .steps = std::move(rules.fields.steps), .blocks = {}};
                for (auto& entry : rules.fields.routing) fields.routing.push_back(domain::RoutingNote{std::move(entry.agent), std::move(entry.note)});
                for (auto& block : rules.fields.blocks) fields.blocks.push_back(domain::RuleBlock{std::move(block.heading), std::move(block.text)});
                return domain::Rules::managed(std::move(rules.name), std::move(rules.allowed), std::move(rules.disallowed), std::move(rules.notes),
                                              std::move(fields));
            } else if constexpr (std::is_same_v<R, shown::rules::External>) {
                return domain::Rules::external(std::move(rules.text));
            } else if constexpr (std::is_same_v<R, shown::rules::HandWritten>) {
                return domain::Rules::hand_written(std::move(rules.text));
            } else {
                return domain::Rules::none();
            }
        },
        std::move(workflow.rules));
    return out;
}

} // namespace loa::app::view::settings
