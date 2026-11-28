#include "loa/ui/session/transcript/view.hpp"

#include <algorithm>
#include <cmath>
#include <functional>

#include <QApplication>
#include <QClipboard>
#include <QFocusEvent>
#include <QFrame>
#include <QGridLayout>
#include <QGuiApplication>
#include <QHBoxLayout>
#include <QKeyEvent>
#include <QKeySequence>
#include <QLabel>
#include <QMouseEvent>
#include <QPainter>
#include <QResizeEvent>
#include <QScrollArea>
#include <QScrollBar>
#include <QTextLayout>
#include <QTimer>
#include <QVBoxLayout>

#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/kit/widgets.hpp"

namespace loa::ui::session::transcript {

namespace core = loa::core;
namespace kit = loa::ui::kit;
using kit::ButtonKind;
using kit::theme::ColorRole;
using kit::theme::LayoutRole;
using kit::theme::SpaceRole;
using kit::theme::TextRole;

namespace {

// Line height of body text, relative to its size.
constexpr float LINE_HEIGHT = 1.6F;

int spi(SpaceRole r) { return static_cast<int>(std::lround(kit::theme::sp(r))); }
QString qs(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }
QColor color(ColorRole r) { return kit::qt::qcolor(kit::theme::tokens()->color(r)); }

} // namespace

// ---- CopyButton ----------------------------------------------------------------------------

CopyButton::CopyButton(std::string_view label, std::function<std::string()> value, Emit post, QWidget* parent)
    : QPushButton(qs(label), parent) {
    setObjectName(QStringLiteral("transcript-copy"));
    setAccessibleName(qs(label));
    setFont(kit::qt::font(TextRole::Caption));
    setCursor(Qt::PointingHandCursor);
    kit::qt::style_button(*this, ButtonKind::ghost());
    connect(this, &QPushButton::clicked, this, [value = std::move(value), post = std::move(post)] { post(msg::Copy{value()}); });
}

// ---- SelectableText ------------------------------------------------------------------------

// One block of rich text laid out once per width; the selection area hit-tests and paints it.
class SelectableText final : public QWidget {
  public:
    SelectableText(SelectionArea& area, std::size_t index, const md::Text& spans, float size, float line_height, bool mono, QWidget* parent)
        : QWidget(parent), area_(area), index_(index), line_height_(line_height) {
        setAttribute(Qt::WA_TransparentForMouseEvents);
        setSizePolicy(QSizePolicy::Preferred, QSizePolicy::Preferred);
        QFont font = kit::qt::font(mono ? TextRole::Mono : TextRole::Body);
        font.setPixelSize(static_cast<int>(std::lround(size)));
        QString text;
        QList<QTextLayout::FormatRange> formats;
        for (const auto& span : spans) {
            const auto piece = qs(span.text);
            QTextCharFormat f;
            if (span.strong) f.setFontWeight(QFont::Bold);
            if (span.emphasis) f.setFontItalic(true);
            if (span.strike) f.setFontStrikeOut(true);
            if (span.code) {
                f.setFontFamilies({kit::qt::font(TextRole::Mono).family()});
                f.setBackground(color(ColorRole::Raised));
            }
            if (span.link) {
                f.setForeground(color(ColorRole::Accent));
                f.setFontUnderline(true);
            }
            formats.append({static_cast<int>(text.size()), static_cast<int>(piece.size()), f});
            text += piece;
        }
        utf8_ = text.toStdString();
        // Byte offset of every UTF-16 position.
        bytes_.reserve(static_cast<std::size_t>(text.size()) + 1);
        std::size_t at = 0;
        for (qsizetype i = 0; i < text.size(); ++i) {
            bytes_.push_back(at);
            const auto c = text.at(i).unicode();
            if (text.at(i).isHighSurrogate()) {
                bytes_.push_back(at);
                at += 4;
                ++i;
            } else {
                at += c < 0x80 ? 1 : c < 0x800 ? 2 : 3;
            }
        }
        bytes_.push_back(at);
        layout_.setText(text);
        layout_.setFont(font);
        layout_.setFormats(formats);
        QTextOption opt;
        opt.setWrapMode(QTextOption::WrapAtWordBoundaryOrAnywhere);
        layout_.setTextOption(opt);
        relayout(400);
        setAccessibleName(text.left(80));
    }

    [[nodiscard]] const std::string& utf8() const { return utf8_; }
    [[nodiscard]] std::size_t index() const { return index_; }
    [[nodiscard]] QString plain() const { return layout_.text(); }

