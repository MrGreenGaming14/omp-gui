#pragma once

// Workflow list and editor: managed rules edit as structured fields (name, agent access, routing
// notes, steps and rule blocks), hand-written and external rules as raw Markdown. File access
// and validation belong to the app. Port of `crates/loa-ui-settings/src/workflows/mod.rs`.

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/view/settings_workflows.hpp"

namespace loa::ui::settings::workflows {

namespace vs = loa::view::settings::workflows;
using vs::AgentInfo;
using vs::Draft;
using vs::Error;
using vs::FieldError;
using vs::Intent;
using vs::ManagedFields;
using vs::RoutingNote;
using vs::RuleBlock;
using vs::Rules;
using vs::Workflow;

// An agent's access under managed rules.
enum class Access : std::uint8_t { Allow, Disallow, Unspecified };
[[nodiscard]] std::string_view label(Access a);

namespace local {
struct Open { std::string role; };
struct New {};
struct Role { std::string value; };
struct Name { std::string value; };
struct Models { std::string value; };
struct SetAccess { std::string agent; Access access = Access::Unspecified; };
struct Description { std::string value; };
struct Routing { std::string agent; std::string note; };
struct AddStep {};
struct Step { std::size_t index = 0; std::string value; };
struct RemoveStep { std::size_t index = 0; };
struct MoveStepUp { std::size_t index = 0; };
struct MoveStepDown { std::size_t index = 0; };
struct AddBlock {};
struct BlockHeading { std::size_t index = 0; std::string value; };
// A rule block editor's text after an edit.
struct BlockText { std::size_t index = 0; std::string text; };
struct RemoveBlock { std::size_t index = 0; };
struct CopyFrom { std::string role; };
// The notes or raw Markdown editor's text after an edit.
struct Text { std::string text; };
struct Save {};
struct Delete {};
struct ConfirmDelete {};
struct CancelDelete {};
} // namespace local

using Local = std::variant<local::Open, local::New, local::Role, local::Name, local::Models, local::SetAccess, local::Description,
                           local::Routing, local::AddStep, local::Step, local::RemoveStep, local::MoveStepUp, local::MoveStepDown,
                           local::AddBlock, local::BlockHeading, local::BlockText, local::RemoveBlock, local::CopyFrom, local::Text,
                           local::Save, local::Delete, local::ConfirmDelete, local::CancelDelete>;
using Message = std::variant<Local, Intent>;

class State {
  public:
    State();

    std::optional<Intent> update(Message m);
    std::optional<Intent> reduce(Local m);
    // Close always; Reload only while the file changed on disk.
    [[nodiscard]] bool guard(const Intent& e) const;

    void set_workflows(std::vector<Workflow> entries) { entries_ = std::move(entries); }
    void set_agents(std::vector<AgentInfo> agents) { agents_ = std::move(agents); }
    void set_errors(std::vector<FieldError> errors) { errors_ = std::move(errors); }
    void set_error(std::optional<Error> error) { error_ = std::move(error); }
    // The role of the workflow being edited; `std::nullopt` for a new one.
    [[nodiscard]] std::optional<std::string_view> selected() const;
    void new_workflow();
    // Fills the editor from the listed workflow `role`; false when it isn't listed.
    bool edit(std::string_view role);
    [[nodiscard]] Draft draft() const;
    [[nodiscard]] Access access(std::string_view agent) const;

    // What the view shows.
    [[nodiscard]] const std::vector<Workflow>& entries() const noexcept { return entries_; }
    [[nodiscard]] const std::vector<AgentInfo>& agents() const noexcept { return agents_; }
    [[nodiscard]] const std::string& role() const noexcept { return role_; }
    [[nodiscard]] const std::string& models() const noexcept { return models_; }
    [[nodiscard]] const Rules& rules() const noexcept { return rules_; }
    [[nodiscard]] const std::string& text() const noexcept { return text_; }
    [[nodiscard]] const std::string& block_text(std::size_t index) const { return block_text_.at(index).content; }
    [[nodiscard]] const std::vector<FieldError>& errors() const noexcept { return errors_; }
    [[nodiscard]] const std::optional<Error>& error() const noexcept { return error_; }
    [[nodiscard]] bool confirming_delete() const noexcept { return confirming_delete_; }

  private:
    struct BlockEditor {
        std::string content;
        bool dirty = false;
    };
    void set_rules(Rules rules);

    std::vector<Workflow> entries_;
    std::vector<AgentInfo> agents_;
    std::optional<std::string> previous_role_;
    std::string role_;
    std::string models_;
    Rules rules_;
    std::string text_;
    bool text_dirty_ = false;
    std::vector<BlockEditor> block_text_;
    std::vector<FieldError> errors_;
    std::optional<Error> error_;
    bool confirming_delete_ = false;
};

} // namespace loa::ui::settings::workflows
