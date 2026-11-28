#include "loa/surfaces/docs/docx.hpp"

#include <algorithm>
#include <charconv>
#include <cmath>
#include <limits>
#include <map>
#include <set>
#include <string_view>
#include <tuple>
#include <unordered_map>

#include <pugixml.hpp>

#include "loa/surfaces/docs/ooxml.hpp"

// The reader follows `docx-rs` 0.4.22 `read_docx`: which parts it loads, which
// elements each of its streaming readers consumes (everything else is scanned
// through, so content nested in unknown elements still surfaces), its value
// parsing and its errors. Its `ReaderError` texts become `DocsError::Invalid`
// messages; what panics inside docx-rs (indexing a missing first attribute)
// becomes "malformed document part".

namespace loa::surfaces::docs {

namespace docx {

namespace {

// --- docx-rs reader errors -------------------------------------------------

// A `ReaderError`, by its Display text.
struct Fatal {
    std::string message;
};
// A panic inside docx-rs.
struct Panic {};

constexpr std::string_view ZIP_ERROR = "Failed to read from zip.";
constexpr std::string_view NUM_ERROR = "Failed to parse int.";
constexpr std::string_view FLOAT_ERROR = "Failed to parse float.";
constexpr std::string_view TYPE_ERROR = "Failed to convert type.";
constexpr std::string_view XML_ERROR = "Failed to read xml.";
constexpr std::string_view RELS_NOT_FOUND = "Failed to find document rels.";

[[noreturn]] void fail(std::string_view message) { throw Fatal{std::string(message)}; }

constexpr std::string_view DOC_RELATIONSHIP_TYPE =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
constexpr std::string_view STYLE_RELATIONSHIP_TYPE = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
constexpr std::string_view NUMBERING_RELATIONSHIP_TYPE =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
constexpr std::string_view SETTINGS_TYPE = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings";
constexpr std::string_view WEB_SETTINGS_TYPE = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/webSettings";
constexpr std::string_view COMMENTS_TYPE = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
constexpr std::string_view COMMENTS_EXTENDED_TYPE = "http://schemas.microsoft.com/office/2011/relationships/commentsExtended";

// --- XML and value helpers ---------------------------------------------------

std::string_view lname(const pugi::xml_node& n) { return ooxml::local_name(n); }

std::string_view element_prefix(const pugi::xml_node& n) {
    const std::string_view name = n.name();
    const auto colon = name.find(':');
    return colon == std::string_view::npos ? std::string_view{} : name.substr(0, colon);
}

bool is_namespace_decl(const pugi::xml_attribute& a) {
    const std::string_view name = a.name();
    return name == "xmlns" || name.starts_with("xmlns:");
}

// `attributes[0].value`, which panics in docx-rs when the element has none.
std::string_view first_attr(const pugi::xml_node& n) {
    for (const auto& a : n.attributes())
        if (!is_namespace_decl(a)) return a.value();
    throw Panic{};
}

std::optional<std::string_view> read(const pugi::xml_node& n, std::string_view local) { return ooxml::attr(n, local); }

bool is_false(std::string_view v) {
    const auto ieq = [](std::string_view a, std::string_view b) {
        return a.size() == b.size() && std::equal(a.begin(), a.end(), b.begin(), [](char x, char y) {
                   return std::tolower(static_cast<unsigned char>(x)) == std::tolower(static_cast<unsigned char>(y));
               });
    };
    return v == "0" || ieq(v, "false") || ieq(v, "off");
}

// `w:val`, else the first attribute; true without attributes.
bool read_bool(const pugi::xml_node& n) {
    if (auto v = read(n, "val")) return !is_false(*v);
    for (const auto& a : n.attributes())
        if (!is_namespace_decl(a)) return !is_false(a.value());
    return true;
}

// Rust's `usize::from_str`.
std::optional<std::size_t> parse_usize(std::string_view s) {
    if (s.starts_with('+')) s.remove_prefix(1);
    if (s.empty()) return std::nullopt;
    std::size_t v = 0;
    const auto [end, ec] = std::from_chars(s.data(), s.data() + s.size(), v);
    if (ec != std::errc{} || end != s.data() + s.size()) return std::nullopt;
    return v;
}

std::optional<std::uint32_t> parse_u32(std::string_view s) {
    auto v = parse_usize(s);
    if (!v || *v > std::numeric_limits<std::uint32_t>::max()) return std::nullopt;
    return static_cast<std::uint32_t>(*v);
}

// Rust's `f64::from_str`.
std::optional<double> parse_f64(std::string_view s) {
    if (s.starts_with('+')) {
        s.remove_prefix(1);
        if (s.starts_with('-') || s.starts_with('+')) return std::nullopt;
    }
    if (s.empty()) return std::nullopt;
    double v = 0;
    const auto [end, ec] = std::from_chars(s.data(), s.data() + s.size(), v);
    if (end != s.data() + s.size()) return std::nullopt;
    if (ec == std::errc::result_out_of_range) {
        // Rust rounds to infinity or zero instead of failing.
        const bool negative = s.starts_with('-');
        const auto e = s.find_first_of("eE");
        const bool tiny = e != std::string_view::npos && s.substr(e + 1).starts_with('-');
        if (tiny) return negative ? -0.0 : 0.0;
        return negative ? -std::numeric_limits<double>::infinity() : std::numeric_limits<double>::infinity();
    }
    if (ec != std::errc{}) return std::nullopt;
    return v;
}

// Rust's saturating `f64 as i32`.
std::int32_t to_i32(double v) {
    if (std::isnan(v)) return 0;
    if (v <= static_cast<double>(std::numeric_limits<std::int32_t>::min())) return std::numeric_limits<std::int32_t>::min();
    if (v >= static_cast<double>(std::numeric_limits<std::int32_t>::max())) return std::numeric_limits<std::int32_t>::max();
    return static_cast<std::int32_t>(v);
}

double float_or_fail(std::string_view s) {
    auto v = parse_f64(s);
    if (!v) fail(FLOAT_ERROR);
    return *v;
}

// docx-rs `value_to_dax`: twips, or points with a `pt` suffix.
std::int32_t value_to_dax(std::string_view v) {
    if (v.ends_with("pt")) {
        std::string stripped;
        for (std::size_t i = 0; i < v.size();) {
            if (v.substr(i).starts_with("pt")) {
                i += 2;
            } else {
                stripped.push_back(v[i++]);
            }
        }
        return static_cast<std::int32_t>(static_cast<std::uint32_t>(to_i32(float_or_fail(stripped))) * 20U);
    }
    return to_i32(float_or_fail(v));
}

// docx-rs `escape_owned`, applied to style ids, names and references.
std::string escape_owned(std::string_view text) {
    std::string out;
    out.reserve(text.size());
    for (const char c : text) {
        switch (c) {
        case '&': out += "&amp;"; break;
        case '<': out += "&lt;"; break;
        case '>': out += "&gt;"; break;
        case '"': out += "&quot;"; break;
        case '\'': out += "&apos;"; break;
        case '\n': out += "&#xA;"; break;
        case '\r': break;
        default: out.push_back(c);
        }
    }
    return out;
}

// docx-rs `replace_escaped`, which run text goes through after XML decoding.
void append_unescaped(std::string& out, std::string_view rest) {
    static constexpr std::pair<std::string_view, std::string_view> ENTITIES[] = {
        {"&lt;", "<"}, {"&gt;", ">"}, {"&amp;", "&"}, {"&quot;", "\""}, {"&#39;", "'"}, {"&apos;", "'"}, {"&nbsp;", " "},
    };
    while (true) {
        const auto amp = rest.find('&');
        if (amp == std::string_view::npos) break;
        out.append(rest.substr(0, amp));
        rest.remove_prefix(amp);
        std::size_t consumed = 1;
        std::string_view replacement = "&";
        for (const auto& [entity, text] : ENTITIES) {
            if (rest.starts_with(entity)) {
                replacement = text;
                consumed = entity.size();
                break;
            }
        }
        out.append(replacement);
        rest.remove_prefix(consumed);
    }
    out.append(rest);
}

// `read_width`: panics on an unparsable width.
void read_width(const pugi::xml_node& n) {
    for (const auto& a : n.attributes()) {
        if (ooxml::local_name(a) != "w") continue;
        std::string v(a.value());
        std::erase(v, '%');
        if (!parse_f64(v)) throw Panic{};
    }
}

// `read_indent`: fails on an unparsable value.
void read_indent(const pugi::xml_node& n) {
    for (const auto& a : n.attributes()) {
        const std::string_view name = ooxml::local_name(a);
        if (name == "left" || name == "start" || name == "end" || name == "right" || name == "hanging" || name == "firstLine") {
            (void)value_to_dax(a.value());
        } else if (name == "leftChars" || name == "startChars") {
            (void)float_or_fail(a.value());
        }
    }
}

// --- What docx-rs reads ------------------------------------------------------

struct RunProps {
    std::optional<std::string> style;
    std::optional<bool> bold, italic, underline;
};

struct NumPr {
    std::optional<std::size_t> id, level;
};

struct ParaProps {
    std::optional<std::string> style;
    std::optional<NumPr> numbering;
    bool page_break_before = false;
};

struct RawRun {
    std::string text;
    bool page_break = false;
    RunProps props;
};

struct RawPara {
    ParaProps props;
    // The runs `each_run` visits: direct, inserted, moved to and hyperlinked.
    std::vector<RawRun> runs;
};

struct Content;
struct RawTable {
    // Each cell's content.
    std::vector<std::vector<std::vector<Content>>> rows;
};

// A paragraph, a table or (in cells) a content control's paragraphs and tables.
struct Content {
    enum class Kind : std::uint8_t { Paragraph, Table, Sdt } kind = Kind::Paragraph;
    RawPara paragraph;
    std::vector<RawTable> table; // one for `Table`
    std::vector<Content> sdt;
};

struct Section {
    std::uint32_t w = 11906, h = 16838;
    std::int32_t top = 1985, left = 1701, bottom = 1701, right = 1701, header = 851, footer = 992, gutter = 0;
    // Anything else docx-rs records (grid, references, type, title page, numbering).
    bool extra = false;

