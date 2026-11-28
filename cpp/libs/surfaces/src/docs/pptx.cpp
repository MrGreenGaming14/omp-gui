#include "loa/surfaces/docs/pptx.hpp"

#include <algorithm>
#include <exception>
#include <format>
#include <utility>

#include "loa/surfaces/docs/images.hpp"
#include "loa/surfaces/docs/ooxml.hpp"

namespace loa::surfaces::docs {

namespace {

// English Metric Units per point.
constexpr double EMU_PER_POINT = 12700.0;
// Slide size when `p:sldSz` is missing (4:3).
constexpr PageSize DEFAULT_SLIDE_SIZE{720.0F, 540.0F};
// Default left/right and top text insets of a text body, in points.
constexpr float INSET_X = 7.2F;
constexpr float INSET_Y = 3.6F;

constexpr std::string_view PRESENTATION_PART = "ppt/presentation.xml";
constexpr std::string_view REL_SLIDE = "/slide";
constexpr std::string_view REL_NOTES = "/notesSlide";
constexpr std::string_view REL_LAYOUT = "/slideLayout";
constexpr std::string_view REL_MASTER = "/slideMaster";

using ooxml::Package;
using ooxml::Rel;
using ooxml::XmlResult;

// A box in points or pixels: x, y, width, height.
struct Rect32 {
    float x, y, w, h;
};

float pt(double emu) { return static_cast<float>(emu / EMU_PER_POINT); }

bool is_space(char c) { return c == ' ' || c == '\t' || c == '\r' || c == '\n' || c == '\f' || c == '\v'; }
bool blank(std::string_view s) { return std::ranges::all_of(s, is_space); }

bool is_title(const std::optional<std::string>& placeholder) {
    return placeholder && (*placeholder == "title" || *placeholder == "ctrTitle");
}

json::Value opt_json(const std::optional<std::string>& s) { return s ? json::Value(*s) : json::Value(nullptr); }

json::Value strings_json(const std::vector<std::string>& v) {
    json::Array out;
    out.reserve(v.size());
    for (const auto& s : v) out.emplace_back(s);
    return out;
}

// --- Shape parsing ---

// A rectangle in EMU.
struct Rect {
    double x = 0, y = 0, w = 0, h = 0;
};

// A group's transform from its child coordinate space to its parent's.
struct Frame {
    Rect off, child;

    [[nodiscard]] Rect apply(Rect r) const {
        const double sx = child.w > 0.0 ? off.w / child.w : 1.0;
        const double sy = child.h > 0.0 ? off.h / child.h : 1.0;
        return {off.x + (r.x - child.x) * sx, off.y + (r.y - child.y) * sy, r.w * sx, r.h * sy};
    }
};

// A shape as read from a slide, layout, master or notes part.
struct ParsedShape {
    std::string name;
    ShapeKind kind = ShapeKind::Other;
    std::optional<std::string> placeholder;
    std::optional<std::string> ph_idx;
    // Geometry on the slide, none when the shape has no `xfrm`.
    std::optional<Rect> rect;
    std::vector<std::string> paragraphs;
    std::vector<ParaStyle> styles;
    // Relationship id of the picture.
    std::optional<std::string> embed;
};

// The elements the shape parser tracks while they are open.
enum class Tag : std::uint8_t { Group, Shape, Xfrm, Paragraph, Text, TableRow, TableCell, Skip, Other };

struct Builder {
    ParsedShape shape;
    // The element the shape was opened from (`sp`, `pic`, `graphicFrame`, `cxnSp`).
    std::string_view element;
    bool is_table = false;
    bool in_paragraph = false;
    std::string paragraph;
    ParaStyle style;
    bool style_from_run = false;
    std::vector<std::string> cell;
    std::vector<std::string> row;
    std::optional<ParaStyle> row_style;
};

class ShapeParser {
  public:
    std::vector<ParsedShape> shapes;

