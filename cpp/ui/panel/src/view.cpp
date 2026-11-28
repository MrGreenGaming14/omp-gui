// Qt views of the diff, files and session tree panes. Port of the `view` functions of
// `crates/loa-ui-panel/src/{lib,diff/mod,files,session_tree}.rs`.

#include "loa/ui/panel/view.hpp"

#include <algorithm>
#include <cmath>
#include <format>

#include <QAbstractScrollArea>
#include <QFontMetricsF>
#include <QHBoxLayout>
#include <QLabel>
#include <QMouseEvent>
#include <QPainter>
#include <QPushButton>
#include <QScrollArea>
#include <QScrollBar>
#include <QVBoxLayout>

#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/widgets.hpp"

namespace loa::ui::panel {

namespace theme = loa::ui::kit::theme;
namespace w = loa::ui::kit::widgets;
using loa::ui::kit::ButtonKind;
using loa::ui::kit::Tone;
using loa::view::theme::Icon;
using theme::ColorRole;
using theme::SpaceRole;
using theme::TextRole;

float list_row_height() { return 2.0F * theme::text_size(TextRole::Body); }
float indent() { return theme::sp(SpaceRole::M); }

namespace {

QColor color(ColorRole r) { return kit::qt::qcolor(theme::tokens()->color(r)); }
QString qs(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }
int px(float f) { return static_cast<int>(std::lround(f)); }

// A flat row button holding `content`, as `ButtonKind::Row`.
QPushButton* row_button(QWidget* content, bool selected, std::function<void()> on) {
    auto* b = new QPushButton;
    b->setAccessibleName(QStringLiteral("row"));
    b->setFlat(true);
    auto* l = new QHBoxLayout(b);
    l->setContentsMargins(px(theme::sp(SpaceRole::S)), 0, px(theme::sp(SpaceRole::S)), 0);
    l->addWidget(content);
    b->setFixedHeight(px(list_row_height()));
    kit::qt::style_button(*b, ButtonKind::row(selected));
    QObject::connect(b, &QPushButton::clicked, b, std::move(on));
    return b;
}

QWidget* spacer(float width) {
    auto* s = new QWidget;
    s->setFixedWidth(std::max(0, px(width)));
    return s;
}

// A column of rows in a scroll area, replacing the previous column.
void set_rows(QScrollArea* scroll, const std::vector<QWidget*>& rows) {
    auto* col = new QWidget;
    auto* l = new QVBoxLayout(col);
    l->setContentsMargins(px(theme::sp(SpaceRole::S)), 0, px(theme::sp(SpaceRole::S)), 0);
    l->setSpacing(0);
    for (auto* r : rows) l->addWidget(r);
    l->addStretch(1);
    // Deferred: a render can run inside Qt's own walk over these widgets (a resize while showing).
    if (auto* old = scroll->takeWidget()) {
        old->hide();
        old->deleteLater();
    }
    scroll->setWidget(col);
}

QScrollArea* make_scroll(QWidget* owner, std::string_view name) {
    auto* l = new QVBoxLayout(owner);
    l->setContentsMargins(0, 0, 0, 0);
    auto* s = new QScrollArea(owner);
    s->setWidgetResizable(true);
    s->setFrameShape(QFrame::NoFrame);
    s->setAccessibleName(qs(name));
    l->addWidget(s);
    return s;
}

} // namespace

// ---- Diff -----------------------------------------------------------------------------------

// The diff rows in view, painted by arithmetic over a uniform row height; only the rows the
// state reports visible are prepared.
class DiffBody : public QAbstractScrollArea {
  public:
    struct Side {
        bool present = false;
        diff::Tag tag = diff::Tag::Equal;
        QString old_no, new_no;
        std::vector<std::pair<QString, bool>> parts; // text runs, emphasized or not
    };
    struct Item {
        enum class Kind : std::uint8_t { Hunk, Fold, Line, Pair } kind = Kind::Hunk;
        QString text;          // hunk header or fold caption
        std::size_t fold = 0;  // Fold
        Side a, b;             // Line uses `a`; Pair `a` old, `b` new
    };

    std::function<void(const diff::Message&)> send;

    DiffBody() {
        setAccessibleName(QStringLiteral("diff body"));
        setFrameShape(QFrame::NoFrame);
        connect(verticalScrollBar(), &QScrollBar::valueChanged, this, [this](int v) { report(v); });
    }

