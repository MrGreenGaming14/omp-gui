#pragma once

// An in-panel file picker for document surfaces: browse folders and pick a PDF, Office or image
// file, or type a path. Port of `crates/loa-ui-surface/src/picker.rs`; drawn by `PickerView`.

#include <filesystem>
#include <optional>
#include <string>
#include <variant>
#include <vector>

namespace loa::ui::surface::picker {

using Path = std::filesystem::path;

struct Entry {
    std::string name;
    Path path;
    bool is_dir = false;
    friend bool operator==(const Entry&, const Entry&) = default;
};

namespace msg {
struct Up {
    friend bool operator==(const Up&, const Up&) = default;
};
struct Enter {
    Path dir;
    friend bool operator==(const Enter&, const Enter&) = default;
};
struct Choose {
    Path file;
    friend bool operator==(const Choose&, const Choose&) = default;
};
struct PathChanged {
    std::string path;
    friend bool operator==(const PathChanged&, const PathChanged&) = default;
};
struct PathSubmitted {
    friend bool operator==(const PathSubmitted&, const PathSubmitted&) = default;
};
} // namespace msg

using Message = std::variant<msg::Up, msg::Enter, msg::Choose, msg::PathChanged, msg::PathSubmitted>;

// Browses one folder at a time, listing subfolders and the files the document viewer opens.
class FilePicker {
  public:
    // Starts in `dir`, listing files with one of `extensions` (case insensitive).
    FilePicker(Path dir, std::vector<std::string> extensions);

    // Whether the document viewer opens `path`, by its extension.
    [[nodiscard]] bool supported(const Path& path) const;
    [[nodiscard]] const Path& dir() const noexcept { return dir_; }
    // The path field's text.
    [[nodiscard]] const std::string& path() const noexcept { return path_; }
    [[nodiscard]] const std::vector<Entry>& entries() const noexcept { return entries_; }
    [[nodiscard]] const std::optional<std::string>& error() const noexcept { return error_; }
    // Whether `Up` goes anywhere.
    [[nodiscard]] bool has_parent() const;

    // Returns the file the user picked.
    std::optional<Path> update(Message message);

  private:
    void enter(Path dir);

    Path dir_;
    std::string path_;
    std::vector<Entry> entries_;
    std::optional<std::string> error_;
    // Extensions of the files the document viewer opens.
    std::vector<std::string> extensions_;
};

// `~/rest` or `~\rest` under `home`; anything else, or no home, unchanged.
[[nodiscard]] Path expand_with(std::string_view path, const std::optional<Path>& home);

} // namespace loa::ui::surface::picker

namespace loa::ui::surface {
using FilePicker = picker::FilePicker;
}