    // The caret under `p` (local coordinates), as a byte offset; past the end below the text.
    [[nodiscard]] std::size_t caret(QPointF p) const {
        if (p.y() >= height_) return utf8_.size();
        for (int i = 0; i < layout_.lineCount(); ++i) {
            const auto line = layout_.lineAt(i);
            if (p.y() < line.y() + line.height()) return byte(line.xToCursor(p.x()));
        }
        return utf8_.size();
    }

    bool hasHeightForWidth() const override { return true; }
    int heightForWidth(int w) const override {
        const_cast<SelectableText*>(this)->relayout(w);
        return static_cast<int>(std::ceil(height_));
    }
    QSize sizeHint() const override {
        const_cast<SelectableText*>(this)->relayout(width_ > 0 ? width_ : 400);
        return {static_cast<int>(std::ceil(natural_)), static_cast<int>(std::ceil(height_))};
    }
    QSize minimumSizeHint() const override { return {10, static_cast<int>(std::ceil(line_px()))}; }

  protected:
    void resizeEvent(QResizeEvent*) override { relayout(width()); }
    void paintEvent(QPaintEvent*) override {
        QPainter p(this);
        if (const auto r = area_.selection().range(index_)) {
            QTextLayout::FormatRange sel;
            sel.start = utf16(r->first);
            sel.length = utf16(r->second) - sel.start;
            auto bg = color(ColorRole::Accent);
            bg.setAlphaF(0.45F);
            sel.format.setBackground(bg);
            layout_.draw(&p, {0, 0}, {sel});
        } else {
            layout_.draw(&p, {0, 0});
        }
    }

  private:
    [[nodiscard]] float line_px() const { return QFontMetricsF(layout_.font()).height() * (line_height_ > 0 ? line_height_ : 1.0F); }
    [[nodiscard]] std::size_t byte(int u16) const { return bytes_[static_cast<std::size_t>(std::clamp<int>(u16, 0, static_cast<int>(bytes_.size()) - 1))]; }
    [[nodiscard]] int utf16(std::size_t b) const {
        const auto it = std::lower_bound(bytes_.begin(), bytes_.end(), b);
        return static_cast<int>(it - bytes_.begin());
    }

    void relayout(int w) {
        if (w == width_ && height_ > 0) return;
        width_ = std::max(w, 10);
        const float lh = line_px();
        float y = 0;
        natural_ = 0;
        layout_.beginLayout();
        while (true) {
            auto line = layout_.createLine();
            if (!line.isValid()) break;
            line.setLineWidth(width_);
            if (line_height_ > 0) line.setLeadingIncluded(false);
            const float extra = line_height_ > 0 ? std::max(0.0F, lh - static_cast<float>(line.height())) : 0.0F;
            line.setPosition({0, y + extra / 2.0F});
            y += static_cast<float>(line.height()) + extra;
            natural_ = std::max(natural_, static_cast<float>(line.naturalTextWidth()));
        }
        layout_.endLayout();
        height_ = std::max(y, lh);
    }

