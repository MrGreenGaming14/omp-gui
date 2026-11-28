#pragma once

// The project screen: shared instructions, the ideas notes, pinned files, the digest model and
// every session of the project with its digest and context toggle, over a `ProjectOverview` the
// app loads. Edits change a draft that Save hands to the app; toggling a session updates its row
// at once and asks the app to record it. The header changes the project's folder; Delete
// project, like deleting a saved terminal conversation, asks for a confirmation first.
// `create.hpp` is the New project screen. Port of `crates/loa-ui-settings/src/projects/mod.rs`.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <optional>
#include <string>
#include <variant>

#include "loa/view/settings_projects.hpp"

namespace loa::ui::settings::projects {

namespace vs = loa::view::settings::projects;
using vs::CreateIntent;
using vs::DigestState;
using vs::Intent;
using vs::OmpHistory;
using vs::PinnedFile;
using vs::ProjectOverview;
using vs::ProjectSettings;
using vs::SessionRow;

// Decisions shown under a digest summary.
inline constexpr std::size_t SHOWN_DECISIONS = 3;

namespace local {
// The shared instructions editor's text after an edit.
struct Instructions { std::string text; };
// The ideas notes editor's text after an edit.
struct Ideas { std::string text; };
// The "Use ideas as agent context" box.
struct ShareIdeas { bool on = false; };
// Reload after saving found the notes changed outside.
struct ReloadIdeas {};
struct DigestModel { std::string model; };
struct PinPath { std::string path; };
struct PinNote { std::string note; };
struct AddPin {};
struct RemovePin { std::size_t index = 0; };
struct Save {};
struct Revert {};
// A session's "In context" toggle.
struct SetIncluded { std::string session; bool included = false; };
// Delete project: asks for a confirmation first.
struct Delete {};
struct ConfirmDelete {};
struct CancelDelete {};
// A terminal conversation's Delete: asks for a confirmation first.
struct DeleteOmpHistory { std::string id; };
struct ConfirmDeleteOmpHistory {};
struct CancelDeleteOmpHistory {};
} // namespace local

using Local = std::variant<local::Instructions, local::Ideas, local::ShareIdeas, local::ReloadIdeas, local::DigestModel,
                           local::PinPath, local::PinNote, local::AddPin, local::RemovePin, local::Save, local::Revert,
                           local::SetIncluded, local::Delete, local::ConfirmDelete, local::CancelDelete,
                           local::DeleteOmpHistory, local::ConfirmDeleteOmpHistory, local::CancelDeleteOmpHistory>;

// A local message for the reducer, or an intent the view emits directly (admitted by `guard`).
using Message = std::variant<Local, Intent>;

class State {
  public:
    // A screen for `project` showing a loading line until `set_overview`.
    explicit State(std::string project);

    // Applies a local message or admits an emitted intent while it still holds.
    std::optional<Intent> update(Message m);
    std::optional<Intent> reduce(Local m);
    [[nodiscard]] bool guard(const Intent& e) const;

    [[nodiscard]] const std::string& project() const noexcept { return project_; }
    [[nodiscard]] const ProjectOverview* overview() const noexcept { return overview_ ? &*overview_ : nullptr; }
    // Updates the saved terminal conversations, or hides the section with `std::nullopt` when no project folder is
    // available. A pending row-delete confirmation lasts through loading, and goes once the history no longer lists
    // its row.
    void set_omp_history(std::optional<OmpHistory> history);
    [[nodiscard]] const OmpHistory* omp_history() const noexcept { return omp_history_ ? &*omp_history_ : nullptr; }
    // Disables opening or deleting another terminal conversation while an import or deletion is in flight.
    void set_omp_history_busy(bool busy) noexcept { omp_history_busy_ = busy; }
    [[nodiscard]] bool omp_history_busy() const noexcept { return omp_history_busy_; }
    // Shows the delete confirmation, as for the Delete project button.
    void ask_delete() noexcept { confirm_delete_ = true; }