    [[nodiscard]] bool is_default() const {
        const Section d;
        return std::tie(w, h, top, left, bottom, right, header, footer, gutter, extra) ==
               std::tie(d.w, d.h, d.top, d.left, d.bottom, d.right, d.header, d.footer, d.gutter, d.extra);
    }
};

struct Style {
    std::string id;
    std::string name;
    std::optional<std::string> based_on;
    ParaProps paragraph;
    RunProps run;
};

struct Level {
    std::size_t level = 0;
    // `Start` serializes as `val as u32`.
    std::uint32_t start = 0;
    std::string format = "decimal";
    std::string text;
};

struct LevelOverride {
    std::size_t level = 0;
    std::optional<std::size_t> start;
    std::optional<Level> definition;
};

struct Num {
    std::size_t id = 0;
    std::size_t abstract_id = 0;
    std::vector<LevelOverride> overrides;
};

struct AbstractNum {
    std::size_t id = 0;
    std::vector<Level> levels;
};

struct Numberings {
    std::vector<AbstractNum> abstract_nums;
    std::vector<Num> nums;
};

// Calls `f` on the element children of `n`; `f` returns whether it consumed
// the child, else the scan goes through it as docx-rs's flat event loops do.
template <typename F>
void scan(const pugi::xml_node& n, F&& f) {
    for (const auto& child : n.children()) {
        if (child.type() != pugi::node_element) continue;
        if (!f(child)) scan(child, f);
    }
}

RunProps read_run_props(const pugi::xml_node& node) {
    RunProps rp;
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "rPrChange") return true;
        if (name == "rStyle") {
            if (auto v = read(e, "val")) rp.style = escape_owned(*v);
        } else if (name == "b") {
            rp.bold = read_bool(e);
        } else if (name == "i" || name == "italic") {
            rp.italic = read_bool(e);
        } else if (name == "u") {
            rp.underline = first_attr(e) != "none";
        } else if (name == "highlight" || name == "vertAlign") {
            (void)first_attr(e);
        } else if (name == "sz") {
            (void)float_or_fail(first_attr(e));
        } else if (name == "ptab" || name == "shd" || name == "ins" || name == "del") {
            return true;
        }
        return false;
    });
    return rp;
}

// `None` where docx-rs's reader fails.
std::optional<NumPr> read_num_pr(const pugi::xml_node& node) {
    NumPr np;
    bool failed = false;
    scan(node, [&](const pugi::xml_node& e) {
        if (failed) return true;
        const auto name = lname(e);
        if (name == "ilvl" || name == "numId") {
            auto v = parse_usize(first_attr(e));
            if (!v) {
                failed = true;
            } else {
                (name == "ilvl" ? np.level : np.id) = *v;
            }
            return true;
        }
        return false;
    });
    if (failed) return std::nullopt;
    return np;
}

std::optional<ParaProps> read_para_props(const pugi::xml_node& node) {
    ParaProps pp;
    try {
        scan(node, [&](const pugi::xml_node& e) {
            const auto name = lname(e);
            if (name == "ind") {
                read_indent(e);
                return true;
            }
            if (name == "spacing") return true;
            if (name == "jc" || name == "textAlignment") {
                (void)first_attr(e);
                return true;
            }
            if (name == "pStyle") {
                pp.style = escape_owned(first_attr(e));
                return true;
            }
            if (name == "rPr") {
                try {
                    (void)read_run_props(e);
                } catch (const Fatal&) {
                }
                return true;
            }
            if (name == "numPr") {
                if (auto np = read_num_pr(e)) pp.numbering = *np;
                return true;
            }
            if (name == "pageBreakBefore") {
                if (read_bool(e)) pp.page_break_before = true;
                return false;
            }
            return name == "pPrChange" || name == "sectPr" || name == "framePr" || name == "shd" || name == "pBdr" ||
                   name == "tabs";
        });
    } catch (const Fatal&) {
        return std::nullopt;
    }
    return pp;
}

