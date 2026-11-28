#pragma once

// The agents screen: the omp agents found on disk on the left, the form that builds one on the
// right. Port of `crates/loa-ui-settings/src/agents/mod.rs`.
//
// The form has one field per section of the definition: name, description, model, thinking
// level, one checkbox per tool, one checkbox per other agent the new agent may spawn, and the
// prompt body. A template fills it: blank, or a copy of a listed agent. Save hands the `Draft`
// to the app, which checks it, writes it when it has no errors and answers with
// `State::set_errors`; the problems show next to their fields and, from then on, every edit asks
// the app to check again (`intent::Check`).
//
// The app lists the agents (`State::set_agents`) and what the form offers (`State::set_choices`).
// Replace and delete ask first; enable and disable act on the agent being edited.

#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/view/settings_agents.hpp"

namespace loa::ui::settings::agents {

namespace vs = loa::view::settings::agents;
using vs::AgentEntry;
using vs::AgentFields;
using vs::AgentSource;
using vs::Choices;
using vs::Draft;
using vs::FieldError;
using vs::Intent;
using vs::ModelSpec;

// The model choice that leaves `model` out: omp picks its default.
inline constexpr std::string_view MODEL_DEFAULT = "(omp default)";
// The thinking level choice that leaves `thinkingLevel` out.
inline constexpr std::string_view THINKING_DEFAULT = "(default)";

// A way to start the form.
struct Template {
    // The agent copied, by position in the list; `std::nullopt` is blank.
    std::optional<std::size_t> index;
    std::string text;

    [[nodiscard]] static Template blank() { return {std::nullopt, "Blank"}; }
    // The picker label: `Blank` or `Copy of <name>`.
    [[nodiscard]] const std::string& label() const noexcept { return text; }
    friend bool operator==(const Template&, const Template&) = default;
};

// The form's pickers, whose menus open one at a time.
enum class PickerKind : std::uint8_t { Template, Model, Thinking };

namespace local {
// A template was picked.
struct UseTemplate { Template value; };
// An agent of the list was picked: its fields fill the form.
struct Edit { std::size_t index = 0; };
struct New {};
struct Name { std::string value; };
struct Description { std::string value; };
struct Model { std::string value; };
struct Thinking { std::string value; };
struct Tool { std::string name; bool on = false; };
struct Spawn { std::string name; bool on = false; };
// The prompt editor's text after an edit.
struct Body { std::string text; };
struct Save {};
// Enables or disables the agent being edited.
struct Toggle {};
// Asks before deleting the agent being edited.
struct Delete {};
// Answers the pending question with yes.
struct Confirm {};
// Answers the pending question with no.
struct Cancel {};
// Opens or closes a picker's menu.
struct TogglePicker { PickerKind which = PickerKind::Template; };
} // namespace local

using Local = std::variant<local::UseTemplate, local::Edit, local::New, local::Name, local::Description, local::Model,
                           local::Thinking, local::Tool, local::Spawn, local::Body, local::Save, local::Toggle,
                           local::Delete, local::Confirm, local::Cancel, local::TogglePicker>;
using Message = std::variant<Local, Intent>;

class State {
  public:
    State();

    std::optional<Intent> update(Message m);
    std::optional<Intent> reduce(Local m);
    // The view emits only Close directly; everything else goes through the reducer.
    [[nodiscard]] bool guard(const Intent& e) const;