    SelectionArea& area_;
    std::size_t index_;
    float line_height_;
    QTextLayout layout_;
    std::string utf8_;
    std::vector<std::size_t> bytes_;
    int width_ = -1;
    float height_ = 0, natural_ = 0;
};

// ---- SelectionArea -------------------------------------------------------------------------

SelectionArea::SelectionArea(Emit post, QWidget* parent) : QWidget(parent), emit_(std::move(post)) {
    setObjectName(QStringLiteral("transcript-selection"));
    setAccessibleName(QStringLiteral("selectable text"));
    setFocusPolicy(Qt::ClickFocus);
    setCursor(Qt::IBeamCursor);
}

SelectableText* SelectionArea::text(const md::Text& spans, float size, float line_height, bool mono, QWidget* parent) {
    std::vector<Selection::Link> links;
    std::string all;
    for (const auto& s : spans) {
        if (s.link) links.push_back({all.size(), all.size() + s.text.size(), *s.link});
        all += s.text;
    }
    const auto index = selection_.add(std::move(all), std::move(links));
    auto* block = new SelectableText(*this, index, spans, size, line_height, mono, parent);
    blocks_.push_back(block);
    return block;
}

std::optional<Endpoint> SelectionArea::hit(const QPoint& at, bool clamp) const {
    const SelectableText* previous = nullptr;
    for (const auto* b : blocks_) {
        const QRect r(b->mapTo(this, QPoint(0, 0)), b->size());
        if (at.y() < r.top()) {
            if (!clamp) return std::nullopt;
            if (previous) return Endpoint{previous->index(), previous->utf8().size()};
            return Endpoint{b->index(), 0};
        }
        if (at.y() < r.bottom() + 1) {
            if (!clamp && (at.x() < r.left() || at.x() > r.right())) return std::nullopt;
            return Endpoint{b->index(), b->caret(QPointF(at - r.topLeft()))};
        }
        previous = b;
    }
    if (clamp && previous) return Endpoint{previous->index(), previous->utf8().size()};
    return std::nullopt;
}

void SelectionArea::repaint_blocks() {
    for (auto* b : blocks_) b->update();
}

bool SelectionArea::copy() {
    auto text = selection_.copy();
    if (text.empty()) return false;
    emit_(msg::Copy{std::move(text)});
    return true;
}

void SelectionArea::mousePressEvent(QMouseEvent* e) {
    if (e->button() != Qt::LeftButton) return QWidget::mousePressEvent(e);
    setFocus(Qt::MouseFocusReason);
    const auto at = e->position().toPoint();
    if (const auto end = hit(at, false)) selection_.press(*end, static_cast<float>(at.x()), static_cast<float>(at.y()));
    else selection_.clear();
    repaint_blocks();
    e->accept();
}

void SelectionArea::mouseMoveEvent(QMouseEvent* e) {
    if (!selection_.selecting()) return QWidget::mouseMoveEvent(e);
    const auto at = e->position().toPoint();
    selection_.drag(hit(at, true), static_cast<float>(at.x()), static_cast<float>(at.y()));
    repaint_blocks();
    e->accept();
}

void SelectionArea::mouseReleaseEvent(QMouseEvent* e) {
    if (e->button() != Qt::LeftButton || !selection_.selecting()) return QWidget::mouseReleaseEvent(e);
    const auto link = selection_.release();
    repaint_blocks();
    e->accept();
    if (link) emit_(msg::Link{*link});
}

void SelectionArea::keyPressEvent(QKeyEvent* e) {
    if (e->matches(QKeySequence::Copy) && copy()) return e->accept();
    QWidget::keyPressEvent(e);
}

void SelectionArea::focusOutEvent(QFocusEvent* e) {
    if (e->reason() != Qt::PopupFocusReason) {
        selection_.clear();
        repaint_blocks();
    }
    QWidget::focusOutEvent(e);
}

// ---- Prose ---------------------------------------------------------------------------------

namespace {

struct Prose {
    SelectionArea* area;
    const std::string& source;
    const Emit& post;
    std::size_t code_index = 0;

    float body() const { return kit::theme::text_size(TextRole::Body); }

    QWidget* blocks(const std::vector<md::Block>& items, float spacing, QWidget* parent) {
        auto* w = new QWidget(parent);
        auto* col = new QVBoxLayout(w);
        col->setContentsMargins(0, 0, 0, 0);
        col->setSpacing(static_cast<int>(std::lround(spacing)));
        for (std::size_t i = 0; i < items.size(); ++i) col->addWidget(block(items[i], i, w));
        return w;
    }