void scan_run(const pugi::xml_node& node, RawRun& run, bool in_text) {
    for (const auto& child : node.children()) {
        const auto type = child.type();
        if (type == pugi::node_pcdata || type == pugi::node_cdata) {
            if (in_text) append_unescaped(run.text, child.value());
            continue;
        }
        if (type != pugi::node_element) continue;
        const auto prefix = element_prefix(child);
        const auto name = lname(child);
        bool text = in_text;
        if (prefix == "w") {
            if (name == "rPrChange" || name == "drawing" || name == "instrText") continue;
            if (name == "tab") {
                run.text.push_back('\t');
            } else if (name == "ptab") {
                run.text.push_back('\t');
                continue;
            } else if (name == "rPr") {
                run.props = read_run_props(child);
                continue;
            } else if (name == "t") {
                text = true;
            } else if (name == "delText") {
                text = false;
            } else if (name == "br") {
                std::string_view kind = "textWrapping";
                for (const auto& a : child.attributes()) {
                    if (is_namespace_decl(a)) continue;
                    kind = a.value();
                    break;
                }
                if (kind != "page" && kind != "column" && kind != "textWrapping") fail(TYPE_ERROR);
                run.page_break |= kind == "page";
                run.text.push_back('\n');
            } else if (name == "cr") {
                run.text.push_back('\n');
            }
        } else if ((prefix == "mc" && name == "Fallback") || (prefix == "v" && name == "shape")) {
            continue;
        }
        scan_run(child, run, text);
    }
}

RawRun read_run(const pugi::xml_node& node) {
    RawRun run;
    scan_run(node, run, false);
    return run;
}

// Reads the runs of a deletion or a move away, which only matter for errors.
void read_removed_runs(const pugi::xml_node& node) {
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "r") {
            (void)read_run(e);
            return true;
        }
        return name == "moveFrom";
    });
}

// Runs of an insertion or move: its own runs, not its deletions.
void read_inserted_runs(const pugi::xml_node& node, std::vector<RawRun>& runs) {
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "r") {
            runs.push_back(read_run(e));
            return true;
        }
        if (name == "del") {
            read_removed_runs(e);
            return true;
        }
        return name == "moveFrom" || name == "commentRangeStart" || name == "commentRangeEnd";
    });
}

// A hyperlink drops the parts docx-rs fails to read.
void read_hyperlink_runs(const pugi::xml_node& node, std::vector<RawRun>& runs) {
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        try {
            if (name == "r") {
                runs.push_back(read_run(e));
            } else if (name == "ins") {
                std::vector<RawRun> inserted;
                read_inserted_runs(e, inserted);
                std::move(inserted.begin(), inserted.end(), std::back_inserter(runs));
            } else if (name == "del") {
                read_removed_runs(e);
            } else {
                return name == "bookmarkStart" || name == "bookmarkEnd" || name == "commentRangeStart" ||
                       name == "commentRangeEnd" || name == "moveFrom";
            }
        } catch (const Fatal&) {
        }
        return true;
    });
}

RawPara read_paragraph(const pugi::xml_node& node) {
    RawPara p;
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "r") {
            p.runs.push_back(read_run(e));
        } else if (name == "hyperlink") {
            read_hyperlink_runs(e, p.runs);
        } else if (name == "ins" || name == "moveTo") {
            read_inserted_runs(e, p.runs);
        } else if (name == "del" || name == "moveFrom") {
            read_removed_runs(e);
        } else if (name == "pPr") {
            if (auto props = read_para_props(e)) p.props = std::move(*props);
        } else {
            return name == "bookmarkStart" || name == "bookmarkEnd" || name == "commentRangeStart" ||
                   name == "commentRangeEnd";
        }
        return true;
    });
    return p;
}

RawTable read_table(const pugi::xml_node& node);

// A content control's paragraphs and tables; parts that fail to read are dropped.
std::vector<Content> read_sdt(const pugi::xml_node& node) {
    std::vector<Content> out;
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        try {
            if (name == "p") {
                out.push_back(Content{Content::Kind::Paragraph, read_paragraph(e), {}, {}});
            } else if (name == "tbl") {
                Content c{Content::Kind::Table, {}, {}, {}};
                c.table.push_back(read_table(e));
                out.push_back(std::move(c));
            } else if (name == "r") {
                (void)read_run(e);
            } else {
                return name == "bookmarkStart" || name == "bookmarkEnd" || name == "commentRangeStart" ||
                       name == "commentRangeEnd";
            }
        } catch (const Fatal&) {
        }
        return true;
    });
    return out;
}

std::vector<Content> read_cell(const pugi::xml_node& node) {
    std::vector<Content> out;
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "p") {
            out.push_back(Content{Content::Kind::Paragraph, read_paragraph(e), {}, {}});
        } else if (name == "sdt") {
            try {
                Content c{Content::Kind::Sdt, {}, {}, read_sdt(e)};
                out.push_back(std::move(c));
            } catch (const Fatal&) {
            }
        } else if (name == "tbl") {
            try {
                Content c{Content::Kind::Table, {}, {}, {}};
                c.table.push_back(read_table(e));
                out.push_back(std::move(c));
            } catch (const Fatal&) {
            }
        } else {
            return name == "tcPr";
        }
        return true;
    });
    return out;
}

std::vector<std::vector<Content>> read_row(const pugi::xml_node& node) {
    std::vector<std::vector<Content>> cells;
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "trPrChange" || name == "del" || name == "ins") return true;
        if (name == "tc") {
            cells.push_back(read_cell(e));
            return true;
        }
        if (name == "gridAfter" || name == "gridBefore") {
            if (auto v = read(e, "val"); v && !parse_u32(*v)) fail(NUM_ERROR);
        } else if (name == "wAfter" || name == "wBefore") {
            read_width(e);
        }
        return false;
    });
    return cells;
}

RawTable read_table(const pugi::xml_node& node) {
    RawTable t;
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "tblPrChange" || name == "tblGridChange" || name == "tblPr" || name == "tblBorders" ||
            name == "tblCellMar")
            return true;
        if (name == "tr") {
            t.rows.push_back(read_row(e));
            return true;
        }
        if (name == "tblW" || name == "tblInd") {
            read_width(e);
            return true;
        }
        if (name == "gridCol") {
            read_width(e);
        } else if (name == "jc") {
            (void)first_attr(e);
        }
        return false;
    });
    return t;
}

Section read_section(const pugi::xml_node& node) {
    Section s;
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "pgMar") {
            Section m; // `PageMargin::new()` defaults
            for (const auto& a : e.attributes()) {
                const auto an = ooxml::local_name(a);
                std::int32_t* field = an == "top"      ? &m.top
                                      : an == "right"  ? &m.right
                                      : an == "bottom" ? &m.bottom
                                      : an == "left"   ? &m.left
                                      : an == "header" ? &m.header
                                      : an == "footer" ? &m.footer
                                      : an == "gutter" ? &m.gutter
                                                       : nullptr;
                if (field != nullptr) *field = value_to_dax(a.value());
            }
            std::tie(s.top, s.right, s.bottom, s.left, s.header, s.footer, s.gutter) =
                std::tie(m.top, m.right, m.bottom, m.left, m.header, m.footer, m.gutter);
        } else if (name == "pgSz") {
            Section z;
            for (const auto& a : e.attributes()) {
                const auto an = ooxml::local_name(a);
                if (an == "w") z.w = static_cast<std::uint32_t>(value_to_dax(a.value()));
                if (an == "h") z.h = static_cast<std::uint32_t>(value_to_dax(a.value()));
            }
            s.w = z.w;
            s.h = z.h;
        } else if (name == "docGrid") {
            bool ok = true;
            for (const auto& a : e.attributes()) {
                const auto an = ooxml::local_name(a);
                const std::string_view v = a.value();
                if (an == "type") {
                    ok &= v == "default" || v == "lines" || v == "linesAndChars" || v == "snapToChars";
                } else if (an == "linePitch" || an == "charSpace") {
                    ok &= parse_f64(v).has_value();
                }
            }
            s.extra |= ok;
        } else if (name == "pgNumType" || name == "titlePg") {
            s.extra = true;
        } else if (name == "headerReference" || name == "footerReference") {
            const auto kind = read(e, "type").value_or("default");
            s.extra |= kind == "default" || kind == "first" || kind == "even";
        } else if (name == "type") {
            s.extra |= read(e, "val").has_value();
        }
        return false;
    });
    return s;
}