    // Shows a fresh overview loaded at `now` (Unix ms); unsaved edits stay, otherwise the draft follows the stored
    // settings. Notes not edited here follow the stored ones also while other settings have unsaved edits.
    void set_overview(ProjectOverview overview, std::int64_t now);
    // Drops the notes' unsaved edit for the stored notes.
    void revert_ideas();
    // Shows an error from loading or saving.
    void set_error(std::string error);
    // The answer to `SaveSettings`: on success the settings, and the ideas notes when they were sent, become the
    // stored ones.
    void saved(std::expected<std::pair<ProjectSettings, std::optional<std::string>>, std::string> result);
    // The answer to `SaveSettings` when the settings were stored but the ideas notes changed outside since they
    // were loaded: their edit stays, with a way to reload them.
    void ideas_changed_outside(ProjectSettings settings);

    // The settings as edited. The folder isn't edited here and is kept as stored.
    [[nodiscard]] ProjectSettings draft() const;
    // Whether the ideas notes have an unsaved edit.
    [[nodiscard]] bool ideas_edited() const;
    // Whether the draft differs from the stored settings or ideas notes.
    [[nodiscard]] bool is_dirty() const;

    // What the view shows.
    [[nodiscard]] std::int64_t now() const noexcept { return now_; }
    [[nodiscard]] const std::string& instructions() const noexcept { return instructions_; }
    [[nodiscard]] const std::string& ideas() const noexcept { return ideas_; }
    [[nodiscard]] bool ideas_in_context() const noexcept { return ideas_in_context_; }
    [[nodiscard]] bool ideas_changed() const noexcept { return ideas_changed_; }
    [[nodiscard]] bool ideas_reloading() const noexcept { return ideas_reloading_; }
    // Whether the notes take edits: they were read, and no reload of them is pending.
    [[nodiscard]] bool ideas_editable() const;
    [[nodiscard]] const std::vector<PinnedFile>& pinned() const noexcept { return pinned_; }
    [[nodiscard]] const std::string& digest_model() const noexcept { return digest_model_; }
    [[nodiscard]] const std::string& new_pin_path() const noexcept { return new_pin_path_; }
    [[nodiscard]] const std::string& new_pin_note() const noexcept { return new_pin_note_; }
    [[nodiscard]] bool saving() const noexcept { return saving_; }
    [[nodiscard]] const std::optional<std::string>& error() const noexcept { return error_; }
    [[nodiscard]] bool confirm_delete() const noexcept { return confirm_delete_; }
    [[nodiscard]] const std::optional<std::string>& confirm_delete_history() const noexcept { return confirm_delete_history_; }
    [[nodiscard]] bool lists_omp_history(std::string_view id) const;

  private:
    [[nodiscard]] const std::string* stored_ideas() const;
    [[nodiscard]] std::optional<std::string> ideas_edit() const;
    void reset();

    std::string project_;
    std::optional<ProjectOverview> overview_;
    std::optional<OmpHistory> omp_history_;
    bool omp_history_busy_ = false;
    // Unix milliseconds the overview was loaded at, for "3h ago".
    std::int64_t now_ = 0;
    std::string instructions_;
    std::string ideas_;
    bool ideas_in_context_ = false;
    // Saving found the notes changed outside since they were loaded; their edit stays until reloaded.
    bool ideas_changed_ = false;
    // Reload was pressed; the notes take no edit and nothing saves until the fresh overview or an error arrives.
    bool ideas_reloading_ = false;
    std::vector<PinnedFile> pinned_;
    std::string digest_model_;
    std::string new_pin_path_;
    std::string new_pin_note_;
    bool saving_ = false;
    std::optional<std::string> error_;
    // The Delete project button was pressed; waiting for the confirmation.
    bool confirm_delete_ = false;
    // The terminal conversation whose Delete was pressed; waiting for the confirmation.
    std::optional<std::string> confirm_delete_history_;
};

} // namespace loa::ui::settings::projects