    QWidget* block(const md::Block& b, std::size_t index, QWidget* parent) {
        return std::visit(
            [&](const auto& x) -> QWidget* {
                using T = std::decay_t<decltype(x)>;
                if constexpr (std::is_same_v<T, md::Paragraph>) {
                    return area->text(x.text, body(), LINE_HEIGHT, false, parent);
                } else if constexpr (std::is_same_v<T, md::Heading>) {
                    const float size = x.level == 1   ? body() * 2.0F
                                       : x.level == 2 ? body() * 1.75F
                                       : x.level == 3 ? body() * 1.5F
                                       : x.level == 4 ? body() * 1.25F
                                                      : body();
                    auto spans = x.text;
                    for (auto& s : spans) s.strong = true;
                    auto* wrap = new QWidget(parent);
                    auto* col = new QVBoxLayout(wrap);
                    col->setContentsMargins(0, index == 0 ? 0 : static_cast<int>(body() / 2.0F), 0, 0);
                    col->addWidget(area->text(spans, size, 0, false, wrap));
                    return wrap;
                } else if constexpr (std::is_same_v<T, md::CodeBlock>) {
                    auto* well = new QFrame(parent);
                    well->setObjectName(QStringLiteral("transcript-code"));
                    well->setAccessibleName(QStringLiteral("code block"));
                    kit::qt::style_container(*well, kit::Surface::Well);
                    auto* col = new QVBoxLayout(well);
                    col->setContentsMargins(0, 0, 0, 0);
                    col->setSpacing(0);
                    const auto at = code_index++;
                    auto* copy = new CopyButton("Copy code block", [src = source, at, code = x.code] { return copy_code(src, at, code); }, post, well);
                    col->addWidget(copy, 0, Qt::AlignLeft);
                    auto* inner = new QWidget(well);
                    auto* pad = new QVBoxLayout(inner);
                    pad->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::M), spi(SpaceRole::M), spi(SpaceRole::M));
                    pad->addWidget(area->text({md::Span{code_text(x.code)}}, kit::theme::text_size(TextRole::Mono), 0, true, inner));
                    col->addWidget(inner);
                    return well;
                } else if constexpr (std::is_same_v<T, md::List>) {
                    auto* w = new QWidget(parent);
                    auto* col = new QVBoxLayout(w);
                    col->setContentsMargins(spi(SpaceRole::S), 0, spi(SpaceRole::S), 0);
                    col->setSpacing(spi(SpaceRole::M) / 4);
                    unsigned number = x.start.value_or(1);
                    for (const auto& bullet : x.bullets) {
                        auto* row = new QWidget(w);
                        auto* line = new QHBoxLayout(row);
                        line->setContentsMargins(0, 0, 0, 0);
                        line->setSpacing(spi(SpaceRole::S));
                        QString mark = bullet.task ? (*bullet.task ? QStringLiteral("\u2611") : QStringLiteral("\u2610"))
                                       : x.start   ? QString::number(number++) + QLatin1Char('.')
                                                   : QStringLiteral("\u2022");
                        auto* m = kit::widgets::label(mark.toStdString(), TextRole::Body, kit::Tone::Muted, row);
                        line->addWidget(m, 0, Qt::AlignTop);
                        line->addWidget(blocks(bullet.items, kit::theme::sp(SpaceRole::M) * 0.5F, row), 1);
                        col->addWidget(row);
                    }
                    return w;
                } else if constexpr (std::is_same_v<T, md::Quote>) {
                    auto* w = new QWidget(parent);
                    auto* line = new QHBoxLayout(w);
                    line->setContentsMargins(0, 0, 0, 0);
                    line->setSpacing(spi(SpaceRole::M));
                    auto* guide = new QFrame(w);
                    guide->setFrameShape(QFrame::VLine);
                    guide->setAccessibleName(QStringLiteral("quote"));
                    line->addWidget(guide);
                    line->addWidget(blocks(x.items, kit::theme::sp(SpaceRole::M), w), 1);
                    return w;
                } else if constexpr (std::is_same_v<T, md::Rule>) {
                    auto* rule = new QFrame(parent);
                    rule->setFrameShape(QFrame::HLine);
                    rule->setAccessibleName(QStringLiteral("rule"));
                    return rule;
                } else {
                    auto* w = new QWidget(parent);
                    w->setAccessibleName(QStringLiteral("table"));
                    auto* grid = new QGridLayout(w);
                    grid->setContentsMargins(0, 0, 0, 0);
                    grid->setHorizontalSpacing(spi(SpaceRole::M));
                    for (std::size_t r = 0; r < x.rows.size(); ++r)
                        for (std::size_t c = 0; c < x.rows[r].size(); ++c) {
                            auto spans = x.rows[r][c];
                            if (r < x.header_rows)
                                for (auto& s : spans) s.strong = true;
                            grid->addWidget(area->text(spans, body(), LINE_HEIGHT, false, w), static_cast<int>(r), static_cast<int>(c));
                        }
                    return w;
                }
            },
            b.v);
    }
};

} // namespace

QWidget* prose_widget(const std::vector<md::Block>& blocks, const std::string& source, const Emit& post) {
    auto* area = new SelectionArea(post);
    auto* col = new QVBoxLayout(area);
    col->setContentsMargins(0, 0, 0, 0);
    Prose prose{area, source, post};
    col->addWidget(prose.blocks(blocks, kit::theme::sp(SpaceRole::M), area));
    return area;
}

QWidget* selectable(std::string_view text, const Emit& post) {
    auto* area = new SelectionArea(post);
    auto* col = new QVBoxLayout(area);
    col->setContentsMargins(0, 0, 0, 0);
    col->addWidget(area->text({md::Span{std::string(text)}}, kit::theme::text_size(TextRole::Body), LINE_HEIGHT, false, area));
    return area;
}

