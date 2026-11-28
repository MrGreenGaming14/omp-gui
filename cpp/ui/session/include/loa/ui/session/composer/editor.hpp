#pragma once

// The composer's text model, after iced's `text_editor::Content`: lines of UTF-8 text, a cursor
// at a byte column with an optional selection anchor, and the editing actions the view reports.
// Plain C++ so state tests drive it as the Rust tests drive iced's editor.

#include <cstddef>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

namespace loa::ui::session::composer {

// A line and a byte column in it.
struct Position {
    std::size_t line = 0;
    std::size_t column = 0;
    friend bool operator==(const Position&, const Position&) = default;
    friend auto operator<=>(const Position&, const Position&) = default;
};

// The caret and, while text is selected, the selection's other end (its anchor).
struct Cursor {
    Position position;
    std::optional<Position> selection;
    friend bool operator==(const Cursor&, const Cursor&) = default;
};

enum class Motion : unsigned char { Left, Right, Up, Down, WordLeft, WordRight, Home, End, DocumentStart, DocumentEnd };

namespace edit {
struct Insert {
    char32_t c = 0;
    friend bool operator==(const Insert&, const Insert&) = default;
};
struct Paste {
    std::string text;
    friend bool operator==(const Paste&, const Paste&) = default;
};
struct Enter {
    friend bool operator==(const Enter&, const Enter&) = default;
};
struct Backspace {
    friend bool operator==(const Backspace&, const Backspace&) = default;
};
struct Delete {
    friend bool operator==(const Delete&, const Delete&) = default;
};
} // namespace edit

using Edit = std::variant<edit::Insert, edit::Paste, edit::Enter, edit::Backspace, edit::Delete>;

namespace action {
struct Edit {
    composer::Edit edit;
    friend bool operator==(const Edit&, const Edit&) = default;
};
struct Move {
    Motion motion = Motion::Left;
    friend bool operator==(const Move&, const Move&) = default;
};
struct Select {
    Motion motion = Motion::Left;
    friend bool operator==(const Select&, const Select&) = default;
};
struct SelectAll {
    friend bool operator==(const SelectAll&, const SelectAll&) = default;
};
// A press at a position: the caret goes there.
struct Click {
    Position position;
    friend bool operator==(const Click&, const Click&) = default;
};
// A drag to a position: the selection extends there.
struct Drag {
    Position position;
    friend bool operator==(const Drag&, const Drag&) = default;
};
} // namespace action

using Action = std::variant<action::Edit, action::Move, action::Select, action::SelectAll, action::Click, action::Drag>;

// Editable text: always at least one (possibly empty) line.
class Content {
  public:
    Content() = default;
    [[nodiscard]] static Content with_text(std::string_view text);

    // The lines joined by `\n`.
    [[nodiscard]] std::string text() const;
    [[nodiscard]] const std::vector<std::string>& lines() const { return lines_; }
    [[nodiscard]] const std::string* line(std::size_t n) const { return n < lines_.size() ? &lines_[n] : nullptr; }
    [[nodiscard]] std::size_t line_count() const { return lines_.size(); }
    // One empty line.
    [[nodiscard]] bool is_empty() const { return lines_.size() == 1 && lines_[0].empty(); }
    [[nodiscard]] Cursor cursor() const;
    // The selected text, if any.
    [[nodiscard]] std::optional<std::string> selection() const;

    // Places the cursor, clamped into the text.
    void move_to(Cursor cursor);
    void perform(const Action& action);

  private:
    [[nodiscard]] Position clamp(Position p) const;
    [[nodiscard]] Position moved(Position p, Motion m) const;
    // Deletes the selection; whether there was one.
    bool delete_selection();
    void insert(std::string_view text);

    std::vector<std::string> lines_{std::string()};
    Position position_;
    std::optional<Position> anchor_;
};

} // namespace loa::ui::session::composer
