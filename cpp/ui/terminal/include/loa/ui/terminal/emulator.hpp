#pragma once

// Terminal emulation: a libvterm screen fed with PTY bytes, plus scrollback, viewport, selection and the
// modes and colour overrides libvterm does not track. Port of `crates/loa-ui-terminal/src/emulator.rs`
// (alacritty_terminal replaced by libvterm); libvterm stays out of this header.

#include <array>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <variant>
#include <vector>

#include "loa/ui/terminal/input.hpp"
#include "loa/view/terminal.hpp"

namespace loa::ui::terminal {

using GridSize = loa::view::terminal::port::GridSize;

// A visible cell: row 0 is the top visible line.
struct ViewportCell {
    std::size_t col = 0;
    std::size_t row = 0;
    friend bool operator==(const ViewportCell&, const ViewportCell&) = default;
};

// Which half of a cell the pointer is over.
enum class Side : std::uint8_t { Left, Right };
// Simple drags, semantic (word) or whole lines.
enum class SelectionType : std::uint8_t { Simple, Semantic, Lines };

struct Rgb {
    std::uint8_t r = 0, g = 0, b = 0;
    friend bool operator==(const Rgb&, const Rgb&) = default;
};

// A cell colour as the program set it: the default foreground/background, a palette index or true colour.
struct CellColor {
    enum class Kind : std::uint8_t { Foreground, Background, Indexed, Rgb };
    Kind kind = Kind::Foreground;
    std::uint8_t index = 0;
    terminal::Rgb rgb;
    friend bool operator==(const CellColor&, const CellColor&) = default;
};

// Colours the program overrode (OSC 4/10/11/12): 0..256 palette, then foreground, background, cursor.
inline constexpr std::size_t COLOR_FOREGROUND = 256, COLOR_BACKGROUND = 257, COLOR_CURSOR = 258;
using Colors = std::array<std::optional<Rgb>, 259>;

namespace flags {
inline constexpr std::uint8_t BOLD = 1, ITALIC = 2, UNDERLINE = 4, STRIKEOUT = 8, INVERSE = 16, HIDDEN = 32, DIM = 64, WIDE = 128;
} // namespace flags

// A resolved visible cell: `text` is empty for blank cells; `width` is 2 for wide characters, whose
// following cell is skipped.
struct Cell {
    std::u32string text;
    CellColor fg;
    CellColor bg{CellColor::Kind::Background, 0, {}};
    std::uint8_t flags = 0;
    std::uint8_t width = 1;
};

enum class CursorShape : std::uint8_t { Block, Underline, Beam, Hidden };
// The cursor in viewport rows (outside `0..rows` when scrolled away).
struct Cursor {
    long row = 0;
    std::size_t col = 0;
    CursorShape shape = CursorShape::Block;
};

enum class ClipboardType : std::uint8_t { Clipboard, Selection };

// What output asked of the pane: replies for the program, title changes, clipboard stores.
namespace event {
struct PtyWrite {
    Bytes bytes;
    friend bool operator==(const PtyWrite&, const PtyWrite&) = default;
};
struct Title {
    std::string title;
    friend bool operator==(const Title&, const Title&) = default;
};
struct ResetTitle {
    friend bool operator==(const ResetTitle&, const ResetTitle&) = default;
};
struct ClipboardStore {
    ClipboardType type = ClipboardType::Clipboard;
    std::string text;
    friend bool operator==(const ClipboardStore&, const ClipboardStore&) = default;
};
} // namespace event
using TermEvent = std::variant<event::PtyWrite, event::Title, event::ResetTitle, event::ClipboardStore>;

// One terminal's screen, scrollback, modes and selection.
class Emulator {
  public:
    explicit Emulator(GridSize size);
    ~Emulator();
    Emulator(const Emulator&) = delete;
    Emulator& operator=(const Emulator&) = delete;

    void advance(std::span<const std::uint8_t> bytes);
    // Replies the program expects, title changes, clipboard stores, raised by output so far.
    [[nodiscard]] std::vector<TermEvent> take_events();
    [[nodiscard]] GridSize size() const;
    // Resizes, reflowing text; false when nothing changed.
    bool resize(GridSize size);
    [[nodiscard]] TermMode mode() const;

    void scroll_display(Scroll scroll);
    // Lines scrolled back into history.
    [[nodiscard]] std::size_t display_offset() const;

    void select_start(SelectionType kind, ViewportCell cell, Side side);
    void select_update(ViewportCell cell, Side side);
    [[nodiscard]] std::optional<std::string> selection_text() const;
    [[nodiscard]] bool is_selected(ViewportCell cell) const;
    // The visible rows, trailing blanks trimmed, joined by newlines.
    [[nodiscard]] std::string screen_text() const;

    // The cell shown at a viewport position.
    [[nodiscard]] Cell cell(ViewportCell at) const;
    [[nodiscard]] Cursor cursor() const;
    [[nodiscard]] const Colors& colors() const;

  private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

} // namespace loa::ui::terminal
