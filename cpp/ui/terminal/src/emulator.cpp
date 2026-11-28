#include "loa/ui/terminal/emulator.hpp"

#include <algorithm>
#include <charconv>
#include <cstring>
#include <deque>
#include <string_view>
#include <utility>

#include <vterm.h>

namespace loa::ui::terminal {

namespace {

constexpr std::size_t SCROLLBACK = 10'000;
constexpr std::uint32_t WIDE_SPACER = 0xFFFFFFFFU;
// alacritty's default `semantic_escape_chars`.
constexpr std::u32string_view SEMANTIC_ESCAPE = U",\u2502`|:\"' ()[]{}<>\t";

int rows_of(GridSize s) { return std::max<int>(s.rows, 1); }
int cols_of(GridSize s) { return std::max<int>(s.cols, 2); }

void append_utf8(std::string& out, char32_t c) {
    if (c < 0x80) {
        out.push_back(static_cast<char>(c));
    } else if (c < 0x800) {
        out.push_back(static_cast<char>(0xC0 | (c >> 6)));
        out.push_back(static_cast<char>(0x80 | (c & 0x3F)));
    } else if (c < 0x10000) {
        out.push_back(static_cast<char>(0xE0 | (c >> 12)));
        out.push_back(static_cast<char>(0x80 | ((c >> 6) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | (c & 0x3F)));
    } else {
        out.push_back(static_cast<char>(0xF0 | (c >> 18)));
        out.push_back(static_cast<char>(0x80 | ((c >> 12) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | ((c >> 6) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | (c & 0x3F)));
    }
}

// `rgb:r/g/b` (1..4 hex digits per channel) or `#rrggbb`.
std::optional<Rgb> parse_color(std::string_view spec) {
    const auto hex = [](std::string_view s) -> std::optional<std::uint8_t> {
        if (s.empty() || s.size() > 4) return std::nullopt;
        unsigned v = 0;
        if (std::from_chars(s.data(), s.data() + s.size(), v, 16).ptr != s.data() + s.size()) return std::nullopt;
        const unsigned max = (1U << (4 * s.size())) - 1;
        return static_cast<std::uint8_t>((v * 255 + max / 2) / max);
    };
    if (spec.starts_with("rgb:")) {
        spec.remove_prefix(4);
        const auto a = spec.find('/');
        const auto b = a == std::string_view::npos ? a : spec.find('/', a + 1);
        if (b == std::string_view::npos) return std::nullopt;
        auto r = hex(spec.substr(0, a)), g = hex(spec.substr(a + 1, b - a - 1)), bl = hex(spec.substr(b + 1));
        if (!r || !g || !bl) return std::nullopt;
        return Rgb{*r, *g, *bl};
    }
    if (spec.size() == 7 && spec[0] == '#') {
        auto r = hex(spec.substr(1, 2)), g = hex(spec.substr(3, 2)), b = hex(spec.substr(5, 2));
        if (!r || !g || !b) return std::nullopt;
        return Rgb{*r, *g, *b};
    }
    return std::nullopt;
}

std::vector<std::string_view> split(std::string_view s, char sep) {
    std::vector<std::string_view> out;
    for (;;) {
        const auto at = s.find(sep);
        out.push_back(s.substr(0, at));
        if (at == std::string_view::npos) return out;
        s.remove_prefix(at + 1);
    }
}

std::optional<unsigned> number(std::string_view s) {
    unsigned v = 0;
    if (s.empty() || std::from_chars(s.data(), s.data() + s.size(), v).ptr != s.data() + s.size()) return std::nullopt;
    return v;
}

} // namespace

struct Emulator::Impl {
    struct Point {
        long line = 0;
        long col = 0;
        friend auto operator<=>(const Point&, const Point&) = default;
    };
    struct Selection {
        SelectionType kind = SelectionType::Simple;
        Point start, end;
        Side start_side = Side::Left, end_side = Side::Left;
    };

    VTerm* vt = nullptr;
    VTermScreen* screen = nullptr;
    VTermState* state = nullptr;
    GridSize size;
    std::deque<std::vector<VTermScreenCell>> scrollback;
    std::size_t offset = 0;
    std::vector<TermEvent> events;
    std::string title, clip, osc;
    bool title_open = false;
    VTermSelectionMask clip_mask{};
    char selection_buffer[4096]{};
    // Modes libvterm keeps private, tracked from the byte stream.
    bool app_cursor = false, bracketed = false, alternate_scroll = true, alt_screen = false;
    int mouse = 0;
    bool cursor_visible = true;
    int cursor_shape = VTERM_PROP_CURSORSHAPE_BLOCK;
    Colors overrides{};
    std::optional<Selection> selection;
    // The mode scanner: ground, after ESC, inside CSI.
    enum class Scan : std::uint8_t { Ground, Escape, Csi } scan = Scan::Ground;
    std::string csi;

    explicit Impl(GridSize s) : size(s) {
        vt = vterm_new(rows_of(s), cols_of(s));
        vterm_set_utf8(vt, 1);
        vterm_output_set_callback(vt, &Impl::on_output, this);
        screen = vterm_obtain_screen(vt);
        static const VTermScreenCallbacks callbacks{
            .damage = nullptr, .moverect = nullptr, .movecursor = nullptr, .settermprop = &Impl::on_prop, .bell = nullptr,
            .resize = nullptr, .sb_pushline = &Impl::on_push, .sb_popline = &Impl::on_pop, .sb_clear = &Impl::on_sb_clear};
        vterm_screen_set_callbacks(screen, &callbacks, this);
        static const VTermStateFallbacks fallbacks{
            .control = nullptr, .csi = nullptr, .osc = &Impl::on_osc, .dcs = nullptr, .apc = nullptr, .pm = nullptr, .sos = nullptr};
        vterm_screen_set_unrecognised_fallbacks(screen, &fallbacks, this);
        vterm_screen_enable_altscreen(screen, 1);
        vterm_screen_enable_reflow(screen, true);
        vterm_screen_reset(screen, 1);
        state = vterm_obtain_state(vt);
        static const VTermSelectionCallbacks selection_callbacks{.set = &Impl::on_selection, .query = nullptr};
        vterm_state_set_selection_callbacks(state, &selection_callbacks, this, selection_buffer, sizeof selection_buffer);
    }
    ~Impl() { vterm_free(vt); }

    [[nodiscard]] int rows() const { return rows_of(size); }
    [[nodiscard]] int cols() const { return cols_of(size); }

    static void on_output(const char* s, std::size_t len, void* user) {
        auto& self = *static_cast<Impl*>(user);
        if (!self.events.empty())
            if (auto* last = std::get_if<event::PtyWrite>(&self.events.back())) {
                last->bytes.insert(last->bytes.end(), s, s + len);
                return;
            }
        self.events.emplace_back(event::PtyWrite{Bytes(s, s + len)});
    }
    static int on_prop(VTermProp prop, VTermValue* val, void* user) {
        auto& self = *static_cast<Impl*>(user);
        switch (prop) {
        case VTERM_PROP_ALTSCREEN:
            self.alt_screen = val->boolean != 0;
            self.offset = 0;
            self.selection.reset();
            break;
        case VTERM_PROP_MOUSE: self.mouse = val->number; break;
        case VTERM_PROP_CURSORVISIBLE: self.cursor_visible = val->boolean != 0; break;
        case VTERM_PROP_CURSORSHAPE: self.cursor_shape = val->number; break;
        case VTERM_PROP_TITLE:
            if (val->string.initial) self.title.clear();
            self.title.append(val->string.str, val->string.len);
            if (val->string.final) {
                if (self.title.empty()) self.events.emplace_back(event::ResetTitle{});
                else self.events.emplace_back(event::Title{self.title});
            }
            break;
        default: break;
        }
        return 1;
    }
    static int on_push(int cols, const VTermScreenCell* cells, void* user) {
        auto& self = *static_cast<Impl*>(user);
        self.scrollback.emplace_back(cells, cells + cols);
        if (self.scrollback.size() > SCROLLBACK) {
            self.scrollback.pop_front();
            if (self.selection) {
                --self.selection->start.line;
                --self.selection->end.line;
                if (self.selection->start.line < 0 || self.selection->end.line < 0) self.selection.reset();
            }
        } else if (self.offset > 0) {
            // The viewport stays on the lines it shows while output arrives.
            self.offset = std::min(self.offset + 1, self.scrollback.size());
        }
        return 1;
    }
    static int on_pop(int cols, VTermScreenCell* cells, void* user) {
        auto& self = *static_cast<Impl*>(user);
        if (self.scrollback.empty()) return 0;
        const auto& line = self.scrollback.back();
        const VTermScreenCell blank = self.blank();
        for (int i = 0; i < cols; ++i) cells[i] = static_cast<std::size_t>(i) < line.size() ? line[static_cast<std::size_t>(i)] : blank;
        self.scrollback.pop_back();
        self.offset = std::min(self.offset, self.scrollback.size());
        return 1;
    }
    static int on_sb_clear(void* user) {
        auto& self = *static_cast<Impl*>(user);
        self.scrollback.clear();
        self.offset = 0;
        self.selection.reset();
        return 1;
    }
    static int on_selection(VTermSelectionMask mask, VTermStringFragment frag, void* user) {
        auto& self = *static_cast<Impl*>(user);
        if (frag.initial) {
            self.clip.clear();
            self.clip_mask = mask;
        }
        self.clip.append(frag.str, frag.len);
        if (frag.final) {
            const auto type = (self.clip_mask & VTERM_SELECTION_CLIPBOARD) != 0 ? ClipboardType::Clipboard : ClipboardType::Selection;
            self.events.emplace_back(event::ClipboardStore{type, self.clip});
        }
        return 1;
    }
    static int on_osc(int command, VTermStringFragment frag, void* user) {
        auto& self = *static_cast<Impl*>(user);
        if (command != 4 && command != 10 && command != 11 && command != 12 && command != 104 && command != 110 && command != 111 &&
            command != 112)
            return 0;
        if (frag.initial) self.osc.clear();
        self.osc.append(frag.str, frag.len);
        if (frag.final) self.apply_osc(command, self.osc);
        return 1;
    }

    // OSC 4/10/11/12 set colours, 104/110/111/112 reset them; queries (`?`) are ignored.
    void apply_osc(int command, std::string_view body) {
        switch (command) {
        case 4: {
            const auto parts = split(body, ';');
            for (std::size_t i = 0; i + 1 < parts.size(); i += 2) {
                const auto index = number(parts[i]);
                if (index && *index < 256)
                    if (auto rgb = parse_color(parts[i + 1])) overrides[*index] = *rgb;
            }
            break;
        }
        case 10:
        case 11:
        case 12:
            if (auto rgb = parse_color(body)) overrides[COLOR_FOREGROUND + static_cast<std::size_t>(command - 10)] = *rgb;
            break;
        case 104:
            if (body.empty()) {
                std::fill(overrides.begin(), overrides.begin() + 256, std::nullopt);
            } else {
                for (const auto part : split(body, ';'))
                    if (const auto index = number(part); index && *index < 256) overrides[*index].reset();
            }
            break;
        default: overrides[COLOR_FOREGROUND + static_cast<std::size_t>(command - 110)].reset(); break;
        }
    }

    // DECSET/DECRST of the modes the pane needs (1, 2004, 1007), and RIS.
    void scan_modes(std::span<const std::uint8_t> bytes) {
        for (const auto b : bytes) {
            switch (scan) {
            case Scan::Ground:
                if (b == 0x1b) scan = Scan::Escape;
                break;
            case Scan::Escape:
                if (b == '[') {
                    scan = Scan::Csi;
                    csi.clear();
                } else {
                    if (b == 'c') app_cursor = bracketed = false, alternate_scroll = true;
                    scan = b == 0x1b ? Scan::Escape : Scan::Ground;
                }
                break;
            case Scan::Csi:
                if (b >= 0x40 && b <= 0x7e) {
                    if ((b == 'h' || b == 'l') && csi.starts_with('?')) {
                        for (const auto p : split(std::string_view(csi).substr(1), ';')) {
                            const auto n = number(p);
                            if (!n) continue;
                            if (*n == 1) app_cursor = b == 'h';
                            else if (*n == 2004) bracketed = b == 'h';
                            else if (*n == 1007) alternate_scroll = b == 'h';
                        }
                    }
                    scan = Scan::Ground;
                } else if (b == 0x1b) {
                    scan = Scan::Escape;
                } else if (csi.size() < 64) {
                    csi.push_back(static_cast<char>(b));
                }
                break;
            }
        }
    }

    [[nodiscard]] VTermScreenCell blank() const {
        VTermScreenCell c{};
        c.width = 1;
        VTermColor fg, bg;
        vterm_state_get_default_colors(state, &fg, &bg);
        c.fg = fg;
        c.bg = bg;
        return c;
    }
    [[nodiscard]] long total_lines() const { return static_cast<long>(scrollback.size()) + rows(); }
    [[nodiscard]] long line_of(std::size_t row) const {
        return static_cast<long>(scrollback.size()) - static_cast<long>(offset) + static_cast<long>(row);
    }
    [[nodiscard]] VTermScreenCell raw(long line, long col) const {
        if (line < 0 || col < 0 || col >= cols() || line >= total_lines()) return blank();
        if (line < static_cast<long>(scrollback.size())) {
            const auto& l = scrollback[static_cast<std::size_t>(line)];
            return static_cast<std::size_t>(col) < l.size() ? l[static_cast<std::size_t>(col)] : blank();
        }
        VTermScreenCell c{};
        vterm_screen_get_cell(screen, VTermPos{static_cast<int>(line - static_cast<long>(scrollback.size())), static_cast<int>(col)}, &c);
        return c;
    }
    [[nodiscard]] static char32_t first_char(const VTermScreenCell& c) {
        return c.chars[0] == WIDE_SPACER ? 0 : static_cast<char32_t>(c.chars[0]);
    }
    // Whether the screen line continues the one above (a soft wrap).
    [[nodiscard]] bool continues(long line) const {
        const long row = line - static_cast<long>(scrollback.size());
        if (row <= 0 || row >= rows()) return false;
        const auto* info = vterm_state_get_lineinfo(state, static_cast<int>(row));
        return info && info->continuation;
    }
    [[nodiscard]] Point point(ViewportCell cell) const {
        const auto row = std::min<std::size_t>(cell.row, static_cast<std::size_t>(rows() - 1));
        const auto col = std::min<std::size_t>(cell.col, static_cast<std::size_t>(cols() - 1));
        return {line_of(row), static_cast<long>(col)};
    }
    [[nodiscard]] bool separator(long line, long col) const {
        const auto c = first_char(raw(line, col));
        return c == 0 || SEMANTIC_ESCAPE.find(c) != std::u32string_view::npos;
    }

    // The selected cells, first and last inclusive.
    [[nodiscard]] std::optional<std::pair<Point, Point>> range() const {
        if (!selection) return std::nullopt;
        auto [kind, a, b, as, bs] = *selection;
        if (b < a || (a == b && as == Side::Right && bs == Side::Left)) {
            std::swap(a, b);
            std::swap(as, bs);
        }
        switch (kind) {
        case SelectionType::Lines: return std::pair{Point{a.line, 0}, Point{b.line, cols() - 1}};
        case SelectionType::Semantic:
            if (!separator(a.line, a.col))
                while (a.col > 0 && !separator(a.line, a.col - 1)) --a.col;
            if (!separator(b.line, b.col))
                while (b.col + 1 < cols() && !separator(b.line, b.col + 1)) ++b.col;
            return std::pair{a, b};
        case SelectionType::Simple:
            if (as == Side::Right && ++a.col >= cols()) a = {a.line + 1, 0};
            if (bs == Side::Left && --b.col < 0) b = {b.line - 1, cols() - 1};
            if (b < a) return std::nullopt;
            return std::pair{a, b};
        }
        return std::nullopt;
    }

    // Text of `line` from `first` to `last` (inclusive); trailing blanks trimmed when it reaches the line end.
    [[nodiscard]] std::string line_text(long line, long first, long last) const {
        std::string out;
        for (long col = first; col <= last; ++col) {
            const auto c = raw(line, col);
            if (c.chars[0] == WIDE_SPACER) continue;
            if (c.chars[0] == 0) {
                out.push_back(' ');
                continue;
            }
            for (const auto ch : c.chars) {
                if (ch == 0) break;
                append_utf8(out, static_cast<char32_t>(ch));
            }
            if (c.width == 2) ++col;
        }
        if (last >= cols() - 1)
            while (!out.empty() && out.back() == ' ') out.pop_back();
        return out;
    }
};

Emulator::Emulator(GridSize size) : impl_(std::make_unique<Impl>(size)) {}
Emulator::~Emulator() = default;

void Emulator::advance(std::span<const std::uint8_t> bytes) {
    impl_->scan_modes(bytes);
    if (!bytes.empty()) vterm_input_write(impl_->vt, reinterpret_cast<const char*>(bytes.data()), bytes.size());
}

std::vector<TermEvent> Emulator::take_events() { return std::exchange(impl_->events, {}); }

GridSize Emulator::size() const { return impl_->size; }

bool Emulator::resize(GridSize size) {
    if (size == impl_->size) return false;
    const bool cells = size.cols != impl_->size.cols || size.rows != impl_->size.rows;
    impl_->size = size;
    if (cells) {
        impl_->selection.reset();
        vterm_set_size(impl_->vt, rows_of(size), cols_of(size));
        impl_->offset = std::min(impl_->offset, impl_->scrollback.size());
    }
    return true;
}

TermMode Emulator::mode() const {
    TermMode m;
    if (impl_->app_cursor) m.bits |= TermMode::APP_CURSOR;
    if (impl_->bracketed) m.bits |= TermMode::BRACKETED_PASTE;
    if (impl_->alt_screen) m.bits |= TermMode::ALT_SCREEN;
    if (impl_->alternate_scroll) m.bits |= TermMode::ALTERNATE_SCROLL;
    if (impl_->mouse != VTERM_PROP_MOUSE_NONE) m.bits |= TermMode::MOUSE_MODE;
    return m;
}

void Emulator::scroll_display(Scroll scroll) {
    auto& i = *impl_;
    const long max = i.alt_screen ? 0 : static_cast<long>(i.scrollback.size());
    long offset = static_cast<long>(i.offset);
    switch (scroll.kind) {
    case Scroll::Kind::Delta: offset += scroll.delta; break;
    case Scroll::Kind::PageUp: offset += i.rows(); break;
    case Scroll::Kind::PageDown: offset -= i.rows(); break;
    case Scroll::Kind::Top: offset = max; break;
    case Scroll::Kind::Bottom: offset = 0; break;
    }
    i.offset = static_cast<std::size_t>(std::clamp(offset, 0L, max));
}

std::size_t Emulator::display_offset() const { return impl_->offset; }

void Emulator::select_start(SelectionType kind, ViewportCell cell, Side side) {
    const auto p = impl_->point(cell);
    impl_->selection = Impl::Selection{kind, p, p, side, side};
}

void Emulator::select_update(ViewportCell cell, Side side) {
    if (!impl_->selection) return;
    impl_->selection->end = impl_->point(cell);
    impl_->selection->end_side = side;
}

std::optional<std::string> Emulator::selection_text() const {
    const auto r = impl_->range();
    if (!r) return std::nullopt;
    const auto [a, b] = *r;
    std::string out;
    for (long line = a.line; line <= b.line; ++line) {
        if (line > a.line && !impl_->continues(line)) out.push_back('\n');
        out += impl_->line_text(line, line == a.line ? a.col : 0, line == b.line ? b.col : impl_->cols() - 1);
    }
    if (out.empty()) return std::nullopt;
    return out;
}

bool Emulator::is_selected(ViewportCell cell) const {
    const auto r = impl_->range();
    if (!r) return false;
    const Impl::Point p{impl_->line_of(cell.row), static_cast<long>(cell.col)};
    return r->first <= p && p <= r->second;
}

std::string Emulator::screen_text() const {
    std::string out;
    for (int row = 0; row < impl_->rows(); ++row) {
        if (row > 0) out.push_back('\n');
        out += impl_->line_text(impl_->line_of(static_cast<std::size_t>(row)), 0, impl_->cols() - 1);
    }
    return out;
}

Cell Emulator::cell(ViewportCell at) const {
    const auto c = impl_->raw(impl_->line_of(at.row), static_cast<long>(at.col));
    Cell out;
    if (c.chars[0] != WIDE_SPACER)
        for (const auto ch : c.chars) {
            if (ch == 0) break;
            out.text.push_back(static_cast<char32_t>(ch));
        }
    const auto color = [](const VTermColor& v, CellColor::Kind dflt) {
        if ((dflt == CellColor::Kind::Foreground && VTERM_COLOR_IS_DEFAULT_FG(&v)) ||
            (dflt == CellColor::Kind::Background && VTERM_COLOR_IS_DEFAULT_BG(&v)))
            return CellColor{dflt, 0, {}};
        if (VTERM_COLOR_IS_INDEXED(&v)) return CellColor{CellColor::Kind::Indexed, v.indexed.idx, {}};
        return CellColor{CellColor::Kind::Rgb, 0, Rgb{v.rgb.red, v.rgb.green, v.rgb.blue}};
    };
    out.fg = color(c.fg, CellColor::Kind::Foreground);
    out.bg = color(c.bg, CellColor::Kind::Background);
    if (c.attrs.bold) out.flags |= flags::BOLD;
    if (c.attrs.italic) out.flags |= flags::ITALIC;
    if (c.attrs.underline) out.flags |= flags::UNDERLINE;
    if (c.attrs.strike) out.flags |= flags::STRIKEOUT;
    if (c.attrs.reverse) out.flags |= flags::INVERSE;
    if (c.attrs.conceal) out.flags |= flags::HIDDEN;
    out.width = c.width == 2 ? 2 : 1;
    if (out.width == 2) out.flags |= flags::WIDE;
    return out;
}

Cursor Emulator::cursor() const {
    VTermPos pos{};
    vterm_state_get_cursorpos(impl_->state, &pos);
    Cursor c;
    c.row = pos.row + static_cast<long>(impl_->offset);
    c.col = static_cast<std::size_t>(std::max(pos.col, 0));
    if (!impl_->cursor_visible) c.shape = CursorShape::Hidden;
    else if (impl_->cursor_shape == VTERM_PROP_CURSORSHAPE_UNDERLINE) c.shape = CursorShape::Underline;
    else if (impl_->cursor_shape == VTERM_PROP_CURSORSHAPE_BAR_LEFT) c.shape = CursorShape::Beam;
    return c;
}

const Colors& Emulator::colors() const { return impl_->overrides; }

} // namespace loa::ui::terminal
