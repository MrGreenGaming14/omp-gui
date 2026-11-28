// The terminal tab's Qt view. Port of `State::view` in `crates/loa-ui-terminal/src/state.rs` and the drawing
// and input of `canvas.rs` (background, decoration and text passes over resolved cells, then the cursor).

#include "loa/ui/terminal/view.hpp"

#include <cmath>
#include <format>
#include <vector>

#include <QClipboard>
#include <QFontInfo>
#include <QFontMetricsF>
#include <QGuiApplication>
#include <QHBoxLayout>
#include <QKeyEvent>
#include <QLabel>
#include <QMouseEvent>
#include <QPainter>
#include <QPushButton>
#include <QVBoxLayout>
#include <QWheelEvent>

#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/widgets.hpp"

namespace loa::ui::terminal {

namespace theme = loa::ui::kit::theme;
namespace w = loa::ui::kit::widgets;
using loa::ui::kit::ButtonKind;
using loa::ui::kit::Tone;
using loa::view::theme::Icon;
using theme::SpaceRole;
using theme::TextRole;

namespace {

constexpr auto MULTI_CLICK = std::chrono::milliseconds(400);
constexpr float LINE_HEIGHT = 1.25F;

QString qs(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }
int px(float f) { return static_cast<int>(std::lround(f)); }
QColor qcolor(Color c) { return loa::ui::kit::qt::qcolor(c); }

Named named_key(int key) {
    switch (key) {
    case Qt::Key_Return:
    case Qt::Key_Enter: return Named::Enter;
    case Qt::Key_Backspace: return Named::Backspace;
    case Qt::Key_Tab:
    case Qt::Key_Backtab: return Named::Tab;
    case Qt::Key_Escape: return Named::Escape;
    case Qt::Key_Space: return Named::Space;
    case Qt::Key_Up: return Named::ArrowUp;
    case Qt::Key_Down: return Named::ArrowDown;
    case Qt::Key_Right: return Named::ArrowRight;
    case Qt::Key_Left: return Named::ArrowLeft;
    case Qt::Key_Home: return Named::Home;
    case Qt::Key_End: return Named::End;
    case Qt::Key_Insert: return Named::Insert;
    case Qt::Key_Delete: return Named::Delete;
    case Qt::Key_PageUp: return Named::PageUp;
    case Qt::Key_PageDown: return Named::PageDown;
    default:
        if (key >= Qt::Key_F1 && key <= Qt::Key_F12) return static_cast<Named>(static_cast<int>(Named::F1) + (key - Qt::Key_F1));
        return Named::Other;
    }
}

Modifiers modifiers_of(Qt::KeyboardModifiers q) {
    Modifiers m;
    if (q & Qt::ShiftModifier) m.bits |= Modifiers::SHIFT;
    if (q & Qt::AltModifier) m.bits |= Modifiers::ALT;
#if defined(__APPLE__)
    // Qt reports Cmd as Control and Control as Meta on macOS.
    if (q & Qt::ControlModifier) m.bits |= Modifiers::LOGO;
    if (q & Qt::MetaModifier) m.bits |= Modifiers::CTRL;
#else
    if (q & Qt::ControlModifier) m.bits |= Modifiers::CTRL;
    if (q & Qt::MetaModifier) m.bits |= Modifiers::LOGO;
#endif
    return m;
}

bool printable(const QString& t) { return !t.isEmpty() && t[0].unicode() >= 0x20 && t[0].unicode() != 0x7f; }

} // namespace

Metrics measure(const QFont& font) {
    static constexpr std::string_view SAMPLE = "MMMMMMMMMMMMMMMMMMMM";
    const QFontMetricsF fm(font);
    const auto size = static_cast<float>(QFontInfo(font).pixelSize());
    return Metrics{size, static_cast<float>(fm.horizontalAdvance(qs(SAMPLE))) / static_cast<float>(SAMPLE.size()), size * LINE_HEIGHT};
}

Metrics default_metrics() { return measure(loa::ui::kit::qt::font(TextRole::Mono)); }

// ---- Canvas -------------------------------------------------------------------------------------

Canvas::Canvas(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("terminal canvas"));
    setObjectName(QStringLiteral("terminal canvas"));
    setFocusPolicy(Qt::StrongFocus);
    setAttribute(Qt::WA_OpaquePaintEvent);
    setAttribute(Qt::WA_InputMethodEnabled, false);
    setCursor(Qt::IBeamCursor);
    setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Expanding);
}

void Canvas::render(TabId tab, const Emulator* emulator, Metrics metrics) {
    tab_ = tab;
    emulator_ = emulator;
    metrics_ = metrics;
    update();
}

void Canvas::publish(CanvasEvent e) { emit input(msg::Canvas{tab_, std::move(e)}); }

