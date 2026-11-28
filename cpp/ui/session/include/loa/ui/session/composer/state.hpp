#pragma once

// The message composer (port of `loa-ui-session/src/composer/mod.rs`): an editor with Enter to
// send and Shift+Enter for a newline, undo and redo, `/` command and `@` file popups, token
// highlighting, pasted or dropped images shown as cards and sent as attachments, pickers
// (harness, model, workflow, effort slider, permissions, mode, project, folder), a stop button
// with a queue for messages written mid turn, and a working indicator while the shown
// session's turn runs. The app feeds it `loa::view::session::composer` rows and the session's
// `Activity`, and acts on its `Intent`s. Plain C++: `ComposerView` renders it.

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/ui/session/composer/editor.hpp"
#include "loa/ui/session/composer/highlight.hpp"
#include "loa/ui/session/composer/input.hpp"
#include "loa/ui/session/composer/message.hpp"
#include "loa/ui/session/composer/working.hpp"
#include "loa/view/session.hpp"

namespace loa::ui::session::composer {

using loa::view::session::composer::AccessOption;
using loa::view::session::composer::Command;
using loa::view::session::composer::HarnessOption;
using loa::view::session::composer::Intent;
using loa::view::session::composer::Label;
using loa::view::session::composer::ModelOption;
using loa::view::session::composer::PastedImage;
using loa::view::session::composer::PickerAction;
using loa::view::session::composer::WorkflowOption;
using loa::view::session::composer::Workspace;
using Event = Intent;

// Widget ids of the editor and the folder chip.
inline constexpr std::string_view EDITOR_ID = "loa-composer-editor";
inline constexpr std::string_view WORKSPACE_CHIP_ID = "loa-composer-workspace";
// Height at which the editor stops growing and scrolls.
inline constexpr float MAX_EDITOR_HEIGHT = 240.0F;
inline constexpr std::size_t UNDO_DEPTH = 200;
// Most items a popup matches, and the rows it shows at once.
inline constexpr std::size_t MAX_ITEMS = 50;
inline constexpr std::size_t VISIBLE_POPUP_ROWS = 8;
// Tooltips of the icon-only buttons.
inline constexpr std::string_view SEND_LABEL = "Send (Enter)";
inline constexpr std::string_view STOP_LABEL = "Stop (Esc)";
inline constexpr std::string_view REMOVE_QUEUED_LABEL = "Remove queued message";
inline constexpr std::string_view MENTION_LABEL = "Mention a file (@)";
// The project chip's label and menu row for a session in no project.
inline constexpr std::string_view NO_PROJECT = "No project";
inline constexpr std::string_view FOLDER_ACTIONS[2] = {"Choose another folder\u2026", "Show in Files"};
inline constexpr std::string_view WORKFLOW_ACTION = "Edit workflows\u2026";
inline constexpr std::string_view WORKFLOWS_UNAVAILABLE = "Available with OMP sessions";

// Options of one picker and the selected one.
template <class T>
struct Picker {
    std::vector<T> options;
    std::optional<std::size_t> selected;

    [[nodiscard]] const T* chosen() const { return selected && *selected < options.size() ? &options[*selected] : nullptr; }
};

// An image pasted or dropped since the last send, shown as `[Image #n]` in the text and as a
// card above it.
struct Image {
    std::uint32_t n = 0;
    PastedImage image;
    // Its token is in the text, so its card shows and it is sent.
    bool shown = false;
};

// What the composer holds unsent: the text with its cursor, the pasted images, the mentioned
// paths and the image counter. `State::take_draft` sets it aside, `State::restore_draft`
// brings it back.
struct Draft {
    std::string text;
    Cursor cursor;
    std::vector<Image> images;
    std::vector<std::string> mentions;
    std::uint32_t next_image = 0;

    // Whether there is no text to bring back.
    [[nodiscard]] bool is_empty() const { return text.empty(); }
    // Puts `earlier`, written before this draft, ahead of it with a blank line between; the
    // cursor stays in this draft's text. `earlier`'s images are numbered on from this draft's.
    void prepend(Draft earlier);
};

// A picker menu row: the choice, a muted note, and whether it can be picked.
struct MenuRow {
    std::string label;
    std::optional<std::string> detail;
    bool enabled = true;
};

// A chip of the controls row or the bar above the editor.
struct Chip {
    PickerId picker = PickerId::Harness;
    std::string label;
    bool open = false;
    bool enabled = true;
};

class State {
  public:
    State();