    void set(std::size_t total, std::size_t first, std::vector<Item> items, int digits) {
        total_ = total, first_ = first, items_ = std::move(items), digits_ = digits;
        const auto rh = diff::row_height();
        verticalScrollBar()->setRange(0, std::max(0, px(static_cast<float>(total) * rh) - viewport()->height()));
        verticalScrollBar()->setPageStep(viewport()->height());
        verticalScrollBar()->setSingleStep(px(rh));
        viewport()->update();
    }
    void snap_top() { verticalScrollBar()->setValue(0); }

  protected:
    void resizeEvent(QResizeEvent* e) override {
        QAbstractScrollArea::resizeEvent(e);
        // Queued: the app re-renders in response, which must not run inside Qt's layout pass.
        QMetaObject::invokeMethod(this, [this] { report(verticalScrollBar()->value()); }, Qt::QueuedConnection);
    }
    void mousePressEvent(QMouseEvent* e) override {
        const auto row = static_cast<std::size_t>((e->position().y() + verticalScrollBar()->value()) / diff::row_height());
        if (row >= first_ && row - first_ < items_.size() && items_[row - first_].kind == Item::Kind::Fold && send)
            send(diff::msg::Expand{items_[row - first_].fold});
    }
    void paintEvent(QPaintEvent*) override {
        QPainter p(viewport());
        const auto rh = diff::row_height();
        const auto font = kit::qt::font(TextRole::Mono);
        p.setFont(font);
        const QFontMetricsF fm(font);
        const auto number_w = static_cast<float>(digits_) * fm.horizontalAdvance(QLatin1Char('0')) + theme::sp(SpaceRole::L);
        const auto width = static_cast<float>(viewport()->width());
        const auto scroll = static_cast<float>(verticalScrollBar()->value());
        for (std::size_t k = 0; k < items_.size(); ++k) {
            const auto y = static_cast<float>(first_ + k) * rh - scroll;
            if (y + rh < 0 || y > static_cast<float>(viewport()->height())) continue;
            const auto& it = items_[k];
            const QRectF r(0, y, width, rh);
            switch (it.kind) {
            case Item::Kind::Hunk:
                p.fillRect(r, color(ColorRole::Raised));
                p.setPen(color(ColorRole::Text2));
                p.drawText(r.adjusted(theme::sp(SpaceRole::M), 0, 0, 0), Qt::AlignVCenter, it.text);
                break;
            case Item::Kind::Fold:
                p.setPen(color(ColorRole::Text2));
                p.drawText(r.adjusted(theme::sp(SpaceRole::S), 0, 0, 0), Qt::AlignVCenter, it.text);
                break;
            case Item::Kind::Line: paint_side(p, r, it.a, number_w, true, fm); break;
            case Item::Kind::Pair:
                paint_side(p, QRectF(0, y, width / 2, rh), it.a, number_w, false, fm);
                paint_side(p, QRectF(width / 2, y, width / 2, rh), it.b, number_w, false, fm);
                break;
            }
        }
    }

  private:
    void paint_side(QPainter& p, const QRectF& r, const Side& s, float number_w, bool unified, const QFontMetricsF& fm) {
        p.save();
        p.setClipRect(r);
        if (!s.present) {
            p.fillRect(r, color(ColorRole::Raised));
            p.restore();
            return;
        }
        if (s.tag != diff::Tag::Equal) p.fillRect(r, color(s.tag == diff::Tag::Delete ? ColorRole::RemovedBg : ColorRole::AddedBg));
        auto x = static_cast<float>(r.x());
        p.setPen(color(ColorRole::Text2));
        auto number = [&](const QString& n) {
            p.drawText(QRectF(x, r.y(), number_w, r.height()), Qt::AlignVCenter | Qt::AlignRight, n);
            x += number_w;
        };
        number(s.old_no);
        if (unified) {
            number(s.new_no);
            const auto [sign, role] = s.tag == diff::Tag::Equal ? std::pair{QStringLiteral(" "), ColorRole::Text2}
                                      : s.tag == diff::Tag::Delete ? std::pair{QStringLiteral("-"), ColorRole::Danger}
                                                                   : std::pair{QStringLiteral("+"), ColorRole::Ok};
            x += theme::sp(SpaceRole::M);
            p.setPen(color(role));
            p.drawText(QPointF(x, r.y() + (r.height() + fm.ascent() - fm.descent()) / 2), sign);
            x += static_cast<float>(fm.horizontalAdvance(sign)) + theme::sp(SpaceRole::S);
        } else {
            x += theme::sp(SpaceRole::S);
        }
        const auto strong = s.tag == diff::Tag::Insert ? ColorRole::Ok : ColorRole::Danger;
        const auto tint = theme::tokens()->color(strong).scale_alpha(theme::tokens()->alpha(theme::AlphaRole::Tint));
        const auto base = r.y() + (r.height() + fm.ascent() - fm.descent()) / 2;
        for (const auto& [text, emph] : s.parts) {
            const auto adv = static_cast<float>(fm.horizontalAdvance(text));
            if (emph) p.fillRect(QRectF(x, r.y(), adv, r.height()), kit::qt::qcolor(tint));
            p.setPen(color(ColorRole::Text));
            p.drawText(QPointF(x, base), text);
            x += adv;
        }
        p.restore();
    }
    void report(int value) {
        if (send) send(diff::msg::Scrolled{static_cast<float>(value), static_cast<float>(viewport()->height())});
    }

