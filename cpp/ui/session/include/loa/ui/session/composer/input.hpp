#pragma once

// The composer's editing machinery (port of `composer/input.rs`): undo history, `/` and `@`
// popup triggers, `@path` mentions, the mid-turn queue and the editor key bindings.

#include <cstddef>
#include <cstdint>
#include <deque>
#include <functional>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/ui/session/composer/editor.hpp"
#include "loa/ui/session/composer/message.hpp"
#include "loa/view/session.hpp"

namespace loa::ui::session::composer {

// Editor text and cursor at one point in time.
struct Snapshot {
    std::string text;
    Cursor cursor;
    friend bool operator==(const Snapshot&, const Snapshot&) = default;
};

// How an edit groups with its neighbours: typing one word (and the whitespace after it) is one
// group, a run of deletions another; any other edit stands alone.
struct EditKind {
    enum class Kind : std::uint8_t { Insert, Delete, Other };
    Kind kind = Kind::Other;
    bool whitespace = false; // for `Insert`

    static constexpr EditKind insert(bool whitespace) { return {Kind::Insert, whitespace}; }
    static constexpr EditKind del() { return {Kind::Delete, false}; }
    static constexpr EditKind other() { return {Kind::Other, false}; }
    friend bool operator==(const EditKind&, const EditKind&) = default;
};

// How `edit` groups: an IME commit arrives as a paste; a single typed character is an insert.
[[nodiscard]] EditKind edit_kind(const Edit& edit);

// Bounded undo and redo stacks holding the state before each group of edits.
class UndoHistory {
  public:
    explicit UndoHistory(std::size_t depth) : depth_(depth < 1 ? 1 : depth) {}

    // Whether an edit of `kind` joins the open group; a word after whitespace starts a new one.
    [[nodiscard]] bool continues(EditKind kind) const;
    // Records `before`, the state preceding a new group of `kind`; clears redo.
    void begin(EditKind kind, Snapshot before);
    void extend(EditKind kind) { open_ = kind; }
    void break_group() { open_.reset(); }
    std::optional<Snapshot> undo(Snapshot current);
    std::optional<Snapshot> redo(Snapshot current);
    void clear() { *this = UndoHistory(depth_); }

  private:
    void push_undo(Snapshot s);

    std::deque<Snapshot> undo_;
    std::vector<Snapshot> redo_;
    std::optional<EditKind> open_;
    std::size_t depth_;
};

// Which popup a trigger opens.
enum class PopupKind : std::uint8_t {
    Command, // `/` at the start of the message.
    File,    // `@` at the start of a word.
};

// The word under the cursor that opened a popup: byte columns `start..end` on `line`, and the
// text between the trigger character and the cursor.
struct Trigger {
    PopupKind kind = PopupKind::Command;
    std::size_t line = 0, start = 0, end = 0;
    std::string query;

    // The same popup word, possibly with another query.
    [[nodiscard]] bool same_anchor(const Trigger& o) const { return kind == o.kind && line == o.line && start == o.start; }
    friend bool operator==(const Trigger&, const Trigger&) = default;
};

// The popup trigger for the cursor at byte `column` of `line`, line number `index`.
[[nodiscard]] std::optional<Trigger> detect(std::size_t index, std::string_view line, std::size_t column);

// An open popup: its trigger, the matching candidate indices and the highlighted row.
struct Popup {
    Trigger trigger;
    std::vector<std::size_t> matches;
    std::size_t selected = 0;

    // Moves the highlight by `by` rows, wrapping.
    void step(std::int64_t by);
    friend bool operator==(const Popup&, const Popup&) = default;
};

// The text inserted for a mention of `path`, quoted when it holds whitespace.
[[nodiscard]] std::string token(std::string_view path);
// Paths written as `@path` or `@"path"` at word starts in `text`. An unquoted mention ending in
// punctuation also yields the word without it, so `see @a.rs,` resolves `a.rs`.
[[nodiscard]] std::vector<std::string_view> mentioned(std::string_view text);
// Attachments for the mentions in `text` that `is_known` accepts, once each, in order.
[[nodiscard]] std::vector<loa::view::session::Attachment> resolve(std::string_view text,
                                                                  const std::function<bool(std::string_view)>& is_known);

// One message queued while a turn runs; `id` survives edits of its text.
struct QueuedMessage {
    std::uint64_t id = 0;
    std::string text;
    friend bool operator==(const QueuedMessage&, const QueuedMessage&) = default;
};

// Messages waiting for the running turn, first in first out.
struct Queue {
    std::deque<QueuedMessage> items;

    void push(std::string text);
    // Replaces the text of `id`, or removes it when `text` is unset; false when it is gone.
    bool edit(std::uint64_t id, std::optional<std::string> text);
    // The oldest message with non-blank text; blank ones (edited down to nothing) are dropped.
    std::optional<QueuedMessage> pop_sendable();

  private:
    std::uint64_t next_id_ = 0;
};

// ---- Key bindings --------------------------------------------------------------------------

// A key the editor sees, as the view translates it from the toolkit.
enum class Key : std::uint8_t {
    Character, // see `KeyPress::text`
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Home,
    End,
    Other,
};

struct Modifiers {
    bool shift = false, control = false, alt = false, logo = false;
    friend bool operator==(const Modifiers&, const Modifiers&) = default;
};

// Ctrl, or Cmd on macOS (iced `Modifiers::command`).
[[nodiscard]] bool command(Modifiers m);
// Ctrl, or Alt on macOS (iced `Modifiers::jump`).
[[nodiscard]] bool jump(Modifiers m);
// Cmd on macOS only (iced `Modifiers::macos_command`).
[[nodiscard]] bool macos_command(Modifiers m);

struct KeyPress {
    Key key = Key::Other;
    Modifiers modifiers;
    // The characters the key types (`Character` keys); the Latin letter for chords.
    std::string text;
};

// Composer state the key bindings depend on.
struct KeyContext {
    bool popup_open = false;
    // An input method is composing; its keys must reach the IME.
    bool composing = false;
    bool busy = false;
    // Text is selected; a word delete then deletes just the selection.
    bool selection = false;
};

// What a bound key does: first a clipboard operation, then the messages in order.
struct Binding {
    enum class Clipboard : std::uint8_t { None, Copy, Cut, Paste };
    Clipboard clipboard = Clipboard::None;
    std::vector<Message> messages;
    friend bool operator==(const Binding&, const Binding&) = default;
};

// Enter sends and Shift+Enter breaks the line, undo and redo, word deletes, popup navigation,
// Esc interrupts. `std::nullopt`: the key is not the editor's (app shortcut, IME, unfocus).
[[nodiscard]] std::optional<Binding> bind(const KeyPress& press, KeyContext ctx);

} // namespace loa::ui::session::composer
