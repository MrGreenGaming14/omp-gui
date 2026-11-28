// The agents screen's reducer. Port of `crates/loa-ui-settings/src/agents/mod.rs`.

#include "loa/ui/settings/agents/state.hpp"

#include <algorithm>
#include <format>

#include "strings.hpp"

namespace loa::ui::settings::agents {

namespace {

template <class... F> struct overloaded : F... { using F::operator()...; };
template <class... F> overloaded(F...) -> overloaded<F...>;

bool contains(const std::vector<std::string>& v, std::string_view s) { return std::ranges::find(v, s) != v.end(); }

// `selected` in the order of `known`, unknown entries last; `std::nullopt` when nothing is selected (the key is
// left out and omp's default applies).
std::optional<std::vector<std::string>> ordered(const std::vector<std::string>& selected, const std::vector<std::string>& known) {
    std::vector<std::string> out;
    for (const auto& item : known)
        if (contains(selected, item)) out.push_back(item);
    for (const auto& item : selected)
        if (!contains(known, item)) out.push_back(item);
    if (out.empty()) return std::nullopt;
    return out;
}

std::string copy_label(std::string_view name) { return std::format("Copy of {}", name); }

std::string label_of(const std::optional<ModelSpec>& model) {
    if (!model) return std::string(MODEL_DEFAULT);
    std::string out;
    for (const auto& e : model->entries()) {
        if (!out.empty()) out += ", ";
        out += e;
    }
    return out;
}

void toggle(std::vector<std::string>& list, std::string item, bool on) {
    if (on) {
        if (!contains(list, item)) list.push_back(std::move(item));
    } else {
        std::erase(list, item);
    }
}

} // namespace

State::State() = default;

std::optional<Intent> State::update(Message m) {
    if (auto* l = std::get_if<Local>(&m)) return reduce(std::move(*l));
    auto e = std::get<Intent>(std::move(m));
    if (guard(e)) return e;
    return std::nullopt;
}

bool State::guard(const Intent& e) const { return std::holds_alternative<vs::intent::Close>(e); }

void State::set_agents(std::vector<AgentEntry> entries) {
    entries_ = std::move(entries);
    if (template_.index) {
        if (*template_.index < entries_.size()) template_.text = copy_label(entries_[*template_.index].fields.name);
        else template_ = Template::blank();
    }
}

void State::set_choices(Choices choices) { choices_ = std::move(choices); }

std::optional<std::string> State::pending_question() const {
    if (!pending_) return std::nullopt;
    return std::visit(overloaded{[](const Replace& r) { return std::format("Replace {}?", r.draft.fields.name); },
                                 [](const Delete& d) { return std::format("Delete {}?", d.name); }},
                      *pending_);
}

std::optional<std::string_view> State::pending_yes() const {
    if (!pending_) return std::nullopt;
    return std::holds_alternative<Replace>(*pending_) ? std::string_view("Replace") : std::string_view("Delete");
}

std::optional<bool> State::is_enabled(const std::filesystem::path& path) const {
    for (const auto& e : entries_)
        if (e.path == path) return e.enabled;
    return std::nullopt;
}

bool State::edit_path(const std::filesystem::path& path) {
    for (std::size_t i = 0; i < entries_.size(); ++i)
        if (entries_[i].path == path) {
            edit(i);
            return true;
        }
    return false;
}

void State::ask_replace(Draft draft, std::filesystem::path path) { pending_ = Replace{std::move(draft), std::move(path)}; }

void State::set_errors(std::vector<FieldError> errors) { errors_ = std::move(errors); }

void State::set_error(std::string_view field, std::string message) {
    pending_.reset();
    std::erase_if(errors_, [&](const FieldError& e) { return e.field == field; });
    errors_.push_back(FieldError{field, std::move(message)});
}

void State::new_agent() { fill(Template::blank(), std::nullopt, std::nullopt); }

void State::edit(std::size_t index) {
    if (index >= entries_.size()) return;
    fill(Template::blank(), index, std::nullopt);
    editing_ = entries_[index].path;
}

void State::copy(std::size_t index) {
    if (index >= entries_.size()) return;
    const auto& name = entries_[index].fields.name;
    fill(Template{index, copy_label(name)}, index, name + "-copy");
}

std::vector<std::string_view> State::agent_names() const {
    std::vector<std::string_view> out;
    for (const auto& e : entries_) out.emplace_back(e.fields.name);
    return out;
}

std::vector<Template> State::templates() const {
    std::vector<Template> out{Template::blank()};
    for (std::size_t i = 0; i < entries_.size(); ++i) out.push_back(Template{i, copy_label(entries_[i].fields.name)});
    return out;
}

AgentFields State::fields() const {
    return AgentFields{str::trimmed(name_), str::trimmed(description_), model_, thinking_, ordered(tools_on_, choices_.tools),
                       ordered(spawns_on_, other_agents()), body_};
}

Draft State::draft() const { return Draft{fields(), base_, editing_}; }

void State::fill(Template t, std::optional<std::size_t> base, std::optional<std::string> name) {
    const AgentEntry* entry = base && *base < entries_.size() ? &entries_[*base] : nullptr;
    AgentFields f = entry ? entry->fields : AgentFields{};
    base_ = entry ? std::optional(entry->path) : std::nullopt;
    template_ = std::move(t);
    name_ = name ? std::move(*name) : std::move(f.name);
    description_ = std::move(f.description);
    model_ = std::move(f.model);
    thinking_ = std::move(f.thinking_level);
    tools_on_ = f.tools.value_or(std::vector<std::string>{});
    spawns_on_ = f.spawns.value_or(std::vector<std::string>{});
    body_ = std::move(f.body);
    errors_.clear();
    show_errors_ = false;
    editing_.reset();
    pending_.reset();
    open_picker_.reset();
}

std::vector<std::string> State::other_agents() const {
    std::vector<std::string> names;
    const auto self = str::trim(name_);
    for (const auto& e : entries_) {
        const auto& n = e.fields.name;
        if (n != self && !contains(names, n)) names.push_back(n);
    }
    return names;
}

std::optional<ModelSpec> State::model_choice(std::string_view label) const {
    if (label == MODEL_DEFAULT) return std::nullopt;
    if (model_ && label_of(model_) == label) return model_;
    return ModelSpec{std::string(label)};
}

std::vector<std::string> State::model_options() const {
    std::vector<std::string> options{std::string(MODEL_DEFAULT)};
    options.insert(options.end(), choices_.models.begin(), choices_.models.end());
    auto current = label_of(model_);
    if (!contains(options, current)) options.push_back(std::move(current));
    return options;
}

std::string State::model_label() const { return label_of(model_); }

std::vector<std::string> State::thinking_options() const {
    std::vector<std::string> levels{std::string(THINKING_DEFAULT)};
    levels.insert(levels.end(), choices_.thinking_levels.begin(), choices_.thinking_levels.end());
    return levels;
}

std::optional<Intent> State::reduce(Local m) {
    namespace l = local;
    using R = std::optional<Intent>;
    // Messages that answer for themselves; the rest are edits that fall through to the check below.
    std::optional<R> answered = std::visit(
        overloaded{
            [&](l::Save&) -> std::optional<R> {
                pending_.reset();
                show_errors_ = true;
                return R{vs::intent::Save{draft()}};
            },
            [&](l::Toggle&) -> std::optional<R> {
                if (!editing_) return R{};
                return R{vs::intent::Toggle{*editing_}};
            },
            [&](l::Delete&) -> std::optional<R> {
                if (editing_) pending_ = Delete{name_, *editing_};
                return R{};
            },
            [&](l::Cancel&) -> std::optional<R> {
                pending_.reset();
                return R{};
            },
            [&](l::Confirm&) -> std::optional<R> {
                if (!pending_) return R{};
                auto p = std::move(*pending_);
                pending_.reset();
                if (auto* r = std::get_if<Replace>(&p)) return R{vs::intent::Replace{std::move(r->draft), std::move(r->path)}};
                return R{vs::intent::Delete{std::move(std::get<Delete>(p).path)}};
            },
            [&](l::TogglePicker& a) -> std::optional<R> {
                open_picker_ = open_picker_ == a.which ? std::nullopt : std::optional(a.which);
                return R{};
            },
            [&](l::UseTemplate& a) -> std::optional<R> {
                if (a.value.index) copy(*a.value.index);
                else new_agent();
                return std::nullopt;
            },
            [&](l::New&) -> std::optional<R> {
                new_agent();
                return std::nullopt;
            },
            [&](l::Edit& a) -> std::optional<R> {
                edit(a.index);
                return std::nullopt;
            },
            [&](l::Name& a) -> std::optional<R> {
                name_ = std::move(a.value);
                return std::nullopt;
            },
            [&](l::Description& a) -> std::optional<R> {
                description_ = std::move(a.value);
                return std::nullopt;
            },
            [&](l::Model& a) -> std::optional<R> {
                model_ = model_choice(a.value);
                open_picker_.reset();
                return std::nullopt;
            },
            [&](l::Thinking& a) -> std::optional<R> {
                if (a.value == THINKING_DEFAULT) thinking_.reset();
                else thinking_ = std::move(a.value);
                open_picker_.reset();
                return std::nullopt;
            },
            [&](l::Tool& a) -> std::optional<R> {
                toggle(tools_on_, std::move(a.name), a.on);
                return std::nullopt;
            },
            [&](l::Spawn& a) -> std::optional<R> {
                toggle(spawns_on_, std::move(a.name), a.on);
                return std::nullopt;
            },
            [&](l::Body& a) -> std::optional<R> {
                body_ = std::move(a.text);
                return std::nullopt;
            },
        },
        m);
    if (answered) return std::move(*answered);
    if (show_errors_) return Intent{vs::intent::Check{draft()}};
    return std::nullopt;
}

} // namespace loa::ui::settings::agents