    // Walks `node`'s children as the open, text and close events of a pull parser.
    XmlResult<void> walk(const pugi::xml_node& node) {
        for (pugi::xml_node child = node.first_child(); child; child = child.next_sibling()) {
            switch (child.type()) {
            case pugi::node_element: {
                auto tag = open(child);
                if (!tag) return std::unexpected(std::move(tag.error()));
                if (*tag == Tag::Skip) break;
                stack_.push_back(*tag);
                if (auto r = walk(child); !r) return r;
                stack_.pop_back();
                close(*tag);
                break;
            }
            case pugi::node_pcdata:
            case pugi::node_cdata: push_text(child.value()); break;
            default: break;
            }
        }
        return {};
    }

  private:
    std::vector<Tag> stack_;
    // Transforms of the open groups, the shape tree first.
    std::vector<Frame> frames_;
    std::optional<Builder> current_;

    // Appends character data to the paragraph when inside `a:t`.
    void push_text(std::string_view text) {
        if (!stack_.empty() && stack_.back() == Tag::Text && current_ && current_->in_paragraph)
            current_->paragraph += text;
    }

    XmlResult<Tag> open(const pugi::xml_node& e) {
        const std::string_view name = ooxml::local_name(e);
        if (name == "spTree" || name == "grpSp") return group();
        for (std::string_view s : {"sp", "pic", "graphicFrame", "cxnSp"})
            if (name == s) return shape(s);
        if (name == "Fallback") return Tag::Skip;
        if (name == "xfrm") return Tag::Xfrm;
        for (std::string_view s : {"off", "ext", "chOff", "chExt"})
            if (name == s) return geometry(e, s);
        if (name == "cNvPr") {
            if (current_ && current_->shape.name.empty())
                if (auto n = ooxml::attr(e, "name")) current_->shape.name = std::string(*n);
            return Tag::Other;
        }
        if (name == "ph") {
            if (current_) {
                const auto type = ooxml::attr(e, "type");
                current_->shape.placeholder = type ? std::string(*type) : std::string("obj");
                const auto idx = ooxml::attr(e, "idx");
                current_->shape.ph_idx = idx ? std::optional<std::string>(std::string(*idx)) : std::nullopt;
            }
            return Tag::Other;
        }
        if (name == "blip") {
            if (current_ && !current_->shape.embed)
                if (auto id = ooxml::attr(e, "embed")) current_->shape.embed = std::string(*id);
            return Tag::Other;
        }
        if (name == "tbl") {
            if (current_) current_->is_table = true;
            return Tag::Other;
        }
        if (name == "tr") return Tag::TableRow;
        if (name == "tc") {
            if (current_) current_->cell.clear();
            return Tag::TableCell;
        }
        if (name == "p") {
            if (!current_) return Tag::Other;
            current_->in_paragraph = true;
            current_->paragraph.clear();
            current_->style = ParaStyle{};
            current_->style_from_run = false;
            return Tag::Paragraph;
        }
        if (name == "t") return Tag::Text;
        if (name == "br") {
            if (current_ && current_->in_paragraph) current_->paragraph.push_back('\n');
            return Tag::Other;
        }
        if (name == "rPr" || name == "endParaRPr") return run_props(e, name);
        return Tag::Other;
    }

    Tag group() {
        if (current_) return Tag::Other;
        frames_.emplace_back();
        return Tag::Group;
    }

    Tag shape(std::string_view element) {
        if (current_) return Tag::Other;
        current_.emplace();
        current_->element = element;
        return Tag::Shape;
    }