    // ---- Inputs from the app ----
    // Folders the folder menu offers besides the current one.
    void set_folders(std::vector<Workspace> folders);
    // The folder the session works in, shown above the editor.
    void set_workspace(std::optional<Workspace> workspace);
    [[nodiscard]] const Workspace* workspace() const { return pickers_.folder.chosen(); }
    // Node key or user written to `Prompt::by`.
    void set_author(std::string author) { author_ = std::move(author); }
    // The window the composer is in: only its focus starts and stops the animations.
    void set_window(WindowId window) { window_ = window; }
    void set_commands(std::vector<Command> commands);
    [[nodiscard]] const std::vector<Command>& commands() const { return commands_; }
    // Files for the `@` popup, project relative and `/` separated.
    void set_files(std::vector<std::string> files);
    void set_models(std::vector<ModelOption> models, std::optional<std::string_view> current);
    // Rows listed under the models, e.g. "All models…".
    void set_model_actions(std::vector<PickerAction> actions) { pickers_.model_actions = std::move(actions); }
    // Effort levels of the current model; none hides the picker.
    void set_efforts(std::vector<std::string> efforts, std::optional<std::string> current);
    [[nodiscard]] std::pair<const std::vector<ModelOption>&, const ModelOption*> models() const {
        return {pickers_.model.options, pickers_.model.chosen()};
    }
    [[nodiscard]] std::pair<std::vector<std::string>, std::optional<std::string>> efforts() const;
    void set_mode(loa::view::session::SessionMode mode);
    // Permission levels and the current one; none hides the picker.
    void set_access(std::vector<AccessOption> options, std::optional<std::string_view> current);
    // Harnesses prompts can be routed to; the picker shows with at least two.
    void set_harnesses(std::vector<HarnessOption> harnesses, std::optional<std::string_view> current);
    [[nodiscard]] const HarnessOption* harness() const { return pickers_.harness.chosen(); }
    // Sets the available workflows and whether this harness supports them.
    void set_workflows(std::vector<WorkflowOption> options, bool enabled);
    [[nodiscard]] const WorkflowOption* workflow() const { return pickers_.workflow.chosen(); }
    // The projects the project chip offers, and the current one (unset: no project).
    void set_projects(std::vector<std::string> projects, std::optional<std::string> current);
    // The message picking the option or action row with id `key`, if listed.
    [[nodiscard]] std::optional<Message> pick_key(PickerId picker, std::string_view key) const;

    // ---- Turn lifecycle ----
    void turn_started() { phase_ = Phase::Running; }
    // The turn ended, or the last `Submit` could not start one: releases the next queued message.
    std::optional<Intent> turn_ended();
    // Whether a turn runs or is starting.
    [[nodiscard]] bool is_busy() const { return phase_ != Phase::Idle; }
    // Follows what the shown session's agent is doing.
    void set_activity(Activity activity) { activity_ = std::move(activity); }
    [[nodiscard]] const Activity& activity() const { return activity_; }
    // The working indicator's label, unset while it is hidden.
    [[nodiscard]] std::optional<std::string_view> indicator() const;
    // The indicator's trailer (elapsed time and hint), shown once the turn runs.
    [[nodiscard]] std::optional<std::string> trailer() const;
    // Follows the session now shown: busy while it runs a turn. Leaving a running session for an
    // idle one puts the messages queued for it back into the editor.
    void follow_session(bool busy);

    // ---- Drafts ----
    // Empties the editor and returns what it held as a `Draft`; the undo history is dropped.
    Draft take_draft();
    // Puts `draft` in the editor, cursor where it was, replacing what is there.
    void restore_draft(Draft draft);
    // Puts `draft`, written elsewhere before what the editor holds, ahead of it.
    void prepend_draft(Draft draft);
    // The app could not start a turn for `text`, the last `Submit`: it goes back into the editor
    // ahead of anything typed since, and the composer is idle again.
    Task give_back(std::string_view text);
    // Replaces the text, cursor at the end, and focuses the editor; undo brings the old text back.
    Task set_text(std::string_view text);
    // Adds a pasted or dropped image: `[Image #n]` goes in at the cursor and its card above.
    Task attach_image(PastedImage image);
    // Mentions a dropped file by its path, as the `@` popup would.
    Task mention_path(std::string_view path);

    // ---- Reading ----
    [[nodiscard]] std::string text() const { return content_.text(); }
    [[nodiscard]] Cursor cursor() const { return content_.cursor(); }
    [[nodiscard]] const Content& content() const { return content_; }
    [[nodiscard]] const std::deque<QueuedMessage>& queue() const { return queue_.items; }
    [[nodiscard]] std::optional<PopupKind> popup_kind() const;
    // Labels of the popup rows in order, with the highlighted row.
    [[nodiscard]] std::pair<std::vector<std::string>, std::size_t> popup_items() const;
    // Descriptions of the popup rows (commands only).
    [[nodiscard]] std::vector<std::string> popup_details() const;
    [[nodiscard]] const std::vector<Image>& images() const { return images_; }
    [[nodiscard]] bool keyword() const { return keyword_; }
    [[nodiscard]] std::uint8_t shimmer() const { return shimmer_; }
    [[nodiscard]] bool focused() const { return focused_; }
    [[nodiscard]] bool can_send() const { return can_send_; }
    [[nodiscard]] std::optional<PickerId> open_picker() const { return pickers_.open; }
    [[nodiscard]] bool workflows_enabled() const { return pickers_.workflows_enabled; }
    [[nodiscard]] KeyContext key_context() const;
    [[nodiscard]] std::string placeholder() const;
    // The highlighter phase: only a keyword moves with the shimmer.
    [[nodiscard]] Settings highlight_settings() const { return {keyword_ ? shimmer_ : std::uint8_t{0}}; }
    // How often the shimmer clock ticks, unset when it rests.
    [[nodiscard]] std::optional<std::chrono::milliseconds> tick() const;

