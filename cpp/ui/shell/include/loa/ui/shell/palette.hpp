#pragma once

// The Ctrl/Cmd+K command palette over registered `Command`s. Port of
// `loa-ui-shell/src/palette/mod.rs`; plain C++, drawn by `PaletteView`.

#include <cstddef>
#include <optional>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/kit/text.hpp"
#include "loa/ui/shell/command_id.hpp"
#include "loa/ui/shell/keymap.hpp"
#include "loa/ui/shell/task.hpp"


namespace loa::ui::shell::palette {

inline constexpr float ROW_HEIGHT = 35.0F;
inline constexpr float HEADER_HEIGHT = 35.0F;
inline constexpr float MAX_LIST_HEIGHT = 360.0F;
inline constexpr std::size_t MAX_RECENT = 32;
inline constexpr std::string_view INPUT_ID = "loa-palette-search";
inline constexpr std::string_view LIST_ID = "loa-palette-list";

struct Command {
    CommandId id;
    std::string title;
    std::string section;
    std::optional<Chord> shortcut;
    // A hint for keys that aren't one chord (e.g. `1–9`); wins over `shortcut`.
    std::optional<std::string> keys;
    bool enabled = true;

    Command(CommandId id, std::string title, std::string section)
        : id(std::move(id)), title(std::move(title)), section(std::move(section)) {}
    [[nodiscard]] Command with_shortcut(std::optional<Chord> s) && { shortcut = s; return std::move(*this); }
    [[nodiscard]] Command with_keys(std::string k) && { keys = std::move(k); return std::move(*this); }
    [[nodiscard]] Command with_enabled(bool e) && { enabled = e; return std::move(*this); }

    friend bool operator==(const Command&, const Command&) = default;
};

// A listed command, maybe under a section header, with its title runs (byte ranges, matched or
// not) and hint computed when the rows are built, so drawing does no per-row work.
struct Row {
    struct Run {
        std::size_t begin = 0, end = 0;
        bool matched = false;
        friend bool operator==(const Run&, const Run&) = default;
    };
    std::size_t command = 0;
    bool starts_section = false;
    std::vector<Run> runs;
    std::optional<std::string> hint;

    [[nodiscard]] float height() const { return ROW_HEIGHT + (starts_section ? HEADER_HEIGHT : 0.0F); }
};

namespace msg {
#define LOA_SHELL_UNIT(Name)                                                                                           \
    struct Name {                                                                                                      \
        friend bool operator==(const Name&, const Name&) = default;                                                    \
    }
LOA_SHELL_UNIT(Open);
LOA_SHELL_UNIT(OpenShortcuts);
LOA_SHELL_UNIT(Close);
LOA_SHELL_UNIT(Toggle);
LOA_SHELL_UNIT(SelectNext);
LOA_SHELL_UNIT(SelectPrevious);
LOA_SHELL_UNIT(Submit);
#undef LOA_SHELL_UNIT
struct QueryChanged {
    std::string query;
    friend bool operator==(const QueryChanged&, const QueryChanged&) = default;
};
struct Activate {
    std::size_t row = 0;
    friend bool operator==(const Activate&, const Activate&) = default;
};
struct Scrolled {
    float y = 0;
    friend bool operator==(const Scrolled&, const Scrolled&) = default;
};
} // namespace msg

using Message = std::variant<msg::Open, msg::OpenShortcuts, msg::Close, msg::Toggle, msg::QueryChanged, msg::SelectNext,
                             msg::SelectPrevious, msg::Submit, msg::Activate, msg::Scrolled>;

namespace event {
struct Run {
    CommandId id;
    friend bool operator==(const Run&, const Run&) = default;
};
// Closed without running anything; give focus back.
struct Closed {
    friend bool operator==(const Closed&, const Closed&) = default;
};
} // namespace event

using Event = std::variant<event::Run, event::Closed>;

class State {
  public:
    explicit State(Platform platform);

    void set_commands(std::vector<Command> commands);
    [[nodiscard]] const std::vector<Command>& commands() const { return commands_; }
    // While open, the app resolves keys in `Scope::Palette`.
    [[nodiscard]] bool is_open() const { return open_; }
    [[nodiscard]] bool shortcuts_only() const { return shortcuts_only_; }
    [[nodiscard]] const std::string& query() const { return query_; }
    [[nodiscard]] const std::vector<Row>& rows() const { return rows_; }
    // The listed commands, in row order.
    [[nodiscard]] std::vector<const Command*> entries() const;
    [[nodiscard]] const Command& command_of(const Row& r) const { return commands_[r.command]; }
    [[nodiscard]] std::optional<std::size_t> selected() const { return selected_; }
    [[nodiscard]] float scroll_offset() const { return scroll_offset_; }
    [[nodiscard]] std::optional<std::string> hint(const Command& c) const;
    [[nodiscard]] float list_height() const;

    std::pair<Task, std::optional<Event>> update(Message message);

    // Test hook mirroring the Rust tests' direct `state.open = true`.
    void force_open() { open_ = true; }

  private:
    Task open(bool shortcuts_only);
    Task scroll_to(float y);
    std::optional<Event> run(std::size_t index);
    Task step(bool forward);
    void refilter();

    Platform platform_;
    std::vector<Command> commands_;
    // Recently run ids, most recent first; they win ranking ties.
    std::vector<CommandId> recent_;
    bool open_ = false;
    std::string query_;
    bool shortcuts_only_ = false;
    std::vector<Row> rows_;
    std::optional<std::size_t> selected_;
    float scroll_offset_ = 0;
    kit::text::Matcher matcher_;
};

} // namespace loa::ui::shell::palette