    // The agents found on disk, in list order.
    void set_agents(std::vector<AgentEntry> entries);
    // The models, thinking levels and tools the form offers.
    void set_choices(Choices choices);
    // The question waiting for an answer, if any.
    [[nodiscard]] std::optional<std::string> pending_question() const;
    // The file the form edits, when it edits one.
    [[nodiscard]] const std::optional<std::filesystem::path>& editing() const noexcept { return editing_; }
    // Whether the agent file at `path` is listed enabled.
    [[nodiscard]] std::optional<bool> is_enabled(const std::filesystem::path& path) const;
    // Fills the form from the listed agent file at `path`, unchanged.
    bool edit_path(const std::filesystem::path& path);
    // Asks whether to replace the agent file at `path` with `draft`.
    void ask_replace(Draft draft, std::filesystem::path path);
    // The app's check of the form: the problems by field, none when it passed.
    void set_errors(std::vector<FieldError> errors);
    // Shows `message` next to `field` (`"form"` for the top of the form), keeping the form open.
    // `field` must outlive the state (a literal).
    void set_error(std::string_view field, std::string message);
    // Starts a blank form.
    void new_agent();
    // Fills the form from the agent at `index`, unchanged.
    void edit(std::size_t index);
    // Fills the form with a copy of the agent at `index`: every field and the body, with `-copy` after the name.
    void copy(std::size_t index);
    // The agents' names in list order.
    [[nodiscard]] std::vector<std::string_view> agent_names() const;
    // The templates the picker offers.
    [[nodiscard]] std::vector<Template> templates() const;
    [[nodiscard]] const std::string& name() const noexcept { return name_; }
    // The problems the app's last check found, by field.
    [[nodiscard]] const std::vector<FieldError>& errors() const noexcept { return errors_; }
    // The fields the form holds now.
    [[nodiscard]] AgentFields fields() const;
    // The form as the app writes it.
    [[nodiscard]] Draft draft() const;
    // The other known agents, deduplicated, in list order.
    [[nodiscard]] std::vector<std::string> other_agents() const;

    // What the view shows.
    [[nodiscard]] const std::vector<AgentEntry>& entries() const noexcept { return entries_; }
    [[nodiscard]] const Choices& choices() const noexcept { return choices_; }
    [[nodiscard]] const Template& current_template() const noexcept { return template_; }
    [[nodiscard]] const std::string& description() const noexcept { return description_; }
    [[nodiscard]] const std::optional<std::string>& thinking() const noexcept { return thinking_; }
    [[nodiscard]] const std::vector<std::string>& tools_on() const noexcept { return tools_on_; }
    [[nodiscard]] const std::vector<std::string>& spawns_on() const noexcept { return spawns_on_; }
    [[nodiscard]] const std::string& body() const noexcept { return body_; }
    // `Replace` or `Delete` while a question waits.
    [[nodiscard]] std::optional<std::string_view> pending_yes() const;
    [[nodiscard]] std::optional<PickerKind> open_picker() const noexcept { return open_picker_; }
    // The model picker's options: the default, the offered models, then the current one when not offered.
    [[nodiscard]] std::vector<std::string> model_options() const;
    // The label of the model picked now.
    [[nodiscard]] std::string model_label() const;
    // The thinking picker's options: the default, then the offered levels.
    [[nodiscard]] std::vector<std::string> thinking_options() const;

  private:
    struct Replace {
        Draft draft;
        std::filesystem::path path;
    };
    struct Delete {
        std::string name;
        std::filesystem::path path;
    };
    using Pending = std::variant<Replace, Delete>;

    void fill(Template t, std::optional<std::size_t> base, std::optional<std::string> name);
    [[nodiscard]] std::optional<ModelSpec> model_choice(std::string_view label) const;

    std::vector<AgentEntry> entries_;
    Choices choices_;
    // The listed file the form was filled from; keys the form has no field for stay in it.
    std::optional<std::filesystem::path> base_;
    Template template_ = Template::blank();
    std::string name_;
    std::string description_;
    std::optional<ModelSpec> model_;
    std::optional<std::string> thinking_;
    std::vector<std::string> tools_on_;
    std::vector<std::string> spawns_on_;
    std::string body_;
    std::vector<FieldError> errors_;
    // The file the form edits; `std::nullopt` for a new agent or a copy.
    std::optional<std::filesystem::path> editing_;
    std::optional<Pending> pending_;
    // Set by the first Save press; from then on the errors follow the edits.
    bool show_errors_ = false;
    std::optional<PickerKind> open_picker_;
};

} // namespace loa::ui::settings::agents
