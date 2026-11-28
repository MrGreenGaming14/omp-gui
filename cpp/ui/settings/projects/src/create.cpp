// The New project screen's reducer. Port of `crates/loa-ui-settings/src/projects/create.rs`.

#include "loa/ui/settings/projects/create.hpp"

#include "strings.hpp"

namespace loa::ui::settings::projects::create {

namespace ci = loa::view::settings::projects::create_intent;

State::State(std::string folder) : folder_(std::move(folder)) {}

std::optional<CreateIntent> State::update(Message m) {
    if (auto* l = std::get_if<Local>(&m)) return reduce(std::move(*l));
    auto e = std::get<CreateIntent>(std::move(m));
    if (guard(e)) return e;
    return std::nullopt;
}

void State::set_folder(std::string folder) {
    folder_ = std::move(folder);
    error_.reset();
}

void State::failed(std::string error) {
    creating_ = false;
    error_ = std::move(error);
}

std::optional<std::string_view> State::error() const {
    if (!error_) return std::nullopt;
    return std::string_view(*error_);
}

bool State::can_create() const { return !creating_ && !str::trim(name_).empty() && !str::trim(folder_).empty(); }

std::optional<CreateIntent> State::reduce(Local m) {
    if (auto* a = std::get_if<local::Name>(&m)) name_ = std::move(a->name);
    else if (auto* b = std::get_if<local::Folder>(&m)) folder_ = std::move(b->folder);
    else if (std::holds_alternative<local::ChooseFolder>(m)) return CreateIntent{ci::ChooseFolder{str::trimmed(folder_)}};
    else if (auto* c = std::get_if<local::Instructions>(&m)) instructions_ = std::move(c->text);
    else if (std::holds_alternative<local::Create>(m) && can_create()) {
        creating_ = true;
        error_.reset();
        loa::view::settings::projects::ProjectSettings settings;
        settings.instructions = std::string(str::trim_end(instructions_));
        settings.folder = str::trimmed(folder_);
        return CreateIntent{ci::Create{str::trimmed(name_), std::move(settings)}};
    }
    return std::nullopt;
}

bool State::guard(const CreateIntent& e) const { return std::holds_alternative<ci::Cancel>(e); }

} // namespace loa::ui::settings::projects::create