struct Body {
    std::vector<Content> blocks;
    Section section;
};

Body read_document(const pugi::xml_node& root) {
    Body body;
    scan(root, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "p") {
            body.blocks.push_back(Content{Content::Kind::Paragraph, read_paragraph(e), {}, {}});
        } else if (name == "tbl") {
            Content c{Content::Kind::Table, {}, {}, {}};
            c.table.push_back(read_table(e));
            body.blocks.push_back(std::move(c));
        } else if (name == "sectPr") {
            body.section = read_section(e);
        } else if (name == "sdt") {
            try {
                auto leaves = read_sdt(e);
                std::move(leaves.begin(), leaves.end(), std::back_inserter(body.blocks));
            } catch (const Fatal&) {
            }
        } else {
            return name == "bookmarkStart" || name == "bookmarkEnd" || name == "commentRangeStart" ||
                   name == "commentRangeEnd";
        }
        return true;
    });
    return body;
}

Style read_style(const pugi::xml_node& node) {
    Style style;
    for (const auto& a : node.attributes())
        if (ooxml::local_name(a) == "styleId") style.id = a.value();
    style.id = escape_owned(style.id);
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "name") {
            style.name = escape_owned(first_attr(e));
        } else if (name == "basedOn") {
            if (auto v = read(e, "val")) style.based_on = escape_owned(*v);
        } else if (name == "link") {
        } else if (name == "pPr") {
            if (auto pp = read_para_props(e)) style.paragraph = std::move(*pp);
        } else if (name == "rPr") {
            style.run = read_run_props(e);
        } else {
            return name == "tblPr" || name == "tcPr";
        }
        return true;
    });
    return style;
}

std::vector<Style> read_styles(const pugi::xml_node& root) {
    std::vector<Style> styles;
    scan(root, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "style") {
            try {
                styles.push_back(read_style(e));
            } catch (const Fatal&) {
            }
            return true;
        }
        return name == "docDefaults";
    });
    return styles;
}

std::size_t usize_or_fail(std::string_view s) {
    auto v = parse_usize(s);
    if (!v) fail(NUM_ERROR);
    return *v;
}

Level read_level(const pugi::xml_node& node) {
    Level level;
    for (const auto& a : node.attributes())
        if (ooxml::local_name(a) == "ilvl") level.level = usize_or_fail(a.value());
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "pPr") {
            (void)read_para_props(e);
            return true;
        }
        if (name == "rPr") {
            try {
                (void)read_run_props(e);
            } catch (const Fatal&) {
            }
            return true;
        }
        if (name == "pStyle" || name == "suff" || name == "lvlRestart" || name == "lvlJc") {
            (void)first_attr(e);
        } else if (name == "start") {
            level.start = static_cast<std::uint32_t>(usize_or_fail(first_attr(e)));
        } else if (name == "numFmt") {
            level.format = first_attr(e);
        } else if (name == "lvlText") {
            level.text = first_attr(e);
        } else if (name == "ind") {
            read_indent(e);
        }
        return false;
    });
    return level;
}

std::optional<LevelOverride> read_level_override(const pugi::xml_node& node) {
    auto level = parse_usize(first_attr(node));
    if (!level) return std::nullopt;
    LevelOverride o{*level, std::nullopt, std::nullopt};
    scan(node, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "startOverride") {
            if (auto v = parse_usize(first_attr(e))) o.start = *v;
            return true;
        }
        if (name == "lvl") {
            try {
                o.definition = read_level(e);
            } catch (const Fatal&) {
            }
            return true;
        }
        return false;
    });
    return o;
}

Numberings read_numberings(const pugi::xml_node& root) {
    Numberings out;
    scan(root, [&](const pugi::xml_node& e) {
        const auto name = lname(e);
        if (name == "abstractNum") {
            AbstractNum abs;
            for (const auto& a : e.attributes())
                if (ooxml::local_name(a) == "abstractNumId") abs.id = usize_or_fail(a.value());
            scan(e, [&](const pugi::xml_node& c) {
                const auto n = lname(c);
                if (n == "lvl") {
                    abs.levels.push_back(read_level(c));
                    return true;
                }
                if (n == "styleLink" || n == "numStyleLink") (void)first_attr(c);
                return false;
            });
            out.abstract_nums.push_back(std::move(abs));
            return true;
        }
        if (name == "num") {
            Num num;
            for (const auto& a : e.attributes())
                if (ooxml::local_name(a) == "numId") num.id = usize_or_fail(a.value());
            scan(e, [&](const pugi::xml_node& c) {
                const auto n = lname(c);
                if (n == "abstractNumId") {
                    num.abstract_id = usize_or_fail(first_attr(c));
                } else if (n == "lvlOverride") {
                    try {
                        if (auto o = read_level_override(c)) num.overrides.push_back(std::move(*o));
                    } catch (const Fatal&) {
                    }
                    return true;
                }
                return false;
            });
            out.nums.push_back(std::move(num));
            return true;
        }
        return false;
    });
    return out;
}

// --- Package -----------------------------------------------------------------

// docx-rs `read_zip`: leading `/` dropped, `\` turned into `/`.
std::string zip_name(std::string_view name) {
    std::string out(name);
    std::ranges::replace(out, '\\', '/');
    const auto start = out.find_first_not_of('/');
    return start == std::string::npos ? std::string{} : out.substr(start);
}

std::string read_part(ooxml::Package& pkg, std::string_view name) {
    auto bytes = pkg.read(zip_name(name));
    if (!bytes || !*bytes) fail(ZIP_ERROR);
    return {(*bytes)->begin(), (*bytes)->end()};
}

std::unique_ptr<pugi::xml_document> parse_xml(std::string_view xml) {
    auto doc = ooxml::xml_reader(xml);
    if (!doc) fail(XML_ERROR);
    return std::move(*doc);
}

// `Path::parent`: `None` for an empty path.
std::optional<std::string> parent_dir(std::string_view path) {
    if (path.empty()) return std::nullopt;
    const auto slash = path.rfind('/');
    if (slash == std::string_view::npos) return std::string{};
    if (slash == 0) return std::string("/");
    return std::string(path.substr(0, slash));
}

// `Path::join`: an absolute `rel` replaces `dir`.
std::string join(std::string_view dir, std::string_view rel) {
    if (rel.starts_with('/') || dir.empty()) return std::string(rel);
    std::string out(dir);
    if (!out.ends_with('/')) out.push_back('/');
    out += rel;
    return out;
}