    // Picker menus: the controls row chips (shown ones), the bar chips (project and mode), rows.
    [[nodiscard]] std::vector<Chip> chips() const;
    [[nodiscard]] std::vector<Chip> bar_chips() const;
    [[nodiscard]] std::vector<MenuRow> menu_rows(PickerId picker) const;
    [[nodiscard]] std::vector<std::string> menu_actions(PickerId picker) const;
    [[nodiscard]] std::optional<std::size_t> menu_current(PickerId picker) const;
    // The effort card: levels, the level shown (dragged or selected), and the title.
    [[nodiscard]] std::size_t effort_levels() const { return pickers_.effort.options.size(); }
    [[nodiscard]] std::optional<std::size_t> effort_shown() const;
    [[nodiscard]] std::string effort_title() const;
    [[nodiscard]] std::optional<std::string> effort_model() const;

    std::pair<Task, std::optional<Intent>> update(Message message);

  private:
    enum class Phase : std::uint8_t { Idle, Starting, Running };

    struct Pickers {
        Picker<HarnessOption> harness;
        Picker<ModelOption> model;
        std::vector<PickerAction> model_actions;
        Picker<Label> effort;
        Picker<AccessOption> access;
        Picker<Label> mode;
        Picker<Workspace> folder;
        std::vector<Workspace> folders;
        // `NO_PROJECT`, then the project names; the chip shows even with none.
        Picker<Label> project;
        Picker<WorkflowOption> workflow;
        bool workflows_enabled = false;
        std::optional<PickerId> open;
    };

    void rebuild_folders();
    [[nodiscard]] std::size_t menu_len(PickerId picker) const;
    [[nodiscard]] std::string menu_key(PickerId picker, std::size_t i) const;
    std::optional<Intent> pick(PickerId picker, std::size_t i);
    [[nodiscard]] const Popup* visible_popup() const;
    [[nodiscard]] const std::string& candidate(PopupKind kind, std::size_t index) const;
    [[nodiscard]] Snapshot snapshot() const { return {content_.text(), content_.cursor()}; }
    std::optional<Intent> edit(const Action& action);
    std::optional<Intent> step(bool redo);
    std::optional<Intent> start_mention();
    [[nodiscard]] bool after_word() const;
    void insert_word(std::string_view word);
    std::optional<Intent> remove_image(std::uint32_t n);
    void text_changed();
    std::optional<Intent> submit();
    Intent send(std::string_view message);
    std::optional<Intent> stop();
    void unqueue();
    [[nodiscard]] Intent queue_changed() const;
    std::optional<Intent> accept(std::optional<std::size_t> row);
    std::optional<Intent> refresh_popup();
    void refilter();
    [[nodiscard]] std::vector<std::size_t> matches(const Trigger& trigger) const;
    void replace(std::string_view text);
    Draft queued_attachments();
    void show_draft(Draft draft);
    void reset_editing();

    Content content_;
    UndoHistory history_{UNDO_DEPTH};
    bool composing_ = false;
    bool can_send_ = false;
    Phase phase_ = Phase::Idle;
    Queue queue_;
    std::vector<Command> commands_;
    std::vector<std::string> files_;
    // Paths picked from the `@` popup or dropped since the last send.
    std::vector<std::string> mentions_;
    std::optional<Popup> popup_;
    // A popup closed with Esc stays closed while its word is edited.
    std::optional<Trigger> dismissed_;
    Pickers pickers_;
    std::string author_;
    bool keyword_ = false;
    std::uint8_t shimmer_ = 0;
    std::int64_t now_ = 0;
    Activity activity_;
    std::optional<WindowId> window_;
    bool focused_ = true;
    std::vector<Image> images_;
    std::uint32_t next_image_ = 0;
    std::optional<std::uint8_t> effort_drag_;
};

// Whether a dropped file is an image the composer attaches as a card.
[[nodiscard]] bool is_image(const std::filesystem::path& path);
// The pasted image at `path`, `size` pixels when known.
[[nodiscard]] PastedImage pasted(std::filesystem::path path, std::optional<std::pair<std::uint32_t, std::uint32_t>> size);
// `Claude Opus 5.5` reads `Opus 5.5`.
[[nodiscard]] std::string short_model_name(std::string_view name);
// `medium` reads `Medium`.
[[nodiscard]] std::string capitalized(std::string_view label);

} // namespace loa::ui::session::composer