    std::size_t total_ = 0, first_ = 0;
    std::vector<Item> items_;
    int digits_ = 1;
};

namespace {

DiffBody::Side side(const diff::FileDiff& f, std::size_t i, bool old_number, bool unified) {
    const auto& l = f.lines()[i];
    DiffBody::Side s{.present = true, .tag = l.tag};
    auto no = [](std::optional<std::uint32_t> n) { return n ? QString::number(*n) : QString(); };
    if (unified) s.old_no = no(l.old_no), s.new_no = no(l.new_no);
    else s.old_no = no(old_number ? l.old_no : l.new_no);
    const auto text = f.line_text(i);
    std::size_t at = 0;
    for (const auto r : f.emphasis(i)) {
        if (r.start > at) s.parts.emplace_back(qs(text.substr(at, r.start - at)), false);
        s.parts.emplace_back(qs(text.substr(r.start, r.size())), true);
        at = r.end;
    }
    if (at < text.size()) s.parts.emplace_back(qs(text.substr(at)), false);
    return s;
}

} // namespace

DiffView::DiffView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("diff"));
    layout_ = new QVBoxLayout(this);
    layout_->setContentsMargins(0, 0, 0, 0);
    layout_->setSpacing(0);
    body_ = new DiffBody;
    body_->send = [this](const diff::Message& m) { emit message(m); };
    layout_->addWidget(body_, 1);
}

void DiffView::apply(const diff::Task& task) {
    for (const auto& op : task.ops)
        if (const auto* s = std::get_if<shell::op::SnapTo>(&op); s && s->id == diff::SCROLL_ID) body_->snap_top();
}