// `find_rels_filename`: `<dir>/_rels/<stem>.xml.rels`.
std::string rels_name(std::string_view dir, std::string_view path) {
    std::string_view file = path.substr(path.rfind('/') == std::string_view::npos ? 0 : path.rfind('/') + 1);
    const auto strip_ext = [](std::string_view f) {
        const auto dot = f.rfind('.');
        return dot == std::string_view::npos || dot == 0 ? f : f.substr(0, dot);
    };
    if (file.empty()) fail(RELS_NOT_FOUND);
    const std::string_view stem = strip_ext(file);
    return join(join(dir, "_rels"), std::string(strip_ext(stem)) + ".xml.rels");
}

// Relationship targets by type, each set ordered as docx-rs's `BTreeSet`.
using DocRels = std::map<std::string, std::set<std::pair<std::string, std::string>>, std::less<>>;

DocRels read_document_rels(ooxml::Package& pkg, std::string_view document_path) {
    const auto dir = parent_dir(document_path);
    if (!dir) fail(RELS_NOT_FOUND);
    const auto xml = read_part(pkg, rels_name(*dir, document_path));
    const auto doc = parse_xml(xml);
    DocRels rels;
    scan(*doc, [&](const pugi::xml_node& e) {
        if (lname(e) != "Relationship") return false;
        std::string type, id, target;
        for (const auto& a : e.attributes()) {
            const auto an = ooxml::local_name(a);
            if (an == "Type") type = a.value();
            if (an == "Target") target = a.value();
            if (an == "Id") id = a.value();
        }
        if (!type.ends_with("hyperlink")) target = join(*dir, target);
        rels[type].emplace(std::move(id), std::move(target));
        return true;
    });
    return rels;
}

std::optional<std::string> first_target(const DocRels& rels, std::string_view type) {
    const auto it = rels.find(type);
    if (it == rels.end() || it->second.empty()) return std::nullopt;
    return it->second.begin()->second;
}

// --- Outline -------------------------------------------------------------------

std::string table_text(const Table& table) {
    std::string out;
    for (std::size_t r = 0; r < table.rows.size(); ++r) {
        if (r > 0) out.push_back('\n');
        for (std::size_t c = 0; c < table.rows[r].size(); ++c) {
            if (c > 0) out.push_back('\t');
            out += table.rows[r][c];
        }
    }
    return out;
}

constexpr std::array<std::string_view, LIST_LEVELS> LEVEL_PLACEHOLDERS{"%1", "%2", "%3", "%4", "%5", "%6", "%7", "%8", "%9"};

void replace_all(std::string& s, std::string_view from, std::string_view to) {
    for (std::size_t at = s.find(from); at != std::string::npos; at = s.find(from, at + to.size())) s.replace(at, from.size(), to);
}

// Builds the outline from what docx-rs read, resolving styles and list numbering.
class Reader {
  public:
    Reader(const std::vector<Style>& styles, const Numberings& numberings) : styles_(styles), numberings_(numberings) {}

    std::vector<Block> blocks(const std::vector<Content>& children) {
        std::vector<Block> out;
        for (const auto& child : children) {
            if (child.kind == Content::Kind::Paragraph) out.push_back(Block{paragraph(child.paragraph)});
            if (child.kind == Content::Kind::Table) out.push_back(Block{table(child.table.front())});
        }
        return out;
    }

  private:
    [[nodiscard]] const Style* find_style(std::string_view id) const {
        for (const auto& s : styles_)
            if (s.id == id) return &s;
        return nullptr;
    }

    // The style `id` followed by the styles it is based on.
    [[nodiscard]] std::vector<const Style*> style_chain(const std::optional<std::string>& id) const {
        std::vector<const Style*> chain;
        const Style* next = id ? find_style(*id) : nullptr;
        while (next != nullptr && chain.size() < MAX_STYLE_DEPTH) {
            chain.push_back(next);
            next = next->based_on ? find_style(*next->based_on) : nullptr;
        }
        return chain;
    }

    Paragraph paragraph(const RawPara& p) {
        Paragraph out;
        out.style = p.props.style;
        out.runs = runs(p, out.page_break_after);
        const NumPr* numbering = p.props.numbering ? &*p.props.numbering : nullptr;
        if (numbering == nullptr) {
            for (const Style* s : style_chain(out.style)) {
                if (s->paragraph.numbering) {
                    numbering = &*s->paragraph.numbering;
                    break;
                }
            }
        }
        if (out.style) {
            // From the style's name, falling back to its id.
            const Style* style = find_style(*out.style);
            if (style != nullptr) out.heading_level = heading_level_of(style->name);
            if (!out.heading_level) out.heading_level = heading_level_of(*out.style);
        }
        if (numbering != nullptr) out.list = list_item(*numbering);
        for (const auto& r : out.runs) out.text += r.text;
        out.page_break_before = p.props.page_break_before;
        return out;
    }

    // The paragraph's runs with their text, and whether it holds a page break.
    [[nodiscard]] std::vector<Run> runs(const RawPara& p, bool& page_break) const {
        std::vector<Run> out;
        for (const auto& r : p.runs) {
            page_break |= r.page_break;
            if (r.text.empty()) continue;
            // Each property comes from the run, else its character style, else the paragraph style, each
            // through its `basedOn` chain.
            auto inherited = style_chain(r.props.style);
            const auto paragraph_chain = style_chain(p.props.style);
            inherited.insert(inherited.end(), paragraph_chain.begin(), paragraph_chain.end());
            const auto flag = [&](std::optional<bool> RunProps::* field) {
                if (r.props.*field) return *(r.props.*field);
                for (const Style* s : inherited)
                    if (s->run.*field) return *(s->run.*field);
                return false;
            };
            out.push_back(Run{r.text, flag(&RunProps::bold), flag(&RunProps::italic), flag(&RunProps::underline)});
        }
        return out;
    }