// ---- VirtualList ---------------------------------------------------------------------------

VirtualList::VirtualList(QWidget* parent) : QWidget(parent) {
    setObjectName(QStringLiteral("transcript-rows"));
    setAccessibleName(QStringLiteral("transcript rows"));
}

QWidget* VirtualList::row(NodeId key) const {
    const auto it = rows_.find(key);
    return it == rows_.end() ? nullptr : it->second.widget;
}

void VirtualList::sync(float top, float bottom, const std::vector<NodeId>& keys, const std::vector<std::size_t>& versions,
                       const Build& build, const Measured& measured) {
    top_ = top;
    bottom_ = bottom;
    std::unordered_map<NodeId, Row> next;
    for (std::size_t i = 0; i < keys.size(); ++i) {
        auto it = rows_.find(keys[i]);
        if (it != rows_.end() && it->second.version == versions[i]) {
            next.emplace(keys[i], it->second);
            rows_.erase(it);
            continue;
        }
        Row row{build(keys[i]), versions[i], it != rows_.end() ? it->second.reported : -1.0F};
        row.widget->setParent(this);
        row.widget->show();
        next.emplace(keys[i], row);
    }
    // Rows that left the window, or were rebuilt. A click handler may still be running inside one.
    for (auto& [_, row] : rows_) {
        row.widget->hide();
        row.widget->deleteLater();
    }
    rows_ = std::move(next);
    order_ = keys;
    relayout(measured);
}

void VirtualList::relayout(const Measured& measured) {
    const int w = std::max(width(), 100);
    float y = top_;
    for (const auto key : order_) {
        auto& row = rows_.at(key);
        const int h = row.widget->hasHeightForWidth() ? row.widget->heightForWidth(w) : row.widget->sizeHint().height();
        row.widget->setGeometry(0, static_cast<int>(std::lround(y)), w, h);
        y += static_cast<float>(h);
        if (std::abs(static_cast<float>(h) - row.reported) >= HEIGHT_EPSILON) {
            row.reported = static_cast<float>(h);
            if (measured) measured(key, static_cast<float>(h));
        }
    }
    resize(w, static_cast<int>(std::lround(y + bottom_)));
}

// ---- TranscriptView ------------------------------------------------------------------------

namespace {

void mix(std::size_t& h, std::size_t v) { h ^= v + 0x9e3779b97f4a7c15ULL + (h << 6) + (h >> 2); }

void mix_node(std::size_t& h, const core::Node& n) {
    mix(h, std::hash<std::string>{}(n.text));
    for (const auto& [name, value] : n.attrs) {
        mix(h, std::hash<std::string>{}(name));
        std::visit(
            [&](const auto& v) {
                using T = std::decay_t<decltype(v)>;
                if constexpr (std::is_same_v<T, core::Value::Bytes> || std::is_same_v<T, core::Value::List>) mix(h, v.size());
                else mix(h, std::hash<T>{}(v));
            },
            value.variant());
    }
}

// Changes whenever what the row draws changes.
std::size_t version(const State& state, const core::Dom& dom, const Item& item) {
    std::size_t h = static_cast<std::size_t>(item.kind);
    mix(h, state.expanded(item.key));
    mix(h, state.read_only());
    mix(h, item.calls.size());
    const auto subtree = [&](NodeId id) {
        for (const auto& n : dom.descendants(id)) mix_node(h, n);
    };
    if (item.calls.empty()) subtree(item.key);
    else
        for (const auto id : item.calls) subtree(id);
    return h;
}

} // namespace

TranscriptView::TranscriptView(QWidget* parent) : QWidget(parent) {
    setObjectName(QStringLiteral("transcript"));
    setAccessibleName(QStringLiteral("transcript"));
    auto* layout = new QVBoxLayout(this);
    layout->setContentsMargins(0, 0, 0, 0);
    scroll_ = new QScrollArea(this);
    scroll_->setAccessibleName(QStringLiteral("transcript scroll"));
    scroll_->setFrameShape(QFrame::NoFrame);
    scroll_->setWidgetResizable(false);
    scroll_->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    list_ = new VirtualList;
    scroll_->setWidget(list_);
    layout->addWidget(scroll_);
    jump_ = new QPushButton(QStringLiteral("Jump to latest \u2193"), this);
    jump_->setAccessibleName(QStringLiteral("Jump to latest \u2193"));
    jump_->setFont(kit::qt::font(TextRole::Label));
    kit::qt::style_button(*jump_, ButtonKind::primary());
    jump_->hide();
    connect(jump_, &QPushButton::clicked, this, [this] { send(msg::JumpToLatest{}); });
    auto* bar = scroll_->verticalScrollBar();
    connect(bar, &QScrollBar::valueChanged, this, [this] {
        if (!rendering_) report_scroll();
    });
    connect(bar, &QScrollBar::rangeChanged, this, [this] {
        if (!rendering_) report_scroll();
    });
}

