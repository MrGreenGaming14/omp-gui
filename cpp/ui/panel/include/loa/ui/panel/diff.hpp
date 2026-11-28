#pragma once

// The diff tab: unified and split views of `parse_unified` output with folds, building only the
// rows in view. Port of `crates/loa-ui-panel/src/diff/{mod,model,parse}.rs`. Plain C++, no Qt.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/shell/task.hpp"
#include "loa/view/panel.hpp"

namespace loa::ui::panel::diff {

using Task = loa::ui::shell::Task;

// A half-open byte or index range.
struct Range {
    std::size_t start = 0;
    std::size_t end = 0;
    [[nodiscard]] std::size_t size() const noexcept { return end > start ? end - start : 0; }
    [[nodiscard]] bool empty() const noexcept { return end <= start; }
    [[nodiscard]] bool contains(std::size_t i) const noexcept { return i >= start && i < end; }
    friend bool operator==(const Range&, const Range&) = default;
};

enum class Tag : std::uint8_t { Equal, Delete, Insert };

// A line with 1-based numbers (none on the side it is missing from), its text and emphasis ranges.
struct Line {
    Tag tag = Tag::Equal;
    std::optional<std::uint32_t> old_no;
    std::optional<std::uint32_t> new_no;
    Range text;
    Range emph;
    friend bool operator==(const Line&, const Line&) = default;
};

// `@@ -old_start,old_len +new_start,new_len @@ section`.
struct Hunk {
    std::uint32_t old_start = 0, old_len = 0, new_start = 0, new_len = 0;
    std::optional<std::size_t> section; // index of the `@@` break holding the section text
    friend bool operator==(const Hunk&, const Hunk&) = default;
};

// One rendered row: a hunk header, a collapsed fold, a unified line or a split pair.
namespace row {
struct Hunk {
    diff::Hunk hunk;
    friend bool operator==(const Hunk&, const Hunk&) = default;
};
struct Fold {
    std::size_t fold = 0;
    std::size_t hidden = 0;
    friend bool operator==(const Fold&, const Fold&) = default;
};
struct Line {
    std::size_t line = 0;
    friend bool operator==(const Line&, const Line&) = default;
};
struct Pair {
    std::optional<std::size_t> old_line;
    std::optional<std::size_t> new_line;
    friend bool operator==(const Pair&, const Pair&) = default;
};
} // namespace row
using Row = std::variant<row::Hunk, row::Fold, row::Line, row::Pair>;

enum class FileStatus : std::uint8_t { Added, Deleted, Modified, Renamed };

class Builder;

// The diff of one file; a missing path marks an added or deleted file, and a binary one has no lines.
class FileDiff {
  public:
    std::optional<std::string> old_path;
    std::optional<std::string> new_path;
    bool binary = false;

    // The new path, or the old one when deleted.
    [[nodiscard]] std::string_view path() const noexcept;
    [[nodiscard]] FileStatus status() const noexcept;
    [[nodiscard]] std::size_t added() const noexcept { return added_; }
    [[nodiscard]] std::size_t removed() const noexcept { return removed_; }
    // Largest line number on either side; zero when no line is numbered.
    [[nodiscard]] std::uint32_t max_line_no() const noexcept { return max_line_no_; }
    [[nodiscard]] std::span<const Line> lines() const noexcept { return lines_; }
    [[nodiscard]] std::string_view line_text(std::size_t i) const noexcept;
    // Byte ranges of the changed words in line `i`, relative to its text.
    [[nodiscard]] std::span<const Range> emphasis(std::size_t i) const noexcept;
    // Section text of a hunk header, such as the enclosing function.
    [[nodiscard]] std::string_view section(const Hunk& h) const noexcept;
    // Opens a fold; false when it was open or doesn't exist.
    bool expand(std::size_t fold);
    [[nodiscard]] std::vector<Row> unified_rows() const;
    // Side by side rows: a change run's deleted and inserted lines pair up, the longer side gets blank partners.
    [[nodiscard]] std::vector<Row> split_rows() const;