    // Counts the item in its numbering instance and renders its marker.
    std::optional<ListItem> list_item(const NumPr& np) {
        if (!np.id) return std::nullopt;
        const std::size_t num_id = *np.id;
        // numId 0 removes numbering inherited from the style.
        if (num_id == 0) return std::nullopt;
        const std::size_t level = std::min(np.level.value_or(0), LIST_LEVELS - 1);
        const Num* numbering = nullptr;
        for (const auto& n : numberings_.nums) {
            if (n.id == num_id) {
                numbering = &n;
                break;
            }
        }
        const AbstractNum* abstract_num = nullptr;
        if (numbering != nullptr) {
            for (const auto& a : numberings_.abstract_nums) {
                if (a.id == numbering->abstract_id) {
                    abstract_num = &a;
                    break;
                }
            }
        }
        const auto override_of = [&](std::size_t l) -> const LevelOverride* {
            if (numbering == nullptr) return nullptr;
            for (const auto& o : numbering->overrides)
                if (o.level == l) return &o;
            return nullptr;
        };
        const auto definition = [&](std::size_t l) -> const Level* {
            if (const auto* o = override_of(l); o != nullptr && o->definition) return &*o->definition;
            if (abstract_num == nullptr) return nullptr;
            for (const auto& d : abstract_num->levels)
                if (d.level == l) return &d;
            return nullptr;
        };
        const auto start = [&](std::size_t l) -> std::uint32_t {
            std::optional<std::size_t> v;
            if (const auto* o = override_of(l); o != nullptr) v = o->start;
            if (!v) {
                if (const auto* d = definition(l); d != nullptr) v = d->start;
            }
            if (!v) return 1;
            return *v > std::numeric_limits<std::uint32_t>::max() ? std::numeric_limits<std::uint32_t>::max()
                                                                  : static_cast<std::uint32_t>(*v);
        };

        auto& counters = counters_[num_id];
        if (counters[level]) {
            counters[level] = *counters[level] == std::numeric_limits<std::uint32_t>::max() ? *counters[level] : *counters[level] + 1;
        } else {
            counters[level] = start(level);
        }
        for (std::size_t l = level + 1; l < LIST_LEVELS; ++l) counters[l].reset();
        const auto snapshot = counters;

        std::string marker;
        const Level* def = definition(level);
        if (def == nullptr) {
            marker = std::to_string(snapshot[level].value_or(1)) + ".";
        } else if (def->format == "bullet") {
            marker = "\xE2\x80\xA2";
        } else if (def->format == "none") {
        } else {
            marker = def->text;
            for (std::size_t l = 0; l < LIST_LEVELS; ++l) {
                if (marker.find(LEVEL_PLACEHOLDERS[l]) == std::string::npos) continue;
                const Level* lvl = definition(l);
                const std::string_view format = lvl != nullptr ? std::string_view(lvl->format) : "decimal";
                const std::uint32_t n = snapshot[l] ? *snapshot[l] : start(l);
                replace_all(marker, LEVEL_PLACEHOLDERS[l], format_number(n, format));
            }
        }
        return ListItem{num_id, static_cast<std::uint8_t>(level), std::move(marker)};
    }

    [[nodiscard]] Table table(const RawTable& t) const {
        Table out;
        for (const auto& row : t.rows) {
            std::vector<std::string> cells;
            cells.reserve(row.size());
            for (const auto& cell : row) cells.push_back(cell_text(cell));
            out.rows.push_back(std::move(cells));
        }
        return out;
    }

    [[nodiscard]] std::string cell_text(const std::vector<Content>& cell) const {
        const auto plain = [](const RawPara& p) {
            std::string s;
            for (const auto& r : p.runs) s += r.text;
            return s;
        };
        std::string out;
        for (std::size_t i = 0; i < cell.size(); ++i) {
            if (i > 0) out.push_back('\n');
            const auto& content = cell[i];
            switch (content.kind) {
            case Content::Kind::Paragraph: out += plain(content.paragraph); break;
            case Content::Kind::Table: out += table_text(table(content.table.front())); break;
            case Content::Kind::Sdt:
                for (const auto& leaf : content.sdt) {
                    if (leaf.kind == Content::Kind::Paragraph) out += plain(leaf.paragraph);
                    if (leaf.kind == Content::Kind::Table) out += table_text(table(leaf.table.front()));
                }
                break;
            }
        }
        return out;
    }

    const std::vector<Style>& styles_;
    const Numberings& numberings_;
    // Counter per level of each numbering instance; empty until used or after a shallower level reset it.
    std::unordered_map<std::size_t, std::array<std::optional<std::uint32_t>, LIST_LEVELS>> counters_;
};

// Page size and margins of the body's section properties. A section property
// identical to docx-rs's defaults is taken as absent: Letter with 1 inch margins.
std::pair<PageSize, Margins> section_geometry(const Section& section) {
    const float m = DEFAULT_MARGIN;
    const std::pair<PageSize, Margins> fallback{LETTER, Margins{m, m, m, m}};
    if (section.is_default() || section.w == 0 || section.h == 0) return fallback;
    const PageSize page{static_cast<float>(section.w) / TWIPS_PER_POINT, static_cast<float>(section.h) / TWIPS_PER_POINT};
    // Negative margins let text overlap the header; clamp so the text area keeps at least a third of the page.
    const auto margin = [](std::int32_t twips, float extent) {
        return std::min(static_cast<float>(std::max(twips, 0)) / TWIPS_PER_POINT, extent / 3.0F);
    };
    return {page, Margins{margin(section.top, page.height), margin(section.right, page.width), margin(section.bottom, page.height),
                          margin(section.left, page.width)}};
}

struct Parsed {
    Outline outline;
    PageSize page;
    Margins margins;
};

Parsed read_docx(std::span<const std::uint8_t> bytes) {
    auto opened = ooxml::Package::open(bytes, Format::Docx);
    if (!opened) fail(ZIP_ERROR);
    ooxml::Package& pkg = *opened;

    (void)parse_xml(read_part(pkg, "[Content_Types].xml"));
    std::string document_path = "word/document.xml";
    {
        const auto rels = parse_xml(read_part(pkg, "_rels/.rels"));
        bool found = false;
        for (const auto& rel : rels->document_element().children()) {
            if (found || rel.type() != pugi::node_element) continue;
            std::string type, target;
            for (const auto& a : rel.attributes()) {
                if (ooxml::local_name(a) == "Type") type = a.value();
                if (ooxml::local_name(a) == "Target") target = a.value();
            }
            if (type == DOC_RELATIONSHIP_TYPE) {
                document_path = std::move(target);
                found = true;
            }
        }
    }
    const DocRels rels = read_document_rels(pkg, document_path);

    // Comments are only parsed; a part that exists must be XML.
    for (const auto type : {COMMENTS_EXTENDED_TYPE, COMMENTS_TYPE}) {
        if (auto path = first_target(rels, type)) {
            auto data = pkg.read(zip_name(*path));
            if (data && *data) (void)parse_xml(std::string((*data)->begin(), (*data)->end()));
        }
    }

    Body body = read_document(*parse_xml(read_part(pkg, document_path)));

    std::vector<Style> styles;
    if (auto path = first_target(rels, STYLE_RELATIONSHIP_TYPE)) styles = read_styles(*parse_xml(read_part(pkg, *path)));
    Numberings numberings;
    if (auto path = first_target(rels, NUMBERING_RELATIONSHIP_TYPE))
        numberings = read_numberings(*parse_xml(read_part(pkg, *path)));
    for (const auto type : {SETTINGS_TYPE, WEB_SETTINGS_TYPE})
        if (auto path = first_target(rels, type)) (void)parse_xml(read_part(pkg, *path));

    Reader reader(styles, numberings);
    Outline outline{reader.blocks(body.blocks)};
    const auto [page, margins] = section_geometry(body.section);
    return Parsed{std::move(outline), page, margins};
}

// --- Layout --------------------------------------------------------------------

// Byte range of `line` in `source`; `wrap` returns subslices of its text.
std::pair<std::size_t, std::size_t> span(std::string_view source, std::string_view line) {
    const auto s = reinterpret_cast<std::uintptr_t>(source.data());
    const auto l = reinterpret_cast<std::uintptr_t>(line.data());
    const std::size_t start = l > s ? l - s : 0;
    return {start, start + line.size()};
}

class Layout {
  public:
    Layout(const Fonts& fonts, float body_size, Margins margins, float width, float bottom)
        : fonts_(fonts), body_size_(body_size), pages_(1), margins_(margins), width_(width), bottom_(bottom), y_(margins.top) {}