void TranscriptView::render(const State& state, const core::Dom& dom) {
    rendering_ = true;
    bottom_inset_ = state.bottom_inset();
    const auto window = state.window();
    const auto& items = state.items();
    const auto& heights = state.heights();
    const auto above = static_cast<float>(heights.prefix(window.begin));
    const auto below = static_cast<float>(heights.total() - heights.prefix(window.end));
    std::vector<NodeId> keys;
    std::vector<std::size_t> versions;
    for (auto i = window.begin; i < window.end; ++i) {
        keys.push_back(items[i].key);
        versions.push_back(version(state, dom, items[i]));
    }
    const Emit post = [this](Message m) { send(std::move(m)); };
    const auto build = [&](NodeId key) -> QWidget* {
        const auto& item = items[*state.position(key)];
        QWidget* content = nullptr;
        if (item.kind == ItemKind::Decision) {
            content = slot_ ? slot_(Slot{item.kind, key}) : nullptr;
        } else if (const auto cx = state.context(dom, item, true)) {
            content = renderer(item.kind).view(item, *cx, post);
        }
        if (!content) content = new QWidget;
        auto* row = new QWidget;
        row->setAccessibleName(qs(name(item.kind)));
        auto* line = new QHBoxLayout(row);
        const int pad = static_cast<int>(PADDING / 2.0F);
        line->setContentsMargins(spi(SpaceRole::Gutter), pad, spi(SpaceRole::Gutter), pad);
        content->setMaximumWidth(static_cast<int>(kit::theme::width(LayoutRole::ComposerWidth)));
        line->addStretch(1);
        line->addWidget(content, 1000);
        line->addStretch(1);
        return row;
    };
    list_->resize(scroll_->viewport()->width(), list_->height());
    std::vector<std::pair<NodeId, float>> measured;
    list_->sync(above, below + bottom_inset_, keys, versions, build, [&](NodeId key, float h) { measured.emplace_back(key, h); });
    auto* bar = scroll_->verticalScrollBar();
    bar->setRange(0, std::max(0, list_->height() - scroll_->viewport()->height()));
    if (state.is_following()) bar->setValue(bar->maximum());
    jump_->setVisible(!state.is_following());
    place_jump();
    rendering_ = false;
    // Sizes and the scroll position reach the state after this render, as sensors report in iced.
    QTimer::singleShot(0, this, [this, measured = std::move(measured)] {
        for (const auto& [key, h] : measured) send(msg::Measured{key, h});
        report_scroll();
    });
}

void TranscriptView::apply(const Task& task) {
    auto* bar = scroll_->verticalScrollBar();
    for (const auto& o : task.ops) {
        std::visit(
            [&](const auto& x) {
                using T = std::decay_t<decltype(x)>;
                if constexpr (std::is_same_v<T, op::SnapToEnd>) bar->setValue(bar->maximum());
                else if constexpr (std::is_same_v<T, op::ScrollBy>) bar->setValue(bar->value() + static_cast<int>(std::lround(x.y)));
                else QGuiApplication::clipboard()->setText(qs(x.text));
            },
            o);
    }
}

void TranscriptView::report_scroll() {
    const auto* bar = scroll_->verticalScrollBar();
    const ScrollMetrics m{static_cast<float>(bar->value()), static_cast<float>(scroll_->viewport()->height()),
                          static_cast<float>(list_->height())};
    if (m == reported_) return;
    reported_ = m;
    send(msg::Scrolled{m});
}

void TranscriptView::place_jump() {
    const auto hint = jump_->sizeHint();
    const int bottom = spi(SpaceRole::L) + static_cast<int>(bottom_inset_);
    jump_->setGeometry((width() - hint.width()) / 2, height() - bottom - hint.height(), hint.width(), hint.height());
    jump_->raise();
}

void TranscriptView::resizeEvent(QResizeEvent* e) {
    QWidget::resizeEvent(e);
    place_jump();
    report_scroll();
}

} // namespace loa::ui::session::transcript