  private:
    friend class Builder;
    struct Fold {
        Range lines;
        bool expanded = false;
    };
    struct Break {
        std::size_t line = 0;
        Range section;
    };
    template <typename F> void walk(F&& emit) const;

    std::string text_;
    std::vector<Line> lines_;
    std::vector<Range> emph_;
    std::vector<Fold> folds_;
    std::vector<Break> breaks_;
    std::size_t added_ = 0, removed_ = 0;
    std::uint32_t max_line_no_ = 0;
};

// A unified diff the parser rejects, with the 1-based input line.
struct ParseError {
    enum class Kind : std::uint8_t { HunkHeader, Orphan, Overflow, Truncated };
    Kind kind = Kind::HunkHeader;
    std::size_t line = 0;
    std::uint32_t old_left = 0, new_left = 0; // `Truncated` only

    [[nodiscard]] std::string message() const;
    friend bool operator==(const ParseError&, const ParseError&) = default;
};

// One `FileDiff` per file of a plain (`diff -u`) or git style diff; text before the first file
// header (a commit message) is skipped.
[[nodiscard]] std::expected<std::vector<FileDiff>, ParseError> parse_unified(std::string_view input);

// Height of one diff row: a mono line, uniform so lists virtualize by arithmetic.
[[nodiscard]] float row_height();

// Rows built beyond the viewport so fast scrolling shows no gaps.
inline constexpr std::size_t OVERSCAN = 20;
// Viewport height assumed until the first scroll event reports it.
inline constexpr float DEFAULT_VIEWPORT = 1200.0F;
// The scrollable the body snaps through `Task`.
inline constexpr std::string_view SCROLL_ID = "panel.diff";

enum class Mode : std::uint8_t { Unified, Split };

namespace msg {
struct SelectFile {
    std::size_t index = 0;
};
struct SetMode {
    Mode mode = Mode::Unified;
};
// Opens a fold of the selected file.
struct Expand {
    std::size_t fold = 0;
};
struct Scrolled {
    float offset = 0, height = 0;
};
// Opens the selected file.
struct Open {};
// Hides the selected file's lines.
struct ToggleCollapsed {};
} // namespace msg
using Message = std::variant<msg::SelectFile, msg::SetMode, msg::Expand, msg::Scrolled, msg::Open, msg::ToggleCollapsed>;
using Event = loa::view::panel::DiffIntent;

class State {
  public:
    // Replaces "No changes", e.g. outside a git repository, and clears the files.
    Task set_note(std::optional<std::string> note);
    // Replaces the files and selects the first.
    Task set_files(std::vector<FileDiff> files);
    [[nodiscard]] std::span<const FileDiff> files() const noexcept { return files_; }
    [[nodiscard]] const FileDiff* selected() const noexcept;
    [[nodiscard]] std::size_t selected_index() const noexcept { return selected_; }
    [[nodiscard]] Mode mode() const noexcept { return mode_; }
    [[nodiscard]] bool collapsed() const noexcept { return collapsed_; }
    [[nodiscard]] const std::optional<std::string>& note() const noexcept { return note_; }
    [[nodiscard]] std::span<const Row> rows() const noexcept { return rows_; }
    [[nodiscard]] float scroll_offset() const noexcept { return scroll_offset_; }
    // The rows the view builds at the current scroll position.
    [[nodiscard]] Range visible_rows() const;

    std::pair<Task, std::optional<Event>> update(Message message);

  private:
    Task select(std::size_t index);
    void rebuild_rows();

    std::vector<FileDiff> files_;
    std::size_t selected_ = 0;
    Mode mode_ = Mode::Unified;
    std::vector<Row> rows_;
    float scroll_offset_ = 0;
    float viewport_height_ = DEFAULT_VIEWPORT;
    bool collapsed_ = false;
    std::optional<std::string> note_;
};

} // namespace loa::ui::panel::diff