GridSize Canvas::grid() const { return metrics_.grid_for(static_cast<float>(width()), static_cast<float>(height())); }

void Canvas::paintEvent(QPaintEvent*) {
    QPainter p(this);
    const auto palette = TermPalette::from_tokens(*theme::tokens());
    p.fillRect(rect(), qcolor(palette.background));
    if (!emulator_) return;
    const auto size = emulator_->size();
    const std::size_t rows = std::max<std::uint16_t>(size.rows, 1), cols = std::max<std::uint16_t>(size.cols, 2);
    const float cw = metrics_.cell_width, ch = metrics_.cell_height;
    QFont base = loa::ui::kit::qt::font(TextRole::Mono);
    base.setPixelSize(std::max(1, px(metrics_.font_size)));
    const auto& overrides = emulator_->colors();
    for (std::size_t row = 0; row < rows; ++row) {
        // Runs of one text style, drawn together.
        QString run;
        float run_x = 0;
        QColor run_color;
        std::uint8_t run_style = 0;
        const auto flush = [&] {
            const QString text = run.trimmed().isEmpty() ? QString() : run;
            if (!text.isEmpty()) {
                QFont f = base;
                f.setBold((run_style & flags::BOLD) != 0);
                f.setItalic((run_style & flags::ITALIC) != 0);
                p.setFont(f);
                p.setPen(run_color);
                p.drawText(QRectF(run_x, static_cast<float>(row) * ch, cw * static_cast<float>(run.size()) + cw, ch),
                           Qt::AlignLeft | Qt::AlignVCenter | Qt::TextDontClip, text);
            }
            run.clear();
        };
        for (std::size_t col = 0; col < cols; ++col) {
            const ViewportCell at{col, row};
            const auto cell = emulator_->cell(at);
            Color fg = palette.resolve(cell.fg, overrides), bg = palette.resolve(cell.bg, overrides);
            if (cell.flags & flags::INVERSE) std::swap(fg, bg);
            if (emulator_->is_selected(at)) fg = palette.foreground, bg = palette.selection;
            if (cell.flags & flags::DIM) fg = mix(fg, bg, 0.33F);
            const float x = static_cast<float>(col) * cw, y = static_cast<float>(row) * ch;
            const float wpx = cw * static_cast<float>(cell.width);
            if (bg != palette.background) p.fillRect(QRectF(x, y, wpx, ch), qcolor(bg));
            const float thick = std::max(metrics_.font_size / 14.0F, 1.0F);
            if (cell.flags & flags::UNDERLINE) p.fillRect(QRectF(x, y + ch - thick, wpx, thick), qcolor(fg));
            if (cell.flags & flags::STRIKEOUT) p.fillRect(QRectF(x, y + ch * 0.5F, wpx, thick), qcolor(fg));
            const bool blank = cell.text.empty() || cell.text == U" " || cell.text == U"\t" || (cell.flags & flags::HIDDEN);
            const auto style = static_cast<std::uint8_t>(cell.flags & (flags::BOLD | flags::ITALIC));
            const QColor color = qcolor(fg);
            if (cell.width == 2 || (!blank && !run.isEmpty() && (color != run_color || style != run_style))) flush();
            if (run.isEmpty()) {
                run_x = x;
                run_color = color;
                run_style = style;
            }
            if (blank) {
                run.append(QLatin1Char(' '));
            } else {
                run.append(QString::fromUcs4(cell.text.data(), static_cast<qsizetype>(cell.text.size())));
            }
            if (cell.width == 2) {
                flush();
                ++col;
            }
        }
        flush();
    }
    // The cursor, over the content.
    const auto cursor = emulator_->cursor();
    if (cursor.shape == CursorShape::Hidden || cursor.row < 0 || cursor.row >= static_cast<long>(rows)) return;
    const auto row = static_cast<std::size_t>(cursor.row);
    const auto cell = emulator_->cell({cursor.col, row});
    const QRectF box(static_cast<float>(cursor.col) * cw, static_cast<float>(row) * ch, cw * static_cast<float>(cell.width), ch);
    const float thick = std::max(metrics_.font_size / 8.0F, 1.0F);
    const QColor color = qcolor(palette.cursor);
    if (cursor.shape == CursorShape::Block && hasFocus()) {
        p.fillRect(box, color);
        if (!cell.text.empty() && cell.text != U" ") {
            QFont f = base;
            f.setBold((cell.flags & flags::BOLD) != 0);
            f.setItalic((cell.flags & flags::ITALIC) != 0);
            p.setFont(f);
            p.setPen(qcolor(palette.background));
            p.drawText(box, Qt::AlignLeft | Qt::AlignVCenter | Qt::TextDontClip,
                       QString::fromUcs4(cell.text.data(), static_cast<qsizetype>(cell.text.size())));
        }
    } else if (cursor.shape == CursorShape::Beam) {
        p.fillRect(QRectF(box.x(), box.y(), thick, ch), color);
    } else if (cursor.shape == CursorShape::Underline) {
        p.fillRect(QRectF(box.x(), box.y() + ch - thick, box.width(), thick), color);
    } else {
        p.setPen(QPen(color, thick / 2.0F));
        p.setBrush(Qt::NoBrush);
        p.drawRect(box);
    }
}

