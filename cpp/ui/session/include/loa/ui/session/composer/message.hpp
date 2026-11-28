#pragma once

// The composer's Messages (Rust `composer::Message`) and the widget operations an update asks
// the view to run (iced `Task`).

#include <cstdint>
#include <optional>
#include <string>
#include <variant>
#include <vector>

#include "loa/ui/session/composer/editor.hpp"

namespace loa::ui::session::composer {

// One of the composer's pickers.
enum class PickerId : std::uint8_t { Harness, Model, Workflow, Effort, Access, Mode, Folder, Project };

// A window, as the app names it; only the composer's own window's focus counts.
using WindowId = std::uint64_t;

namespace msg {
struct Edit {
    Action action;
    friend bool operator==(const Edit&, const Edit&) = default;
};
// Enter in the editor.
struct Submit {
    friend bool operator==(const Submit&, const Submit&) = default;
};
// The send button.
struct Send {
    friend bool operator==(const Send&, const Send&) = default;
};
struct Interrupt {
    friend bool operator==(const Interrupt&, const Interrupt&) = default;
};
struct Undo {
    friend bool operator==(const Undo&, const Undo&) = default;
};
struct Redo {
    friend bool operator==(const Redo&, const Redo&) = default;
};
struct PopupStep {
    std::int64_t by = 0;
    friend bool operator==(const PopupStep&, const PopupStep&) = default;
};
// Accept the highlighted popup row, or row `row`.
struct PopupAccept {
    std::optional<std::size_t> row;
    friend bool operator==(const PopupAccept&, const PopupAccept&) = default;
};
struct PopupClose {
    friend bool operator==(const PopupClose&, const PopupClose&) = default;
};
// Edit a queued message's text, or remove it with `std::nullopt`.
struct Queue {
    std::uint64_t id = 0;
    std::optional<std::string> text;
    friend bool operator==(const Queue&, const Queue&) = default;
};
struct Toggle {
    PickerId picker = PickerId::Harness;
    friend bool operator==(const Toggle&, const Toggle&) = default;
};
// Row `row` of a picker's menu: its options, then its action rows.
struct Pick {
    PickerId picker = PickerId::Harness;
    std::size_t row = 0;
    friend bool operator==(const Pick&, const Pick&) = default;
};
// A press outside the open menu, or Escape.
struct PickerClosed {
    friend bool operator==(const PickerClosed&, const PickerClosed&) = default;
};
// Whether an input method is composing text.
struct Composing {
    bool composing = false;
    friend bool operator==(const Composing&, const Composing&) = default;
};
// The `+` button: starts an `@` mention at the cursor.
struct MentionPressed {
    friend bool operator==(const MentionPressed&, const MentionPressed&) = default;
};
// Ctrl+V was pressed; the editor pastes any text itself.
struct PasteImage {
    friend bool operator==(const PasteImage&, const PasteImage&) = default;
};
// The × on a pasted image's card.
struct RemoveImage {
    std::uint32_t n = 0;
    friend bool operator==(const RemoveImage&, const RemoveImage&) = default;
};
// A shimmer step at this wall-clock time (Unix milliseconds).
struct Shimmer {
    std::int64_t now = 0;
    friend bool operator==(const Shimmer&, const Shimmer&) = default;
};
// A window gained or lost focus.
struct WindowFocused {
    WindowId window = 0;
    bool focused = false;
    friend bool operator==(const WindowFocused&, const WindowFocused&) = default;
};
// The effort slider moved to this level index.
struct EffortSlid {
    std::uint8_t level = 0;
    friend bool operator==(const EffortSlid&, const EffortSlid&) = default;
};
// The effort slider was let go: its level applies.
struct EffortReleased {
    friend bool operator==(const EffortReleased&, const EffortReleased&) = default;
};
struct OpenRules {
    friend bool operator==(const OpenRules&, const OpenRules&) = default;
};
} // namespace msg

using Message = std::variant<msg::Edit, msg::Submit, msg::Send, msg::Interrupt, msg::Undo, msg::Redo, msg::PopupStep,
                             msg::PopupAccept, msg::PopupClose, msg::Queue, msg::Toggle, msg::Pick, msg::PickerClosed,
                             msg::Composing, msg::MentionPressed, msg::PasteImage, msg::RemoveImage, msg::Shimmer,
                             msg::WindowFocused, msg::EffortSlid, msg::EffortReleased, msg::OpenRules>;

// Widget operations an update asks the view to run; today only focusing the editor.
struct Task {
    bool focus_editor = false;

    [[nodiscard]] static Task none() { return {}; }
    [[nodiscard]] static Task focus() { return {true}; }
    [[nodiscard]] bool empty() const { return !focus_editor; }
    friend bool operator==(const Task&, const Task&) = default;
};

} // namespace loa::ui::session::composer