    void paragraph(std::size_t block, const Paragraph& p) {
        if (p.page_break_before && !page_is_empty()) new_page();
        const bool heading = p.heading_level.has_value();
        const float size = p.heading_level ? (*p.heading_level < HEADING_SIZES.size() ? HEADING_SIZES[*p.heading_level] : 12.0F)
                                           : body_size_;
        if (heading && y_ > margins_.top) y_ += size * 0.5F;
        const float indent = p.list ? LIST_INDENT * (static_cast<float>(p.list->level) + 1.0F) : 0.0F;
        const float width = std::max(width_ - indent, size);
        const float line_height = fonts_.line_height(size);
        const float x = margins_.left + indent;
        const auto lines = fonts_.wrap(p.text, size, heading, width);
        for (std::size_t i = 0; i < lines.size(); ++i) {
            const float y = advance(line_height);
            if (i == 0 && p.list) push(op::Marker{x - LIST_INDENT, y, size, block});
            if (!lines[i].empty()) {
                const auto [start, end] = span(p.text, lines[i]);
                push(op::Text{x, y, size, block, std::nullopt, start, end});
            }
        }
        y_ += size * 0.5F;
        if (p.page_break_after) new_page();
    }

    void table(std::size_t block, const Table& t) {
        std::size_t cols = 0;
        for (const auto& row : t.rows) cols = std::max(cols, row.size());
        if (cols == 0) return;
        const float col_width = width_ / static_cast<float>(cols);
        const float text_width = std::max(col_width - 2.0F * CELL_PADDING, 1.0F);
        const float size = body_size_;
        const float line_height = fonts_.line_height(size);
        for (std::size_t r = 0; r < t.rows.size(); ++r) {
            const auto& row = t.rows[r];
            std::vector<std::vector<std::string_view>> lines;
            lines.reserve(row.size());
            for (const auto& cell : row) lines.push_back(fonts_.wrap(cell, size, false, text_width));
            std::size_t most = 1;
            if (!lines.empty()) {
                most = 0;
                for (const auto& l : lines) most = std::max(most, l.size());
                most = std::max<std::size_t>(most, 1);
            }
            const float row_height = static_cast<float>(most) * line_height + 2.0F * CELL_PADDING;
            const float y = advance(row_height);
            for (std::size_t c = 0; c < cols; ++c) {
                const float x = margins_.left + static_cast<float>(c) * col_width;
                push(op::Border{x, y, col_width, row_height});
                if (c >= row.size()) continue;
                for (std::size_t i = 0; i < lines[c].size(); ++i) {
                    if (lines[c][i].empty()) continue;
                    const auto [start, end] = span(row[c], lines[c][i]);
                    push(op::Text{x + CELL_PADDING, y + CELL_PADDING + static_cast<float>(i) * line_height, size, block,
                                  std::pair{r, c}, start, end});
                }
            }
        }
        y_ += size * 0.5F;
    }

    Pages finish() && {
        // A trailing page break leaves an empty last page Word would not show.
        if (pages_.size() > 1 && page_is_empty()) pages_.pop_back();
        return std::move(pages_);
    }

  private:
    void new_page() {
        pages_.emplace_back();
        y_ = margins_.top;
    }

    // Takes `height` below the cursor, on a new page when it doesn't fit unless the cursor is already at the
    // top (then it overflows). Returns the top of the taken space.
    float advance(float height) {
        if (y_ + height > bottom_ && y_ > margins_.top) new_page();
        y_ += height;
        return y_ - height;
    }

    void push(Op op) {
        if (!pages_.empty()) pages_.back().push_back(op);
    }

    [[nodiscard]] bool page_is_empty() const { return pages_.empty() || pages_.back().empty(); }

    const Fonts& fonts_;
    float body_size_;
    Pages pages_;
    Margins margins_;
    // Width and bottom edge of the text area.
    float width_;
    float bottom_;
    float y_;
};

// Draws one line piece, advancing tabs by `TAB_EM`. Returns the advance.
float draw_segment(Canvas& canvas, const Fonts& fonts, TextStyle style, float x, float y, std::string_view text, bool underline) {
    float advance = 0.0F;
    std::size_t i = 0;
    while (true) {
        const auto tab = text.find('\t');
        const std::string_view piece = text.substr(0, tab);
        if (i > 0) advance += style.size * TAB_EM;
        if (!piece.empty()) advance += canvas.draw_text(fonts, style, x + advance, y, piece);
        ++i;
        if (tab == std::string_view::npos) break;
        text.remove_prefix(tab + 1);
    }
    if (underline) canvas.fill_rect(x, y + style.size * 1.05F, advance, std::max(style.size / 14.0F, 1.0F), style.color);
    return advance;
}

// `str::get(a..b)`: the bytes when the range is in bounds on character boundaries.
std::optional<std::string_view> substr_get(std::string_view s, std::size_t a, std::size_t b) {
    const auto boundary = [&](std::size_t i) {
        return i == s.size() || (i < s.size() && (static_cast<unsigned char>(s[i]) & 0xC0U) != 0x80U);
    };
    if (a > b || b > s.size() || !boundary(a) || !boundary(b)) return std::nullopt;
    return s.substr(a, b - a);
}

bool is_default_body(const RenderOpts& opts) { return opts.typography.body == DocTypography{}.body; }

} // namespace

json::Value Run::to_json() const {
    return json::Object{{"text", text}, {"bold", bold}, {"italic", italic}, {"underline", underline}};
}

json::Value ListItem::to_json() const {
    return json::Object{{"num_id", static_cast<std::uint64_t>(num_id)}, {"level", static_cast<std::uint64_t>(level)}, {"marker", marker}};
}

json::Object Paragraph::fields() const {
    json::Array runs_json;
    runs_json.reserve(runs.size());
    for (const auto& r : runs) runs_json.push_back(r.to_json());
    return json::Object{
        {"style", style ? json::Value(*style) : json::Value(nullptr)},
        {"heading_level", heading_level ? json::Value(static_cast<std::uint64_t>(*heading_level)) : json::Value(nullptr)},
        {"list", list ? list->to_json() : json::Value(nullptr)},
        {"runs", std::move(runs_json)},
        {"text", text},
        {"page_break_before", page_break_before},
        {"page_break_after", page_break_after},
    };
}

json::Value Block::to_json() const {
    if (const auto* p = paragraph()) {
        json::Object out{{"type", "paragraph"}};
        for (auto& field : p->fields()) out.push_back(std::move(field));
        return out;
    }
    json::Array rows;
    for (const auto& row : std::get<Table>(v).rows) {
        json::Array cells;
        for (const auto& cell : row) cells.emplace_back(cell);
        rows.emplace_back(std::move(cells));
    }
    return json::Object{{"type", "table"}, {"rows", std::move(rows)}};
}

json::Value Outline::to_json() const {
    json::Array out;
    out.reserve(blocks.size());
    for (const auto& b : blocks) out.push_back(b.to_json());
    return json::Object{{"blocks", std::move(out)}};
}