void Canvas::resizeEvent(QResizeEvent* e) {
    QWidget::resizeEvent(e);
    const auto g = grid();
    if (sent_grid_ == g) return;
    sent_grid_ = g;
    publish(canvas_event::Resized{g});
}

void Canvas::keyPressEvent(QKeyEvent* e) {
    const auto mods = modifiers_of(e->modifiers());
    const QString text = e->text();
    std::optional<std::string> typed;
    if (printable(text)) typed = text.toStdString();
    Key key;
    if (const auto n = named_key(e->key()); n != Named::Other) {
        key = Key::named(n);
    } else if (typed) {
        key = Key::character(*typed);
    } else if (e->key() >= 0x20 && e->key() < 0x7f) {
        // Ctrl combinations type control characters; the key is the character itself.
        auto c = static_cast<char>(e->key());
        if (c >= 'A' && c <= 'Z' && !mods.shift()) c = static_cast<char>(c - 'A' + 'a');
        key = Key::character(std::string(1, c));
    } else if (!text.isEmpty()) {
        key = Key::unidentified();
    } else {
        QWidget::keyPressEvent(e);
        return;
    }
    publish(canvas_event::KeyPress{std::move(key), mods, std::move(typed)});
    e->accept();
}

void Canvas::mousePressEvent(QMouseEvent* e) {
    if (e->button() != Qt::LeftButton) return QWidget::mousePressEvent(e);
    setFocus(Qt::MouseFocusReason);
    selecting_ = true;
    const auto [cell, side] = metrics_.cell_at(static_cast<float>(e->position().x()), static_cast<float>(e->position().y()), grid());
    const auto now = std::chrono::steady_clock::now();
    int clicks = 1;
    if (last_click_ && last_click_->cell == cell && now - last_click_->at < MULTI_CLICK) clicks = last_click_->count % 3 + 1;
    last_click_ = Click{now, cell, clicks};
    constexpr SelectionType KINDS[] = {SelectionType::Simple, SelectionType::Semantic, SelectionType::Lines};
    publish(canvas_event::SelectStart{cell, side, KINDS[clicks - 1]});
}

void Canvas::mouseMoveEvent(QMouseEvent* e) {
    if (!selecting_) return QWidget::mouseMoveEvent(e);
    const auto [cell, side] = metrics_.cell_at(static_cast<float>(e->position().x()), static_cast<float>(e->position().y()), grid());
    publish(canvas_event::SelectUpdate{cell, side});
}

void Canvas::mouseReleaseEvent(QMouseEvent* e) {
    if (e->button() != Qt::LeftButton || !selecting_) return QWidget::mouseReleaseEvent(e);
    selecting_ = false;
    publish(canvas_event::SelectEnd{});
}

void Canvas::wheelEvent(QWheelEvent* e) {
    float lines = scroll_remainder_;
    if (!e->pixelDelta().isNull()) lines += static_cast<float>(e->pixelDelta().y()) / std::max(metrics_.cell_height, 1.0F);
    else lines += static_cast<float>(e->angleDelta().y()) / 120.0F * 3.0F;
    const float whole = std::trunc(lines);
    scroll_remainder_ = lines - whole;
    e->accept();
    if (whole != 0) publish(canvas_event::Scroll{static_cast<int>(whole)});
}

void Canvas::focusInEvent(QFocusEvent* e) {
    QWidget::focusInEvent(e);
    update();
}

void Canvas::focusOutEvent(QFocusEvent* e) {
    QWidget::focusOutEvent(e);
    update();
}

// Tab and Backtab go to the program, not to focus traversal.
bool Canvas::focusNextPrevChild(bool) { return false; }

// ---- The tab ------------------------------------------------------------------------------------

