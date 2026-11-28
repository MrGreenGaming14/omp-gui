#pragma once

// The New project screen: a name, the project's folder and optional shared instructions. Create
// hands them to the app as `create_intent::Create`; the app answers a failure with
// `State::failed`. Port of `crates/loa-ui-settings/src/projects/create.rs`.

#include <optional>
#include <string>
#include <string_view>
#include <variant>

#include "loa/view/settings_projects.hpp"

namespace loa::ui::settings::projects::create {

using loa::view::settings::projects::CreateIntent;

// Id of the name input, focused when the screen opens.
inline constexpr std::string_view NAME_ID = "loa-new-project-name";

namespace local {
struct Name { std::string name; };
struct Folder { std::string folder; };
// The Choose… button beside the folder.
struct ChooseFolder {};
// The shared instructions editor's text after an edit.
struct Instructions { std::string text; };
struct Create {};
} // namespace local

using Local = std::variant<local::Name, local::Folder, local::ChooseFolder, local::Instructions, local::Create>;
using Message = std::variant<Local, CreateIntent>;

class State {
  public:
    // An empty form whose folder starts at `folder`, the current one.
    explicit State(std::string folder);

    std::optional<CreateIntent> update(Message m);
    std::optional<CreateIntent> reduce(Local m);
    [[nodiscard]] bool guard(const CreateIntent& e) const;

    // The folder the app's dialog picked.
    void set_folder(std::string folder);
    // The app couldn't create the project.
    void failed(std::string error);

    [[nodiscard]] std::optional<std::string_view> error() const;
    [[nodiscard]] const std::string& name() const noexcept { return name_; }
    [[nodiscard]] const std::string& folder() const noexcept { return folder_; }
    [[nodiscard]] const std::string& instructions() const noexcept { return instructions_; }
    [[nodiscard]] bool creating() const noexcept { return creating_; }
    [[nodiscard]] bool can_create() const;

  private:
    std::string name_;
    std::string folder_;
    std::string instructions_;
    bool creating_ = false;
    std::optional<std::string> error_;
};

} // namespace loa::ui::settings::projects::create