std::optional<std::uint8_t> heading_level_of(std::string_view label) {
    std::string lower(label);
    for (char& c : lower) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    if (lower == "title") return 0;
    if (!lower.starts_with("heading")) return std::nullopt;
    std::string_view rest = std::string_view(lower).substr(7);
    const auto ws = [](char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v'; };
    while (!rest.empty() && ws(rest.front())) rest.remove_prefix(1);
    while (!rest.empty() && ws(rest.back())) rest.remove_suffix(1);
    const auto level = parse_usize(rest);
    if (!level || *level < 1 || *level > 9) return std::nullopt;
    return static_cast<std::uint8_t>(*level);
}

std::string format_number(std::uint32_t n, std::string_view format) {
    static constexpr std::pair<std::uint32_t, std::string_view> NUMERALS[] = {
        {1000, "M"}, {900, "CM"}, {500, "D"}, {400, "CD"}, {100, "C"}, {90, "XC"}, {50, "L"},
        {40, "XL"},  {10, "X"},   {9, "IX"},  {5, "V"},    {4, "IV"},  {1, "I"},
    };
    if ((format == "lowerLetter" || format == "upperLetter") && n > 0) {
        const char letter = static_cast<char>((format == "upperLetter" ? 'A' : 'a') + (n - 1) % 26);
        return std::string((n - 1) / 26 + 1, letter);
    }
    if ((format == "lowerRoman" || format == "upperRoman") && n > 0) {
        std::uint32_t rest = n;
        std::string roman;
        for (const auto& [value, numeral] : NUMERALS) {
            while (rest >= value) {
                roman += numeral;
                rest -= value;
            }
        }
        if (format == "lowerRoman")
            for (char& c : roman) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
        return roman;
    }
    if (format == "decimalZero") return n < 10 ? "0" + std::to_string(n) : std::to_string(n);
    return std::to_string(n);
}

Pages layout(const Outline& outline, PageSize page, Margins margins, const Fonts& fonts, float body_size) {
    const float width = std::max(page.width - margins.left - margins.right, 1.0F);
    const float bottom = page.height - margins.bottom;
    Layout layout(fonts, body_size, margins, width, bottom);
    for (std::size_t block = 0; block < outline.blocks.size(); ++block) {
        if (const auto* p = outline.blocks[block].paragraph()) layout.paragraph(block, *p);
        if (const auto* t = outline.blocks[block].table()) layout.table(block, *t);
    }
    return std::move(layout).finish();
}

} // namespace docx

using namespace docx;

Result<std::unique_ptr<DocxDoc>> DocxDoc::parse(std::span<const std::uint8_t> bytes) {
    try {
        auto parsed = read_docx(bytes);
        auto pages = layout(parsed.outline, parsed.page, parsed.margins, Fonts::none(), DocTypography{}.body);
        return std::unique_ptr<DocxDoc>(new DocxDoc(std::move(parsed.outline), parsed.page, parsed.margins, std::move(pages)));
    } catch (const Fatal& e) {
        return std::unexpected(DocsError::invalid(Format::Docx, e.message));
    } catch (...) {
        // docx-rs indexes attribute lists without checking them (`<w:u/>`, `<w:start/>`), so a malformed part
        // panics inside the reader.
        return std::unexpected(DocsError::invalid(Format::Docx, "malformed document part"));
    }
}

std::string DocxDoc::text() const {
    std::string out;
    for (std::size_t i = 0; i < outline_.blocks.size(); ++i) {
        if (i > 0) out.push_back('\n');
        if (const auto* p = outline_.blocks[i].paragraph()) out += p->text;
        if (const auto* t = outline_.blocks[i].table()) out += table_text(*t);
    }
    return out;
}

std::size_t DocxDoc::page_count() const { return pages_.size(); }

Result<PageSize> DocxDoc::page_size(std::size_t index) const {
    if (index >= pages_.size()) return std::unexpected(DocsError::page_range(index, pages_.size()));
    return page_;
}

std::size_t DocxDoc::page_count_with_opts(const RenderOpts& opts) const {
    if (is_default_body(opts)) return pages_.size();
    return layout(outline_, page_, margins_, Fonts::none(), opts.typography.body).size();
}

Result<PageSize> DocxDoc::page_size_with_opts(std::size_t index, const RenderOpts& opts) const {
    const std::size_t count = page_count_with_opts(opts);
    if (index < count) return page_;
    return std::unexpected(DocsError::page_range(index, count));
}

std::optional<Result<json::Value>> DocxDoc::query(const DataQuery& query) const {
    if (const auto* t = std::get_if<query::Text>(&query.v); t != nullptr && !t->page) return Result<json::Value>(json::Value(text()));
    if (std::holds_alternative<query::Outline>(query.v)) return Result<json::Value>(outline_.to_json());
    return std::nullopt;
}

void DocxDoc::draw_op(Canvas& canvas, const Fonts& fonts, float scale, const RenderOpts& opts, const Op& op) const {
    const auto s = [scale](float v) { return v * scale; };
    if (const auto* b = std::get_if<op::Border>(&op)) {
        canvas.stroke_rect(s(b->x), s(b->y), s(b->w), s(b->h), scale, opts.palette.rule);
    } else if (const auto* m = std::get_if<op::Marker>(&op)) {
        if (m->block >= outline_.blocks.size()) return;
        const auto* p = outline_.blocks[m->block].paragraph();
        if (p != nullptr && p->list) draw_segment(canvas, fonts, opts.text(s(m->size)), s(m->x), s(m->y), p->list->marker, false);
    } else if (const auto* t = std::get_if<op::Text>(&op)) {
        if (t->block >= outline_.blocks.size()) return;
        const TextStyle style = opts.text(s(t->size));
        const float y = s(t->y);
        const auto& block = outline_.blocks[t->block];
        if (const auto* p = block.paragraph(); p != nullptr && !t->cell) {
            // Run by run, so bold and underline follow the runs.
            float caret = s(t->x);
            std::size_t run_start = 0;
            for (const auto& run : p->runs) {
                const std::size_t a = std::max(t->start, run_start);
                const std::size_t b = std::min(t->end, run_start + run.text.size());
                run_start += run.text.size();
                if (auto segment = substr_get(p->text, a, b)) {
                    caret += draw_segment(canvas, fonts, style.with_bold(p->heading_level.has_value() || run.bold), caret, y, *segment,
                                          run.underline);
                }
            }
        } else if (const auto* table = block.table(); table != nullptr && t->cell) {
            const auto [row, col] = *t->cell;
            if (row < table->rows.size() && col < table->rows[row].size()) {
                if (auto line = substr_get(table->rows[row][col], t->start, t->end))
                    draw_segment(canvas, fonts, style, s(t->x), y, *line, false);
            }
        }
    }
}

Result<Rgba> DocxDoc::render(std::size_t index, float scale, const Fonts& fonts, const RenderOpts& opts) const {
    Pages custom;
    const Pages* pages = &pages_;
    if (!is_default_body(opts)) {
        custom = layout(outline_, page_, margins_, Fonts::none(), opts.typography.body);
        pages = &custom;
    }
    if (index >= pages->size()) return std::unexpected(DocsError::page_range(index, pages->size()));
    const auto [width, height] = page_.pixels(scale);
    Canvas canvas(width, height, opts.palette.page);
    for (const auto& op : (*pages)[index]) draw_op(canvas, fonts, scale, opts, op);
    return std::move(canvas).into_rgba();
}

} // namespace loa::surfaces::docs