TerminalView::TerminalView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("terminal"));
    setObjectName(QStringLiteral("terminal"));
    layout_ = new QVBoxLayout(this);
    const int pad = px(theme::sp(SpaceRole::S));
    layout_->setContentsMargins(0, 0, 0, 0);
    layout_->setSpacing(0);
    bar_ = new QWidget;
    layout_->addWidget(bar_);
    auto* rule = new QWidget;
    rule->setObjectName(QStringLiteral("terminal-rule"));
    rule->setFixedHeight(1);
    rule->setAutoFillBackground(true);
    QPalette pal = rule->palette();
    pal.setColor(QPalette::Window, loa::ui::kit::qt::qcolor(theme::tokens()->color(theme::ColorRole::Line)));
    rule->setPalette(pal);
    layout_->addWidget(rule);
    body_ = new QWidget;
    auto* body = new QVBoxLayout(body_);
    body->setContentsMargins(pad, px(theme::sp(SpaceRole::Xs)), pad, px(theme::sp(SpaceRole::Xs)));
    canvas_ = new Canvas;
    body->addWidget(canvas_, 1);
    layout_->addWidget(body_, 1);
    connect(canvas_, &Canvas::input, this, &TerminalView::message);
}

void TerminalView::render(const State& state) {
    // The bar and the status line are rebuilt; the canvas stays so it keeps focus.
    auto* bar = new QWidget;
    bar->setObjectName(QStringLiteral("terminal tabs"));
    bar->setAccessibleName(QStringLiteral("terminal tabs"));
    auto* row = new QHBoxLayout(bar);
    row->setContentsMargins(px(theme::sp(SpaceRole::S)), px(theme::sp(SpaceRole::Xs)), px(theme::sp(SpaceRole::S)), px(theme::sp(SpaceRole::Xs)));
    row->setSpacing(px(theme::sp(SpaceRole::Xs)));
    std::vector<w::Tab> tabs;
    for (const auto id : state.tabs()) {
        auto* close = w::icon_button(Icon::X, "Close terminal", [this, id] { emit message(msg::Close{id}); });
        tabs.push_back(w::Tab{*state.title(id), std::nullopt, state.active() == id, [this, id] { emit message(msg::Select{id}); }, close});
    }
    row->addWidget(w::tab_strip(tabs));
    row->addWidget(w::icon_button(Icon::Plus, "New terminal", [this] { emit message(msg::NewTab{}); }));
    row->addStretch(1);
    layout_->replaceWidget(bar_, bar);
    bar_->hide();
    bar_->deleteLater();
    bar_ = bar;

    auto* body = static_cast<QVBoxLayout*>(body_->layout());
    for (auto* old : body_->findChildren<QWidget*>(QStringLiteral("terminal-extra"), Qt::FindDirectChildrenOnly)) {
        old->hide();
        old->deleteLater();
    }
    const auto active = state.active();
    const Emulator* emulator = active ? state.emulator(*active) : nullptr;
    canvas_->setVisible(emulator != nullptr);
    if (!emulator) {
        auto* empty = new QWidget(body_);
        empty->setObjectName(QStringLiteral("terminal-extra"));
        empty->setAccessibleName(QStringLiteral("no terminal"));
        auto* col = new QVBoxLayout(empty);
        col->setSpacing(px(theme::sp(SpaceRole::S)));
        col->addStretch(1);
        col->addWidget(w::label("No terminal open", TextRole::Label, Tone::Muted), 0, Qt::AlignHCenter);
        const auto text = std::format("Open a terminal in {}", target_title(Target{state.new_tab()}));
        col->addWidget(w::text_button(text, ButtonKind::secondary(), [this] { emit message(msg::NewTab{}); }), 0, Qt::AlignHCenter);
        col->addStretch(1);
        body->addWidget(empty, 1);
        return;
    }
    canvas_->render(*active, emulator, state.metrics());
    const auto* status = state.status(*active);
    std::optional<std::pair<std::string, Tone>> line;
    if (status->kind == Status::Kind::Connecting) line = {std::format("Connecting to {}\u2026", *state.title(*active)), Tone::Muted};
    else if (status->kind == Status::Kind::Failed) line = {status->error, Tone::Danger};
    if (line) {
        auto* label = w::label(line->first, TextRole::Caption, line->second, body_);
        label->setObjectName(QStringLiteral("terminal-extra"));
        label->setAccessibleName(QStringLiteral("terminal status"));
        body->addWidget(label);
    }
}

void TerminalView::apply(const Task& task) {
    auto* clipboard = QGuiApplication::clipboard();
    for (const auto& o : task.ops) {
        if (const auto* write = std::get_if<op::WriteClipboard>(&o)) {
            if (write->primary && !clipboard->supportsSelection()) continue;
            clipboard->setText(qs(write->text), write->primary ? QClipboard::Selection : QClipboard::Clipboard);
        } else if (const auto* read = std::get_if<op::ReadClipboard>(&o)) {
            const QString text = clipboard->text(QClipboard::Clipboard);
            emit message(msg::Paste{read->tab, text.isNull() ? std::nullopt : std::optional(text.toStdString())});
        }
    }
}

} // namespace loa::ui::terminal