    XmlResult<Tag> geometry(const pugi::xml_node& e, std::string_view name) {
        if (stack_.empty() || stack_.back() != Tag::Xfrm) return Tag::Other;
        const bool offset = name.ends_with("ff");
        auto a = ooxml::num_attr(e, offset ? "x" : "cx");
        if (!a) return std::unexpected(std::move(a.error()));
        auto b = ooxml::num_attr(e, offset ? "y" : "cy");
        if (!b) return std::unexpected(std::move(b.error()));
        const bool own = name == "off" || name == "ext";
        Rect* target = nullptr;
        if (current_) {
            if (own) {
                if (!current_->shape.rect) current_->shape.rect.emplace();
                target = &*current_->shape.rect;
            }
        } else if (!frames_.empty()) {
            target = own ? &frames_.back().off : &frames_.back().child;
        }
        if (target != nullptr) {
            if (offset) {
                target->x = a->value_or(0.0);
                target->y = b->value_or(0.0);
            } else {
                target->w = a->value_or(0.0);
                target->h = b->value_or(0.0);
            }
        }
        return Tag::Other;
    }

    XmlResult<Tag> run_props(const pugi::xml_node& e, std::string_view name) {
        if (current_ && current_->in_paragraph && !current_->style_from_run) {
            auto sz = ooxml::num_attr(e, "sz");
            if (!sz) return std::unexpected(std::move(sz.error()));
            ParaStyle style;
            if (*sz) {
                const auto size = static_cast<float>(**sz / 100.0);
                if (size > 0.0F) style.size = size;
            }
            const auto b = ooxml::attr(e, "b");
            style.bold = b && (*b == "1" || *b == "true");
            current_->style = style;
            current_->style_from_run = name == "rPr";
        }
        return Tag::Other;
    }

