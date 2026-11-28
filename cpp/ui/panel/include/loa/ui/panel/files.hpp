#pragma once

// The files tab: a lazy tree listed through the `FileSystem` port on first expansion, nodes in a
// flat arena. Port of `crates/loa-ui-panel/src/files.rs`; `LocalFs` lives in `cpp/app`.

#include <cstddef>
#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/shell/task.hpp"
#include "loa/view/panel.hpp"

namespace loa::ui::panel::files {

using Task = loa::ui::shell::Task;
using Path = std::filesystem::path;
using FileSystem = loa::view::panel::port::FileSystem;
using Entry = loa::view::panel::port::Entry;

namespace msg {
// The clicked row's node index.
struct Click {
    std::size_t node = 0;
};
} // namespace msg
using Message = std::variant<msg::Click>;
using Event = loa::view::panel::FilesIntent;

// A file or directory, `rel` to the root; node 0 is the hidden root. Listed children are contiguous.
struct Node {
    std::size_t depth = 0;
    std::string name;
    Path rel;
    bool is_dir = false;
    bool expanded = false;
    std::optional<std::pair<std::size_t, std::size_t>> children; // [first, end)
    std::optional<std::string> error;
};

// A visible line: a node, or the load error under a directory.
struct Row {
    enum class Kind : std::uint8_t { Node, Error };
    Kind kind = Kind::Node;
    std::size_t node = 0;
    friend bool operator==(const Row&, const Row&) = default;
};

class State {
  public:
    // The tree with the root's children listed.
    State(Path root, std::shared_ptr<const FileSystem> fs);

    [[nodiscard]] const Path& root() const noexcept { return root_; }
    // Re-roots the tree, forgetting nodes and selection.
    void set_root(Path root);
    // Root-relative paths of the visible rows, top to bottom.
    [[nodiscard]] std::vector<Path> visible() const;
    // Re-lists every loaded directory, keeping expansion and selection where paths remain.
    void refresh();
    // A click selects a row, toggles a directory and opens a file.
    std::pair<Task, std::optional<Event>> update(Message message);

    [[nodiscard]] const std::vector<Node>& nodes() const noexcept { return nodes_; }
    [[nodiscard]] const std::vector<Row>& rows() const noexcept { return rows_; }
    [[nodiscard]] std::optional<std::size_t> selected() const noexcept { return selected_; }

  private:
    void load(std::size_t dir);
    void rebuild_rows();
    [[nodiscard]] bool shown(std::size_t node) const;

    Path root_;
    std::shared_ptr<const FileSystem> fs_;
    std::vector<Node> nodes_;
    std::vector<Row> rows_;
    std::optional<std::size_t> selected_;
};

// The issue's name for the files pane state.
using Tree = State;

} // namespace loa::ui::panel::files