void DiffView::render(const diff::State& state) {
    if (head_) {
        layout_->removeWidget(head_);
        head_->hide();
        head_->deleteLater();
        head_ = nullptr;
    }
    head_ = new QWidget;
    auto* head = new QVBoxLayout(head_);
    head->setContentsMargins(0, 0, 0, 0);
    head->setSpacing(0);
    layout_->insertWidget(0, head_);
    const auto* file = state.selected();
    if (!file) {
        head->addWidget(w::label(state.note() ? std::string_view(*state.note()) : "No changes", TextRole::Body, Tone::Muted));
        body_->hide();
        return;
    }
    // Collapse chevron and path, then the change counts and the unified/split switch.
    auto* bar = new QWidget;
    bar->setAccessibleName(QStringLiteral("diff toolbar"));
    auto* row = new QHBoxLayout(bar);
    row->setContentsMargins(px(theme::sp(SpaceRole::S)), px(theme::sp(SpaceRole::Xs)), px(theme::sp(SpaceRole::S)), px(theme::sp(SpaceRole::Xs)));
    row->setSpacing(px(theme::sp(SpaceRole::Xs)));
    row->addWidget(w::icon_button(state.collapsed() ? Icon::ChevronRight : Icon::ChevronDown, state.collapsed() ? "Expand" : "Collapse",
                                  [this] { emit message(diff::msg::ToggleCollapsed{}); }));
    row->addWidget(w::text_button(file->path(), ButtonKind::row(false), [this] { emit message(diff::msg::Open{}); }));
    row->addStretch(1);
    row->addWidget(w::label(std::format("+{}", file->added()), TextRole::Caption, Tone::Ok));
    row->addWidget(w::label(std::format("-{}", file->removed()), TextRole::Caption, Tone::Danger));
    const auto other = state.mode() == diff::Mode::Unified ? std::pair{diff::Mode::Split, "Split"} : std::pair{diff::Mode::Unified, "Unified"};
    row->addWidget(w::text_button(other.second, ButtonKind::row(false), [this, m = other.first] { emit message(diff::msg::SetMode{m}); }));
    head->addWidget(bar);
    if (state.files().size() > 1) {
        std::vector<QWidget*> entries;
        for (std::size_t i = 0; i < state.files().size(); ++i) {
            const auto& f = state.files()[i];
            auto* line = new QWidget;
            auto* l = new QHBoxLayout(line);
            l->setContentsMargins(0, 0, 0, 0);
            l->setSpacing(px(theme::sp(SpaceRole::S)));
            static constexpr std::array<std::string_view, 4> MARKERS{"A", "D", "M", "R"};
            l->addWidget(w::label(MARKERS[static_cast<std::size_t>(f.status())], TextRole::Label, Tone::Muted));
            l->addWidget(w::label(f.path(), TextRole::Label, Tone::Normal), 1);
            l->addWidget(w::label(std::format("+{}", f.added()), TextRole::Label, Tone::Ok));
            l->addWidget(w::label(std::format("-{}", f.removed()), TextRole::Label, Tone::Danger));
            entries.push_back(row_button(line, i == state.selected_index(), [this, i] { emit message(diff::msg::SelectFile{i}); }));
        }
        auto* list = new QScrollArea;
        list->setFrameShape(QFrame::NoFrame);
        list->setWidgetResizable(true);
        list->setAccessibleName(QStringLiteral("diff files"));
        set_rows(list, entries);
        list->setFixedHeight(std::min(px(static_cast<float>(entries.size()) * list_row_height()), 140));
        head->addWidget(list);
    }
    if (state.collapsed()) {
        body_->hide();
        return;
    }
    if (file->binary) {
        head->addWidget(w::label("Binary file", TextRole::Body, Tone::Muted));
        body_->hide();
        return;
    }
    body_->show();
    const auto range = state.visible_rows();
    std::vector<DiffBody::Item> items;
    items.reserve(range.size());
    using Item = DiffBody::Item;
    for (auto k = range.start; k < range.end; ++k) {
        const auto& r = state.rows()[k];
        Item it;
        if (const auto* h = std::get_if<diff::row::Hunk>(&r)) {
            it.kind = Item::Kind::Hunk;
            it.text = qs(std::format("@@ -{},{} +{},{} @@ {}", h->hunk.old_start, h->hunk.old_len, h->hunk.new_start, h->hunk.new_len, file->section(h->hunk)));
        } else if (const auto* fo = std::get_if<diff::row::Fold>(&r)) {
            it.kind = Item::Kind::Fold;
            it.fold = fo->fold;
            it.text = fo->hidden == 1 ? QStringLiteral("Expand 1 unchanged line") : qs(std::format("Expand {} unchanged lines", fo->hidden));
        } else if (const auto* ln = std::get_if<diff::row::Line>(&r)) {
            it.kind = Item::Kind::Line;
            it.a = side(*file, ln->line, true, true);
        } else if (const auto* pr = std::get_if<diff::row::Pair>(&r)) {
            it.kind = Item::Kind::Pair;
            if (pr->old_line) it.a = side(*file, *pr->old_line, true, false);
            if (pr->new_line) it.b = side(*file, *pr->new_line, false, false);
        }
        items.push_back(std::move(it));
    }
    const auto digits = static_cast<int>(std::to_string(std::max<std::uint32_t>(file->max_line_no(), 1)).size());
    body_->set(state.rows().size(), range.start, std::move(items), digits);
}

// ---- Files ----------------------------------------------------------------------------------

FilesView::FilesView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("files"));
    scroll_ = make_scroll(this, "files list");
}