    // Handles the end of an element opened as `tag`.
    void close(Tag tag) {
        if (tag == Tag::Group) {
            frames_.pop_back();
            return;
        }
        if (tag == Tag::Shape) {
            if (!current_) return;
            Builder b = std::move(*current_);
            current_.reset();
            ParsedShape shape = std::move(b.shape);
            if (shape.rect)
                for (auto f = frames_.rbegin(); f != frames_.rend(); ++f) shape.rect = f->apply(*shape.rect);
            const bool text = shape.placeholder || std::ranges::any_of(shape.paragraphs, [](const auto& p) { return !blank(p); });
            if (b.element == "sp" && text) shape.kind = ShapeKind::Text;
            else if (b.element == "pic") shape.kind = ShapeKind::Picture;
            else if (b.element == "graphicFrame" && b.is_table) shape.kind = ShapeKind::Table;
            else shape.kind = ShapeKind::Other;
            shapes.push_back(std::move(shape));
            return;
        }
        const bool in_cell = std::ranges::find(stack_, Tag::TableCell) != stack_.end();
        if (!current_) return;
        Builder& b = *current_;
        switch (tag) {
        case Tag::Paragraph: {
            b.in_paragraph = false;
            std::string text = std::exchange(b.paragraph, {});
            if (in_cell) {
                if (!b.row_style) b.row_style = b.style;
                if (!blank(text)) b.cell.push_back(std::move(text));
            } else {
                b.shape.paragraphs.push_back(std::move(text));
                b.shape.styles.push_back(b.style);
            }
            break;
        }
        case Tag::TableCell: {
            std::string cell;
            for (std::size_t i = 0; i < b.cell.size(); ++i) cell += (i ? " " : "") + b.cell[i];
            b.row.push_back(std::move(cell));
            b.cell.clear();
            break;
        }
        case Tag::TableRow: {
            std::string row;
            for (std::size_t i = 0; i < b.row.size(); ++i) row += (i ? " | " : "") + b.row[i];
            b.shape.paragraphs.push_back(std::move(row));
            b.shape.styles.push_back(b.row_style.value_or(ParaStyle{}));
            b.row_style.reset();
            b.row.clear();
            break;
        }
        default: break;
        }
    }
};

// Reads the shapes of a part's shape tree in document order, groups flattened.
XmlResult<std::vector<ParsedShape>> parse_shapes(std::string_view xml) {
    auto doc = ooxml::xml_reader(xml);
    if (!doc) return std::unexpected(std::move(doc.error()));
    ShapeParser parser;
    if (auto r = parser.walk(**doc); !r) return std::unexpected(std::move(r.error()));
    return std::move(parser.shapes);
}

using Presentation = std::pair<std::optional<PageSize>, std::vector<std::string>>;

// Slide size and the slide relationship ids in presentation order.
XmlResult<Presentation> parse_presentation(std::string_view xml) {
    Presentation out;
    auto r = ooxml::each_element(xml, [&](const pugi::xml_node& e) -> XmlResult<void> {
        const auto name = ooxml::local_name(e);
        if (name == "sldSz") {
            auto cx = ooxml::num_attr(e, "cx");
            if (!cx) return std::unexpected(std::move(cx.error()));
            auto cy = ooxml::num_attr(e, "cy");
            if (!cy) return std::unexpected(std::move(cy.error()));
            if (!*cx || !*cy) return std::unexpected(std::string("p:sldSz lacks cx or cy"));
            if (**cx <= 0.0 || **cy <= 0.0)
                return std::unexpected(std::format("slide size {} x {} EMU is not positive", **cx, **cy));
            out.first = PageSize{pt(**cx), pt(**cy)};
        } else if (name == "sldId") {
            // The relationship id is the prefixed `r:id`; the bare `id` is a number.
            auto rid = ooxml::find_attr(e, [](const pugi::xml_attribute& a) {
                return !ooxml::prefix(a).empty() && ooxml::local_name(a) == "id";
            });
            if (!rid) return std::unexpected(std::string("p:sldId lacks r:id"));
            out.second.emplace_back(*rid);
        }
        return {};
    });
    if (!r) return std::unexpected(std::move(r.error()));
    return out;
}

// Placeholder geometry of a slide layout or master.
struct Placeholder {
    std::string kind;
    std::optional<std::string> idx;
    std::optional<Rect> rect;
};
struct Template {
    std::vector<Placeholder> placeholders;
    std::optional<std::string> parent;
};
using Templates = std::map<std::string, Template, std::less<>>;
using Media = std::map<std::string, std::vector<std::uint8_t>, std::less<>>;

const Rel* find_rel(const std::vector<Rel>& rels, std::string_view kind_suffix) {
    auto it = std::ranges::find_if(rels, [&](const Rel& r) { return r.kind.ends_with(kind_suffix); });
    return it == rels.end() ? nullptr : &*it;
}

// Parses a layout or master part (and a layout's master) into `templates`.
Result<void> load_template(Package& pkg, Templates& templates, const std::string& part) {
    if (templates.contains(part)) return {};
    auto parsed = pkg.parsed<std::vector<ParsedShape>>(part, parse_shapes);
    if (!parsed) return std::unexpected(std::move(parsed.error()));
    Template t;
    if (*parsed)
        for (auto& s : **parsed)
            if (s.placeholder) t.placeholders.push_back({std::move(*s.placeholder), std::move(s.ph_idx), s.rect});
    auto rels = pkg.rels(part);
    if (!rels) return std::unexpected(std::move(rels.error()));
    if (const Rel* r = find_rel(*rels, REL_MASTER)) t.parent = r->target;
    auto parent = t.parent;
    // Inserted before the parent is loaded so a cycle of relationships ends.
    templates.emplace(part, std::move(t));
    if (parent) return load_template(pkg, templates, *parent);
    return {};
}

// Placeholder types that inherit from the same master placeholder.
std::string_view placeholder_family(std::string_view kind) {
    if (kind == "ctrTitle") return "title";
    if (kind == "subTitle" || kind == "obj") return "body";
    return kind;
}

// Geometry of a placeholder from the layout (matched by index, then type) or
// its master (matched by type).
std::optional<Rect> inherited_rect(const Templates& templates, const std::string* layout, std::string_view kind,
                                   const std::optional<std::string>& ph_idx) {
    const auto family = placeholder_family(kind);
    const std::string* part = layout;
    std::optional<std::string_view> idx;
    if (ph_idx) idx = *ph_idx;
    while (part != nullptr) {
        auto t = templates.find(*part);
        if (t == templates.end()) return std::nullopt;
        const auto& ps = t->second.placeholders;
        const Placeholder* found = nullptr;
        if (idx) {
            auto it = std::ranges::find_if(ps, [&](const Placeholder& p) { return p.idx && *p.idx == *idx; });
            if (it != ps.end()) found = &*it;
        }
        if (found == nullptr) {
            auto it = std::ranges::find_if(ps, [&](const Placeholder& p) { return placeholder_family(p.kind) == family; });
            if (it != ps.end()) found = &*it;
        }
        if (found != nullptr && found->rect) return found->rect;
        part = t->second.parent ? &*t->second.parent : nullptr;
        idx.reset();
    }
    return std::nullopt;
}

// Non empty paragraphs joined with newlines, none when there are none.
template <typename Range>
std::optional<std::string> joined_text(const Range& paragraphs) {
    std::optional<std::string> out;
    for (const std::string& p : paragraphs) {
        if (blank(p)) continue;
        if (out) out->push_back('\n');
        else out.emplace();
        *out += p;
    }
    return out;
}

// Reads one slide part with its pictures and notes.
Result<Slide> read_slide(Package& pkg, Templates& templates, Media& media, std::size_t index, const std::string& part) {
    auto parsed = pkg.parsed<std::vector<ParsedShape>>(part, parse_shapes);
    if (!parsed) return std::unexpected(std::move(parsed.error()));
    if (!*parsed) return std::unexpected(pkg.invalid(std::format("slide part {} is missing", part)));
    auto rels = pkg.rels(part);
    if (!rels) return std::unexpected(std::move(rels.error()));
    const Rel* layout_rel = find_rel(*rels, REL_LAYOUT);
    const std::string* layout = layout_rel ? &layout_rel->target : nullptr;
    if (layout)
        if (auto r = load_template(pkg, templates, *layout); !r) return std::unexpected(std::move(r.error()));

    Slide slide;
    slide.index = index;
    slide.shapes.reserve((*parsed)->size());
    for (auto& p : **parsed) {
        std::optional<Rect> rect = p.rect;
        if (!rect && p.placeholder) rect = inherited_rect(templates, layout, *p.placeholder, p.ph_idx);
        std::optional<std::string> image;
        if (p.embed) {
            auto it = std::ranges::find_if(*rels, [&](const Rel& r) { return r.id == *p.embed; });
            if (it != rels->end()) image = it->target;
        }
        if (image && !media.contains(*image)) {
            auto bytes = pkg.read(*image);
            if (!bytes) return std::unexpected(std::move(bytes.error()));
            if (*bytes) media.emplace(*image, std::move(**bytes));
        }
        const Rect r = rect.value_or(Rect{});
        slide.shapes.push_back(Shape{std::move(p.name), p.kind, std::move(p.placeholder), pt(r.x), pt(r.y), pt(r.w), pt(r.h),
                                     std::move(p.paragraphs), std::move(image), std::move(p.styles)});
    }

    const auto& shapes = slide.shapes;
    auto title_it = std::ranges::find_if(shapes, [](const Shape& s) { return is_title(s.placeholder); });
    const auto title_at = static_cast<std::size_t>(title_it - shapes.begin());
    if (title_it != shapes.end()) slide.title = joined_text(title_it->paragraphs);
    for (std::size_t i = 0; i < shapes.size(); ++i) {
        const auto& s = shapes[i];
        if (i == title_at || (s.kind != ShapeKind::Text && s.kind != ShapeKind::Table) ||
            (s.placeholder && *s.placeholder == "sldNum"))
            continue;
        for (const auto& p : s.paragraphs)
            if (!blank(p)) slide.body.push_back(p);
    }

    if (const Rel* notes = find_rel(*rels, REL_NOTES)) {
        auto np = pkg.parsed<std::vector<ParsedShape>>(notes->target, parse_shapes);
        if (!np) return std::unexpected(std::move(np.error()));
        if (*np) {
            std::vector<std::string> paragraphs;
            for (const auto& s : **np)
                if (s.placeholder && *s.placeholder == "body")
                    paragraphs.insert(paragraphs.end(), s.paragraphs.begin(), s.paragraphs.end());
            slide.notes = joined_text(paragraphs);
        }
    }
    return slide;
}

// The box a shape is drawn in, in points. Placeholders whose geometry couldn't
// be resolved get the usual title or body area.
std::optional<Rect32> shape_box(const Shape& shape, bool title, PageSize slide) {
    if (shape.width > 0.0F && shape.height > 0.0F) return Rect32{shape.x, shape.y, shape.width, shape.height};
    if (!shape.placeholder || shape.paragraphs.empty()) return std::nullopt;
    const float w = slide.width, h = slide.height;
    if (title) return Rect32{w * 0.05F, h * 0.04F, w * 0.9F, h * 0.16F};
    return Rect32{w * 0.05F, h * 0.24F, w * 0.9F, h * 0.7F};
}

// Draws a shape's paragraphs wrapped to its box, top aligned.
void draw_paragraphs(Canvas& canvas, const Fonts& fonts, const RenderOpts& opts, const Shape& shape, bool title, Rect32 box,
                     float scale) {
    const float left = box.x + INSET_X * scale;
    const float max_width = std::max(box.w - 2.0F * INSET_X * scale, 1.0F);
    const auto bottom = static_cast<float>(canvas.height());
    float top = box.y + INSET_Y * scale;
    for (std::size_t i = 0; i < shape.paragraphs.size(); ++i) {
        const ParaStyle style = i < shape.styles.size() ? shape.styles[i] : ParaStyle{};
        const float size = style.size.value_or(opts.typography.slide) * scale;
        const bool bold = style.bold || title;
        const TextStyle text_style = opts.text(size).with_bold(bold);
        for (std::string_view line : fonts.wrap(shape.paragraphs[i], size, bold, max_width)) {
            if (top >= bottom) return;
            canvas.draw_text(fonts, text_style, left, top, line);
            top += fonts.line_height(size);
        }
    }
}

Result<std::unique_ptr<PptxDoc>> malformed() {
    return std::unexpected(DocsError::invalid(Format::Pptx, "malformed document part"));
}

} // namespace

std::string_view to_string(ShapeKind kind) {
    switch (kind) {
    case ShapeKind::Text: return "text";
    case ShapeKind::Picture: return "picture";
    case ShapeKind::Table: return "table";
    case ShapeKind::Other: return "other";
    }
    return "other";
}

json::Value Shape::to_json() const {
    return json::Object{
        {"name", name},
        {"kind", to_string(kind)},
        {"placeholder", opt_json(placeholder)},
        {"x", static_cast<double>(x)},
        {"y", static_cast<double>(y)},
        {"width", static_cast<double>(width)},
        {"height", static_cast<double>(height)},
        {"paragraphs", strings_json(paragraphs)},
        {"image", opt_json(image)},
    };
}

json::Value Slide::to_json() const {
    json::Array out;
    out.reserve(shapes.size());
    for (const auto& s : shapes) out.push_back(s.to_json());
    return json::Object{
        {"index", static_cast<std::uint64_t>(index)},
        {"title", opt_json(title)},
        {"body", strings_json(body)},
        {"notes", opt_json(notes)},
        {"shapes", std::move(out)},
    };
}

Result<std::unique_ptr<PptxDoc>> PptxDoc::parse(std::span<const std::uint8_t> bytes) {
    try {
        auto pkg = Package::open(bytes, Format::Pptx);
        if (!pkg) return std::unexpected(std::move(pkg.error()));
        auto presentation = pkg->parsed<Presentation>(PRESENTATION_PART, parse_presentation);
        if (!presentation) return std::unexpected(std::move(presentation.error()));
        if (!*presentation) return std::unexpected(pkg->invalid(std::format("{} is missing", PRESENTATION_PART)));
        auto& [size, order] = **presentation;
        auto presentation_rels = pkg->rels(PRESENTATION_PART);
        if (!presentation_rels) return std::unexpected(std::move(presentation_rels.error()));
        Templates templates;
        std::unique_ptr<PptxDoc> doc(new PptxDoc());
        doc->slides_.reserve(order.size());
        for (std::size_t index = 0; index < order.size(); ++index) {
            const auto& rid = order[index];
            auto rel = std::ranges::find_if(*presentation_rels,
                                            [&](const Rel& r) { return r.id == rid && r.kind.ends_with(REL_SLIDE); });
            if (rel == presentation_rels->end())
                return std::unexpected(pkg->invalid(std::format("slide relationship {} is missing", rid)));
            auto slide = read_slide(*pkg, templates, doc->media_, index, rel->target);
            if (!slide) return std::unexpected(std::move(slide.error()));
            doc->slides_.push_back(std::move(*slide));
        }
        doc->size_ = size.value_or(DEFAULT_SLIDE_SIZE);
        return doc;
    } catch (const std::exception&) {
        return malformed();
    }
}

std::string PptxDoc::text() const {
    std::string out;
    for (const auto& slide : slides_) {
        if (!out.empty()) out.push_back('\n');
        out += std::format("Slide {}\n", slide.index + 1);
        if (slide.title) out += *slide.title + "\n";
        for (const auto& line : slide.body) out += line + "\n";
        if (slide.notes) out += std::format("Notes: {}\n", *slide.notes);
    }
    return out;
}

Result<PageSize> PptxDoc::page_size(std::size_t index) const {
    if (index >= slides_.size()) return std::unexpected(DocsError::page_range(index, slides_.size()));
    return size_;
}

std::optional<Result<json::Value>> PptxDoc::query(const DataQuery& query) const {
    if (const auto* text = std::get_if<query::Text>(&query.v)) {
        if (!text->page) return Result<json::Value>(json::Value(this->text()));
        if (*text->page >= slides_.size())
            return Result<json::Value>(std::unexpected(DocsError::page_range(*text->page, slides_.size())));
        return Result<json::Value>(slides_[*text->page].to_json());
    }
    if (std::holds_alternative<query::Slides>(query.v)) {
        json::Array out;
        out.reserve(slides_.size());
        for (const auto& s : slides_) out.push_back(s.to_json());
        return Result<json::Value>(json::Value(std::move(out)));
    }
    return std::nullopt;
}

Result<Rgba> PptxDoc::render(std::size_t index, float scale, const Fonts& fonts, const RenderOpts& opts) const {
    if (index >= slides_.size()) return std::unexpected(DocsError::page_range(index, slides_.size()));
    const auto [width, height] = size_.pixels(scale);
    Canvas canvas(width, height, opts.palette.page);
    for (const auto& shape : slides_[index].shapes) {
        const bool title = is_title(shape.placeholder);
        const auto box = shape_box(shape, title, size_);
        if (!box) continue;
        const Rect32 px{box->x * scale, box->y * scale, box->w * scale, box->h * scale};
        switch (shape.kind) {
        case ShapeKind::Picture: {
            // Draws a picture into its box, or a rule coloured box when it can't be decoded.
            std::optional<Rgba> decoded;
            if (shape.image)
                if (auto it = media_.find(*shape.image); it != media_.end())
                    if (auto format = sniff(it->second); format && is_image(*format))
                        if (auto img = images::open(*format, it->second))
                            if (auto page = (*img)->render_page(0, 1.0F, Fonts::none())) decoded = std::move(*page);
            if (decoded) canvas.blit(*decoded, px.x, px.y, px.w, px.h);
            else canvas.fill_rect(px.x, px.y, px.w, px.h, opts.palette.rule);
            break;
        }
        case ShapeKind::Table: canvas.stroke_rect(px.x, px.y, px.w, px.h, scale, opts.palette.grid); break;
        case ShapeKind::Text:
        case ShapeKind::Other: break;
        }
        draw_paragraphs(canvas, fonts, opts, shape, title, px, scale);
    }
    return std::move(canvas).into_rgba();
}

} // namespace loa::surfaces::docs
