// The workflows screen's reducer. Port of `crates/loa-ui-settings/src/workflows/mod.rs`.

#include "loa/ui/settings/workflows/state.hpp"

#include <algorithm>
#include <format>

#include "strings.hpp"

namespace loa::ui::settings::workflows {

namespace {

template <class... F> struct overloaded : F... { using F::operator()...; };
template <class... F> overloaded(F...) -> overloaded<F...>;

Rules empty_managed() { return Rules{vs::rules::Managed{}}; }

} // namespace

std::string_view label(Access a) {
    switch (a) {
    case Access::Allow: return "Allow";
    case Access::Disallow: return "Disallow";
    case Access::Unspecified: break;
    }
    return "Not set";
}

State::State() : rules_(empty_managed()) {}

std::optional<Intent> State::update(Message m) {
    if (auto* l = std::get_if<Local>(&m)) return reduce(std::move(*l));
    auto e = std::get<Intent>(std::move(m));
    if (guard(e)) return e;
    return std::nullopt;
}

bool State::guard(const Intent& e) const {
    if (std::holds_alternative<vs::intent::Close>(e)) return true;
    return std::holds_alternative<vs::intent::Reload>(e) && error_ && std::holds_alternative<vs::error::ChangedOnDisk>(*error_);
}

std::optional<std::string_view> State::selected() const {
    if (!previous_role_) return std::nullopt;
    return std::string_view(*previous_role_);
}

void State::new_workflow() {
    previous_role_.reset();
    role_.clear();
    models_.clear();
    rules_ = empty_managed();
    text_.clear();
    text_dirty_ = false;
    block_text_.clear();
    errors_.clear();
    error_.reset();
    confirming_delete_ = false;
}

bool State::edit(std::string_view role) {
    const auto it = std::ranges::find_if(entries_, [&](const Workflow& w) { return w.role == role; });
    if (it == entries_.end()) return false;
    Workflow entry = *it;
    previous_role_ = entry.role;
    role_ = entry.role;
    models_.clear();
    for (const auto& m : entry.models) models_ += (models_.empty() ? "" : ", ") + m;
    set_rules(std::move(entry.rules));
    errors_.clear();
    error_.reset();
    confirming_delete_ = false;
    return true;
}

void State::set_rules(Rules rules) {
    text_ = std::visit(overloaded{[](const vs::rules::Managed& r) { return r.notes; }, [](const vs::rules::HandWritten& r) { return r.text; },
                                  [](const vs::rules::External& r) { return r.text; }, [](const vs::rules::None&) { return std::string(); }},
                       rules);
    block_text_.clear();
    if (const auto* m = std::get_if<vs::rules::Managed>(&rules))
        for (const auto& b : m->fields.blocks) block_text_.push_back({b.text, false});
    rules_ = std::move(rules);
    text_dirty_ = false;
}

Draft State::draft() const {
    Rules rules = rules_;
    if (text_dirty_) {
        std::visit(overloaded{[&](vs::rules::Managed& r) { r.notes = text_; }, [&](vs::rules::HandWritten& r) { r.text = text_; },
                              [&](vs::rules::External& r) { r.text = text_; }, [](vs::rules::None&) {}},
                   rules);
    }
    if (auto* m = std::get_if<vs::rules::Managed>(&rules)) {
        const auto n = std::min(m->fields.blocks.size(), block_text_.size());
        for (std::size_t i = 0; i < n; ++i)
            if (block_text_[i].dirty) m->fields.blocks[i].text = block_text_[i].content;
    }
    Workflow w;
    w.role = str::trimmed(role_);
    std::string_view rest = models_;
    while (true) {
        const auto comma = rest.find(',');
        const auto part = str::trim(rest.substr(0, comma));
        if (!part.empty()) w.models.emplace_back(part);
        if (comma == std::string_view::npos) break;
        rest.remove_prefix(comma + 1);
    }
    w.rules = std::move(rules);
    return Draft{std::move(w)};
}

Access State::access(std::string_view agent) const {
    const auto* m = std::get_if<vs::rules::Managed>(&rules_);
    if (!m) return Access::Unspecified;
    if (std::ranges::find(m->allowed, agent) != m->allowed.end()) return Access::Allow;
    if (std::ranges::find(m->disallowed, agent) != m->disallowed.end()) return Access::Disallow;
    return Access::Unspecified;
}

std::optional<Intent> State::reduce(Local m) {
    namespace l = local;
    using R = std::optional<Intent>;
    auto* managed = std::get_if<vs::rules::Managed>(&rules_);
    auto erase_at = [](auto& v, std::size_t i) { v.erase(v.begin() + static_cast<std::ptrdiff_t>(i)); };
    return std::visit(
        overloaded{
            [&](l::Open& a) -> R {
                if (edit(a.role)) return Intent{vs::intent::Open{std::move(a.role)}};
                return std::nullopt;
            },
            [&](l::New&) -> R {
                new_workflow();
                return Intent{vs::intent::New{}};
            },
            [&](l::Role& a) -> R {
                if (!std::holds_alternative<vs::rules::External>(rules_)) role_ = std::move(a.value);
                return std::nullopt;
            },
            [&](l::Models& a) -> R {
                models_ = std::move(a.value);
                return std::nullopt;
            },
            [&](l::Name& a) -> R {
                if (managed) managed->name = std::move(a.value);
                return std::nullopt;
            },
            [&](l::SetAccess& a) -> R {
                if (!managed) return std::nullopt;
                std::erase(managed->allowed, a.agent);
                std::erase(managed->disallowed, a.agent);
                if (a.access != Access::Allow) std::erase_if(managed->fields.routing, [&](const RoutingNote& n) { return n.agent == a.agent; });
                if (a.access == Access::Allow) managed->allowed.push_back(std::move(a.agent));
                else if (a.access == Access::Disallow) managed->disallowed.push_back(std::move(a.agent));
                return std::nullopt;
            },
            [&](l::Description& a) -> R {
                if (managed) managed->fields.description = std::move(a.value);
                return std::nullopt;
            },
            [&](l::Routing& a) -> R {
                if (!managed || std::ranges::find(managed->allowed, a.agent) == managed->allowed.end()) return std::nullopt;
                auto& routing = managed->fields.routing;
                const auto it = std::ranges::find_if(routing, [&](const RoutingNote& n) { return n.agent == a.agent; });
                if (str::trim(a.note).empty()) std::erase_if(routing, [&](const RoutingNote& n) { return n.agent == a.agent; });
                else if (it != routing.end()) it->note = std::move(a.note);
                else routing.push_back(RoutingNote{std::move(a.agent), std::move(a.note)});
                return std::nullopt;
            },
            [&](l::AddStep&) -> R {
                if (managed) managed->fields.steps.emplace_back();
                return std::nullopt;
            },
            [&](l::Step& a) -> R {
                if (managed && a.index < managed->fields.steps.size()) managed->fields.steps[a.index] = std::move(a.value);
                return std::nullopt;
            },
            [&](l::RemoveStep& a) -> R {
                if (managed && a.index < managed->fields.steps.size()) erase_at(managed->fields.steps, a.index);
                return std::nullopt;
            },
            [&](l::MoveStepUp& a) -> R {
                auto* steps = managed ? &managed->fields.steps : nullptr;
                if (steps && a.index > 0 && a.index < steps->size()) std::swap((*steps)[a.index], (*steps)[a.index - 1]);
                return std::nullopt;
            },
            [&](l::MoveStepDown& a) -> R {
                auto* steps = managed ? &managed->fields.steps : nullptr;
                if (steps && !steps->empty() && a.index < steps->size() - 1) std::swap((*steps)[a.index], (*steps)[a.index + 1]);
                return std::nullopt;
            },
            [&](l::AddBlock&) -> R {
                if (managed) {
                    managed->fields.blocks.emplace_back();
                    block_text_.emplace_back();
                }
                return std::nullopt;
            },
            [&](l::BlockHeading& a) -> R {
                if (managed && a.index < managed->fields.blocks.size()) managed->fields.blocks[a.index].heading = std::move(a.value);
                return std::nullopt;
            },
            [&](l::BlockText& a) -> R {
                if (a.index < block_text_.size()) block_text_[a.index] = {std::move(a.text), true};
                return std::nullopt;
            },
            [&](l::RemoveBlock& a) -> R {
                if (managed && a.index < managed->fields.blocks.size()) {
                    erase_at(managed->fields.blocks, a.index);
                    erase_at(block_text_, a.index);
                }
                return std::nullopt;
            },
            [&](l::CopyFrom& a) -> R {
                const auto it = std::ranges::find_if(entries_, [&](const Workflow& w) {
                    return w.role == a.role && !std::holds_alternative<vs::rules::None>(w.rules);
                });
                if (it == entries_.end()) return std::nullopt;
                const Workflow entry = *it;
                Rules rules;
                if (const auto* src = std::get_if<vs::rules::Managed>(&entry.rules)) {
                    auto copy = *src;
                    copy.name = std::format("{} copy", src->name);
                    rules = std::move(copy);
                } else {
                    const auto* ext = std::get_if<vs::rules::External>(&entry.rules);
                    vs::rules::Managed copy;
                    copy.name = std::format("{} copy", a.role);
                    copy.notes = ext ? ext->text : std::get<vs::rules::HandWritten>(entry.rules).text;
                    rules = std::move(copy);
                }
                new_workflow();
                for (const auto& model : entry.models) models_ += (models_.empty() ? "" : ", ") + model;
                set_rules(std::move(rules));
                return std::nullopt;
            },
            [&](l::Text& a) -> R {
                text_ = std::move(a.text);
                text_dirty_ = true;
                return std::nullopt;
            },
            [&](l::Save&) -> R { return Intent{vs::intent::Save{draft(), previous_role_}}; },
            [&](l::Delete&) -> R {
                confirming_delete_ = previous_role_.has_value();
                return std::nullopt;
            },
            [&](l::CancelDelete&) -> R {
                confirming_delete_ = false;
                return std::nullopt;
            },
            [&](l::ConfirmDelete&) -> R {
                if (!confirming_delete_) return std::nullopt;
                confirming_delete_ = false;
                if (!previous_role_) return std::nullopt;
                return Intent{vs::intent::Delete{*previous_role_}};
            },
        },
        m);
}

} // namespace loa::ui::settings::workflows