void FilesView::render(const files::State& state) {
    std::vector<QWidget*> rows;
    for (const auto& r : state.rows()) {
        const auto& n = state.nodes()[r.node];
        if (r.kind == files::Row::Kind::Error) {
            auto* line = new QWidget;
            auto* l = new QHBoxLayout(line);
            l->setContentsMargins(0, 0, 0, 0);
            l->addWidget(spacer(static_cast<float>(n.depth + 1) * indent() + theme::sp(SpaceRole::Xl) + theme::sp(SpaceRole::S)));
            l->addWidget(w::label(n.error.value_or(""), TextRole::Body, Tone::Danger), 1);
            line->setFixedHeight(px(list_row_height()));
            rows.push_back(line);
            continue;
        }
        auto* content = new QWidget;
        auto* l = new QHBoxLayout(content);
        l->setContentsMargins(0, 0, 0, 0);
        l->setSpacing(px(theme::sp(SpaceRole::S)));
        l->addWidget(spacer(static_cast<float>(n.depth - 1) * indent()));
        auto* chevron = new QLabel;
        chevron->setFixedWidth(px(indent()));
        if (n.is_dir) chevron->setPixmap(kit::muted(n.expanded ? Icon::ChevronDown : Icon::ChevronRight, theme::text_size(TextRole::Caption)));
        l->addWidget(chevron);
        auto* icon = new QLabel;
        icon->setPixmap(kit::icon(n.is_dir ? Icon::Folder : Icon::FileText, theme::text_size(TextRole::Body)));
        l->addWidget(icon);
        l->addWidget(w::label(n.name, TextRole::Body, Tone::Normal), 1);
        auto* b = row_button(content, state.selected() == r.node, [this, i = r.node] { emit message(files::msg::Click{i}); });
        b->setAccessibleName(qs(n.name));
        rows.push_back(b);
    }
    set_rows(scroll_, rows);
}

// ---- Session tree ---------------------------------------------------------------------------

SessionTreeView::SessionTreeView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("session tree"));
    scroll_ = make_scroll(this, "session tree list");
}

void SessionTreeView::render(const session_tree::State& state) {
    namespace k = session_tree::kind;
    std::vector<QWidget*> rows;
    for (const auto& r : state.rows()) {
        auto* line = new QWidget;
        line->setFixedHeight(px(list_row_height()));
        auto* l = new QHBoxLayout(line);
        l->setContentsMargins(0, 0, 0, 0);
        l->setSpacing(px(theme::sp(SpaceRole::Xs)));
        l->addWidget(spacer(indent() * static_cast<float>(r.depth)));
        const auto i = r.index;
        const bool selected = state.selected() == i;
        auto pick = [&](std::string_view text, bool bold, bool accent) {
            auto* name = w::label(text, TextRole::Body, accent ? Tone::Accent : Tone::Normal);
            if (bold) {
                auto f = name->font();
                f.setBold(true);
                name->setFont(f);
            }
            return row_button(name, selected, [this, i] { emit message(session_tree::msg::Select{i}); });
        };
        if (const auto* s = std::get_if<k::Session>(&r.kind)) {
            if (!state.has_subtree(i)) l->addWidget(spacer(indent()));
            else
                l->addWidget(w::icon_button(s->collapsed ? Icon::ChevronRight : Icon::ChevronDown, s->collapsed ? "Expand" : "Collapse",
                                            [this, i] { emit message(session_tree::msg::Toggle{i}); }));
            l->addWidget(pick(s->title.empty() ? s->id : s->title, s->current, s->on_path), 1);
            if (!s->current) l->addWidget(w::text_button("Open", ButtonKind::ghost(), [this, i] { emit message(session_tree::msg::Open{i}); }));
        } else if (const auto* f = std::get_if<k::ForkPoint>(&r.kind)) {
            auto* icon = new QLabel;
            icon->setPixmap(kit::muted(Icon::GitCompare, theme::text_size(TextRole::Caption)));
            l->addWidget(icon);
            l->addWidget(w::label(f->first_line ? std::string(*f->first_line) : std::format("at #{}", f->seq), TextRole::Label, Tone::Muted), 1);
        } else if (const auto* t = std::get_if<k::Turn>(&r.kind)) {
            l->addWidget(pick(t->first_line, false, false), 1);
            l->addWidget(w::text_button("Rewind", ButtonKind::ghost(), [this, i] { emit message(session_tree::msg::Rewind{i}); }));
            l->addWidget(w::text_button("Fork", ButtonKind::ghost(), [this, i] { emit message(session_tree::msg::Fork{i}); }));
        }
        rows.push_back(line);
    }
    set_rows(scroll_, rows);
}

} // namespace loa::ui::panel
