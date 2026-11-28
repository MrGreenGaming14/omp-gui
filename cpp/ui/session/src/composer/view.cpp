#include "loa/ui/session/composer/view.hpp"

#include <cmath>
#include <string>

#include <QAbstractTextDocumentLayout>
#include <QApplication>
#include <QClipboard>
#include <QEvent>
#include <QHBoxLayout>
#include <QInputMethodEvent>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QMimeData>
#include <QMouseEvent>
#include <QPainter>
#include <QPushButton>
#include <QSlider>
#include <QSyntaxHighlighter>
#include <QTextBlock>
#include <QTimer>
#include <QTransform>
#include <QVBoxLayout>

#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/text.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/kit/widgets.hpp"
#include "loa/ui/kit/widgets/popover.hpp"

namespace loa::ui::session::composer {

namespace kit = loa::ui::kit;
namespace theme = loa::ui::kit::theme;
using kit::ButtonKind;
using kit::Icon;
using kit::Surface;
using kit::Tone;
using theme::SpaceRole;
using theme::TextRole;

namespace {

QString qs(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }

int px(float f) { return static_cast<int>(std::lround(f)); }

// UTF-16 position of byte `column` in `line`, and back.
int utf16_of(std::string_view line, std::size_t column) {
    column = std::min(column, line.size());
    return static_cast<int>(QString::fromUtf8(line.data(), static_cast<qsizetype>(column)).size());
}
std::size_t byte_of(const QString& line, int position) { return static_cast<std::size_t>(line.left(position).toUtf8().size()); }

QHBoxLayout* hbox(QWidget* w, SpaceRole spacing) {
    auto* l = new QHBoxLayout(w);
    l->setContentsMargins(0, 0, 0, 0);
    l->setSpacing(px(theme::sp(spacing)));
    return l;
}

QVBoxLayout* vbox(QWidget* w, SpaceRole spacing) {
    auto* l = new QVBoxLayout(w);
    l->setContentsMargins(0, 0, 0, 0);
    l->setSpacing(px(theme::sp(spacing)));
    return l;
}

QLabel* pixmap_label(const QPixmap& p, std::string_view name) {
    auto* l = new QLabel;
    l->setPixmap(p);
    l->setAccessibleName(qs(name));
    return l;
}

QPushButton* button(QWidget* content, ButtonKind kind, std::string_view name) {
    auto* b = new QPushButton;
    auto* l = new QHBoxLayout(b);
    const auto pad = px(theme::sp(SpaceRole::Xs));
    l->setContentsMargins(pad, pad / 2, pad, pad / 2);
    l->addWidget(content);
    content->setAttribute(Qt::WA_TransparentForMouseEvents);
    kit::qt::style_button(*b, kind);
    b->setAccessibleName(qs(name));
    b->setCursor(Qt::PointingHandCursor);
    return b;
}

// Swaps `old` (deleted later, as it may be emitting) for `fresh` in `layout` at `index`.
void swap_in(QBoxLayout* layout, QWidget*& old, QWidget* fresh, int index) {
    if (old) {
        layout->removeWidget(old);
        old->hide();
        old->deleteLater();
    }
    old = fresh;
    layout->insertWidget(index, fresh);
    // Shown now, not on the next event loop turn, unless hidden on purpose.
    if (!fresh->testAttribute(Qt::WA_WState_ExplicitShowHide)) fresh->show();
}

} // namespace

// ---- Keys ------------------------------------------------------------------------------------

std::optional<KeyPress> to_key_press(const QKeyEvent& event) {
    KeyPress p;
    const auto mods = event.modifiers();
    p.modifiers.shift = mods.testFlag(Qt::ShiftModifier);
#if defined(__APPLE__)
    // Qt reports Cmd as Control and Control as Meta on macOS.
    p.modifiers.control = mods.testFlag(Qt::MetaModifier);
    p.modifiers.logo = mods.testFlag(Qt::ControlModifier);
#else
    p.modifiers.control = mods.testFlag(Qt::ControlModifier);
    p.modifiers.logo = mods.testFlag(Qt::MetaModifier);
#endif
    p.modifiers.alt = mods.testFlag(Qt::AltModifier);
    switch (event.key()) {
    case Qt::Key_Return:
    case Qt::Key_Enter: p.key = Key::Enter; break;
    case Qt::Key_Escape: p.key = Key::Escape; break;
    case Qt::Key_Tab:
    case Qt::Key_Backtab: p.key = Key::Tab; break;
    case Qt::Key_Backspace: p.key = Key::Backspace; break;
    case Qt::Key_Delete: p.key = Key::Delete; break;
    case Qt::Key_Left: p.key = Key::ArrowLeft; break;
    case Qt::Key_Right: p.key = Key::ArrowRight; break;
    case Qt::Key_Up: p.key = Key::ArrowUp; break;
    case Qt::Key_Down: p.key = Key::ArrowDown; break;
    case Qt::Key_Home: p.key = Key::Home; break;
    case Qt::Key_End: p.key = Key::End; break;
    default: {
        p.key = Key::Character;
        if (p.modifiers.control || p.modifiers.logo) {
            // Chords use the Latin letter of the key, whatever the layout types.
            const int k = event.key();
            if (k >= Qt::Key_A && k <= Qt::Key_Z) p.text = std::string(1, static_cast<char>('a' + (k - Qt::Key_A)));
            else p.text = event.text().toLower().toStdString();
        } else {
            p.text = event.text().toStdString();
        }
        if (p.text.empty()) return std::nullopt;
    }
    }
    return p;
}

// ---- Highlighter -----------------------------------------------------------------------------

class ComposerHighlighter : public QSyntaxHighlighter {
  public:
    explicit ComposerHighlighter(QTextDocument* doc) : QSyntaxHighlighter(doc) {}

    void set_phase(std::uint8_t phase) {
        if (phase == phase_) return;
        phase_ = phase;
        rehighlight();
    }

  protected:
    void highlightBlock(const QString& text) override {
        const auto line = text.toStdString();
        const auto t = theme::tokens();
        for (const auto& span : spans(line, currentBlock().blockNumber() == 0, phase_)) {
            const auto f = composer::format(span.highlight, *t);
            QTextCharFormat cf;
            cf.setForeground(kit::qt::qcolor(f.color));
            if (f.bold) cf.setFontWeight(QFont::DemiBold);
            const auto from = utf16_of(line, span.begin);
            setFormat(from, utf16_of(line, span.end) - from, cf);
        }
    }

  private:
    std::uint8_t phase_ = 0;
};

// ---- Editor ----------------------------------------------------------------------------------

ComposerEditor::ComposerEditor(QWidget* parent) : QPlainTextEdit(parent) {
    setObjectName(qs(EDITOR_ID));
    setAccessibleName(QStringLiteral("Message"));
    setUndoRedoEnabled(false);
    setFrameShape(QFrame::NoFrame);
    setAttribute(Qt::WA_InputMethodEnabled);
    setTabChangesFocus(false);
    setFont(kit::qt::font(TextRole::Body));
    setVerticalScrollBarPolicy(Qt::ScrollBarAsNeeded);
    setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    setStyleSheet(QStringLiteral("QPlainTextEdit { background: transparent; border: none; }"));
    highlighter_ = new ComposerHighlighter(document());
    connect(document()->documentLayout(), &QAbstractTextDocumentLayout::documentSizeChanged, this, [this] { fit_height(); });
}

void ComposerEditor::fit_height() {
    const auto line = fontMetrics().lineSpacing();
    const auto lines = std::max<qreal>(document()->documentLayout()->documentSize().height(), 1.0);
    const auto pad = 2 * px(theme::sp(SpaceRole::S));
    const auto min_h = px(2.6F * theme::text_size(TextRole::Body));
    const auto h = std::clamp(static_cast<int>(lines * line) + pad, min_h, px(MAX_EDITOR_HEIGHT));
    setMinimumHeight(h);
    setMaximumHeight(h);
}

void ComposerEditor::render(const State& state) {
    rendering_ = true;
    ctx_ = state.key_context();
    selection_ = state.content().selection().value_or(std::string());
    setPlaceholderText(qs(state.placeholder()));
    const auto text = qs(state.text());
    if (toPlainText() != text) {
        const QSignalBlocker block(this);
        setPlainText(text);
    }
    const auto cursor = state.cursor();
    const auto& lines = state.content().lines();
    const auto at = [&](Position p) {
        const auto block = document()->findBlockByNumber(static_cast<int>(p.line));
        return block.position() + utf16_of(lines[std::min(p.line, lines.size() - 1)], p.column);
    };
    QTextCursor c(document());
    if (cursor.selection) {
        c.setPosition(at(*cursor.selection));
        c.setPosition(at(cursor.position), QTextCursor::KeepAnchor);
    } else {
        c.setPosition(at(cursor.position));
    }
    {
        const QSignalBlocker block(this);
        setTextCursor(c);
    }
    highlighter_->set_phase(state.highlight_settings().phase);
    fit_height();
    rendering_ = false;
}

Position ComposerEditor::position_at(const QPoint& point) const {
    const auto c = cursorForPosition(point);
    const auto block = c.block();
    return Position{static_cast<std::size_t>(block.blockNumber()), byte_of(block.text(), c.positionInBlock())};
}

void ComposerEditor::keyPressEvent(QKeyEvent* event) {
    const auto press = to_key_press(*event);
    const auto b = press ? bind(*press, ctx_) : std::nullopt;
    if (!b) {
        event->ignore();
        return;
    }
    event->accept();
    switch (b->clipboard) {
    case Binding::Clipboard::None: break;
    case Binding::Clipboard::Copy:
        if (!selection_.empty()) QApplication::clipboard()->setText(qs(selection_));
        break;
    case Binding::Clipboard::Cut:
        if (!selection_.empty()) {
            QApplication::clipboard()->setText(qs(selection_));
            send(msg::Edit{action::Edit{edit::Delete{}}});
        }
        break;
    case Binding::Clipboard::Paste: {
        const auto text = QApplication::clipboard()->text();
        if (!text.isEmpty()) send(msg::Edit{action::Edit{edit::Paste{text.toStdString()}}});
        break;
    }
    }
    for (const auto& m : b->messages) send(m);
}

void ComposerEditor::inputMethodEvent(QInputMethodEvent* event) {
    send(msg::Composing{!event->preeditString().isEmpty()});
    if (!event->commitString().isEmpty()) send(msg::Edit{action::Edit{edit::Paste{event->commitString().toStdString()}}});
    event->accept();
}

void ComposerEditor::mousePressEvent(QMouseEvent* event) {
    setFocus(Qt::MouseFocusReason);
    if (event->button() == Qt::LeftButton) {
        const auto at = position_at(event->position().toPoint());
        send(event->modifiers().testFlag(Qt::ShiftModifier) ? Message{msg::Edit{action::Drag{at}}} : Message{msg::Edit{action::Click{at}}});
    }
    event->accept();
}

void ComposerEditor::mouseMoveEvent(QMouseEvent* event) {
    if (event->buttons().testFlag(Qt::LeftButton)) send(msg::Edit{action::Drag{position_at(event->position().toPoint())}});
    event->accept();
}

void ComposerEditor::mouseReleaseEvent(QMouseEvent* event) { event->accept(); }

void ComposerEditor::mouseDoubleClickEvent(QMouseEvent* event) {
    const auto at = position_at(event->position().toPoint());
    send(msg::Edit{action::Click{at}});
    send(msg::Edit{action::Move{Motion::WordLeft}});
    send(msg::Edit{action::Select{Motion::WordRight}});
    event->accept();
}

bool ComposerEditor::canInsertFromMimeData(const QMimeData* source) const { return source->hasText(); }

void ComposerEditor::insertFromMimeData(const QMimeData* source) {
    if (source->hasText()) send(msg::Edit{action::Edit{edit::Paste{source->text().toStdString()}}});
}

// ---- View ------------------------------------------------------------------------------------

ComposerView::ComposerView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("composer"));
    setObjectName(QStringLiteral("composer"));
    root_ = vbox(this, SpaceRole::S);
    frame_ = new QWidget;
    frame_->setObjectName(QStringLiteral("composer frame"));
    frame_->setAccessibleName(QStringLiteral("composer frame"));
    frame_->setAttribute(Qt::WA_StyledBackground);
    kit::qt::style_container(*frame_, Surface::Raised);
    frame_layout_ = vbox(frame_, SpaceRole::Xs);
    const auto m = px(theme::sp(SpaceRole::M));
    frame_layout_->setContentsMargins(m, 0, m, px(theme::sp(SpaceRole::Xs)));
    editor_ = new ComposerEditor;
    connect(editor_, &ComposerEditor::message, this, [this](const Message& msg) { send(msg); });
    frame_layout_->addWidget(editor_);
    root_->addWidget(frame_);
    ticker_ = new QTimer(this);
    connect(ticker_, &QTimer::timeout, this, [this] { send(msg::Shimmer{shimmer::now_ms()}); });
}

void ComposerView::apply(const Task& task) {
    if (task.focus_editor) editor_->setFocus(Qt::OtherFocusReason);
}

void ComposerView::changeEvent(QEvent* event) {
    if (event->type() == QEvent::ActivationChange && window())
        send(msg::WindowFocused{static_cast<WindowId>(window()->winId()), window()->isActiveWindow()});
    QWidget::changeEvent(event);
}

void ComposerView::render(const State& state) {
    // Above the frame: the queue, the `/` and `@` popup, and the bar.
    auto* top = new QWidget;
    auto* top_layout = vbox(top, SpaceRole::S);
    if (!state.queue().empty()) top_layout->addWidget(build_queue(state));
    if (state.popup_kind() && !state.open_picker()) top_layout->addWidget(build_popup(state));
    top_layout->addWidget(build_bar(state));
    swap_in(root_, dynamic_top_, top, 0);

    auto* head = new QWidget;
    auto* head_layout = vbox(head, SpaceRole::Xs);
    if (state.indicator()) head_layout->addWidget(build_working(state));
    if (std::ranges::any_of(state.images(), &Image::shown)) head_layout->addWidget(build_images(state));
    head->setVisible(head_layout->count() > 0);
    swap_in(frame_layout_, dynamic_head_, head, 0);

    editor_->render(state);
    swap_in(frame_layout_, dynamic_tail_, build_controls(state), 2);

    const auto tick = state.tick();
    if (!tick) ticker_->stop();
    else if (!ticker_->isActive() || ticker_->interval() != static_cast<int>(tick->count())) ticker_->start(static_cast<int>(tick->count()));
}

QWidget* ComposerView::build_queue(const State& state) {
    auto* box = new QWidget;
    box->setObjectName(QStringLiteral("composer queue"));
    box->setAccessibleName(QStringLiteral("Queued messages"));
    box->setAttribute(Qt::WA_StyledBackground);
    kit::qt::style_container(*box, Surface::Popup);
    auto* rows = vbox(box, SpaceRole::Xs);
    const auto s = px(theme::sp(SpaceRole::S));
    rows->setContentsMargins(s, s, s, s);
    for (const auto& item : state.queue()) {
        auto* row = new QWidget;
        auto* l = hbox(row, SpaceRole::S);
        l->addWidget(kit::widgets::label("Queued", TextRole::Caption, Tone::Muted));
        auto* input = new QLineEdit(qs(item.text));
        input->setPlaceholderText(QStringLiteral("Empty message is skipped"));
        input->setAccessibleName(QStringLiteral("Queued message"));
        input->setFont(kit::qt::font(TextRole::Label));
        const auto id = item.id;
        connect(input, &QLineEdit::textEdited, this, [this, id](const QString& t) { send(msg::Queue{id, t.toStdString()}); });
        l->addWidget(input, 1);
        auto* remove = button(pixmap_label(kit::muted(Icon::X, theme::text_size(TextRole::Body)), "remove"), ButtonKind::row(false),
                              REMOVE_QUEUED_LABEL);
        remove->setToolTip(qs(REMOVE_QUEUED_LABEL));
        connect(remove, &QPushButton::clicked, this, [this, id] { send(msg::Queue{id, std::nullopt}); });
        l->addWidget(remove);
        rows->addWidget(row);
    }
    return box;
}

QWidget* ComposerView::build_popup(const State& state) {
    auto* box = new QWidget;
    box->setObjectName(QStringLiteral("composer popup"));
    box->setAttribute(Qt::WA_StyledBackground);
    kit::qt::style_container(*box, Surface::Popup);
    auto* l = vbox(box, SpaceRole::Xs);
    const auto xs = px(theme::sp(SpaceRole::Xs));
    l->setContentsMargins(xs, xs, xs, xs);
    const auto kind = *state.popup_kind();
    const auto title = kind == PopupKind::Command ? "Commands" : "Files";
    box->setAccessibleName(QString::fromLatin1(title));
    l->addWidget(kit::widgets::label(title, TextRole::Caption, Tone::Muted));
    const auto [items, selected] = state.popup_items();
    const auto details = state.popup_details();
    const auto first = selected >= VISIBLE_POPUP_ROWS ? selected - (VISIBLE_POPUP_ROWS - 1) : 0;
    for (auto n = first; n < items.size() && n < first + VISIBLE_POPUP_ROWS; ++n) {
        auto* content = new QWidget;
        auto* row = hbox(content, kind == PopupKind::Command ? SpaceRole::M : SpaceRole::S);
        if (kind == PopupKind::Command) {
            row->addWidget(kit::widgets::label("/" + items[n], TextRole::Body, Tone::Normal));
            row->addWidget(kit::widgets::label(details[n], TextRole::Caption, Tone::Muted));
        } else {
            row->addWidget(pixmap_label(kit::muted(Icon::FileText, theme::text_size(TextRole::Body)), "file"));
            row->addWidget(kit::widgets::label(items[n], TextRole::Body, Tone::Normal));
        }
        row->addStretch(1);
        auto* b = button(content, ButtonKind::row(n == selected), items[n]);
        connect(b, &QPushButton::clicked, this, [this, n] { send(msg::PopupAccept{n}); });
        l->addWidget(b);
    }
    return box;
}

QWidget* ComposerView::build_bar(const State& state) {
    auto* bar = new QWidget;
    bar->setObjectName(QStringLiteral("composer bar"));
    bar->setAccessibleName(QStringLiteral("composer bar"));
    auto* l = hbox(bar, SpaceRole::Xs);
    for (const auto& c : state.bar_chips()) {
        auto shown = c;
        if (c.picker == PickerId::Project) {
            // The project gets at most a third of the row so the folder keeps room.
            const auto chrome = 2.0F * theme::text_size(TextRole::Caption) + theme::sp(SpaceRole::S) + 3.0F * theme::sp(SpaceRole::Xs);
            const auto budget = std::max(theme::width(theme::LayoutRole::ComposerWidth) / 3.0F - chrome, 0.0F);
            shown.label = kit::text::ellipsize_text(c.label, budget, theme::text_size(TextRole::Caption), theme::tokens()->font(TextRole::Caption),
                                                    kit::text::EllipsisMode::End);
        }
        l->addWidget(chip(state, shown, true));
    }
    if (const auto* ws = state.workspace()) {
        auto* content = new QWidget;
        auto* row = hbox(content, SpaceRole::S);
        row->addWidget(pixmap_label(kit::tinted(Icon::Folder, TextRole::Body, Tone::Muted), "folder"));
        row->addWidget(kit::widgets::label(ws->path, TextRole::Label, Tone::Normal));
        if (ws->branch) {
            row->addWidget(pixmap_label(kit::tinted(Icon::GitFork, TextRole::Label, Tone::Faint), "branch"));
            row->addWidget(kit::widgets::label(*ws->branch, TextRole::Label, Tone::Faint));
        }
        const bool open = state.open_picker() == PickerId::Folder;
        auto* chip_ = toggle(state, PickerId::Folder, content, open, true, false);
        chip_->setObjectName(qs(WORKSPACE_CHIP_ID));
        // The full path on hover, except while the menu is open.
        if (!open) chip_->setToolTip(qs(ws->full));
        l->addWidget(chip_);
    }
    l->addStretch(1);
    return bar;
}

QWidget* ComposerView::build_working(const State& state) {
    auto* row = new QWidget;
    row->setObjectName(QStringLiteral("composer working"));
    auto* l = hbox(row, SpaceRole::S);
    const auto label = std::string(*state.indicator());
    const auto trailer = state.trailer();
    row->setAccessibleName(qs(trailer ? label + ", " + *trailer : label));
    const auto progress = shimmer::progress(state.shimmer());
    // The spinner turns `SPINS` times per shimmer cycle; it is decorative.
    const auto spinner = kit::tinted(Icon::LoaderCircle, TextRole::Label, Tone::Accent);
    float turn = progress * working::SPINS;
    turn -= std::floor(turn);
    auto* spin = pixmap_label(spinner.transformed(QTransform().rotate(turn * 360.0F), Qt::SmoothTransformation), "spinner");
    spin->setAccessibleName(QString());
    l->addWidget(spin);
    const auto t = theme::tokens();
    const auto accent = t->color(theme::ColorRole::Accent);
    const theme::Color colors[3] = {t->color(theme::ColorRole::Text2), accent.scale_alpha(t->alpha(theme::AlphaRole::Muted)), accent};
    QString html;
    for (const auto& run : working::runs(label, working::band_center(label, progress))) {
        html += QStringLiteral("<span style=\"color:%1\">%2</span>")
                    .arg(qs(kit::qt::css(colors[run.level])), qs(label.substr(run.begin, run.end - run.begin)).toHtmlEscaped());
    }
    auto* text = new QLabel(html);
    text->setObjectName(QStringLiteral("composer working label"));
    text->setTextFormat(Qt::RichText);
    auto font = kit::qt::font(TextRole::Label);
    font.setWeight(QFont::DemiBold);
    text->setFont(font);
    l->addWidget(text);
    if (trailer) {
        auto* tr = kit::widgets::label(*trailer, TextRole::Caption, Tone::Muted);
        tr->setObjectName(QStringLiteral("composer working trailer"));
        l->addWidget(tr);
    }
    l->addStretch(1);
    const auto m = px(theme::sp(SpaceRole::M));
    l->setContentsMargins(0, m, 0, 0);
    return row;
}

QWidget* ComposerView::build_images(const State& state) {
    auto* box = new QWidget;
    box->setObjectName(QStringLiteral("composer images"));
    box->setAccessibleName(QStringLiteral("Attached images"));
    auto* l = hbox(box, SpaceRole::S);
    const auto card_width = px(theme::text_size(TextRole::Body) * 8.0F);
    for (const auto& image : state.images()) {
        if (!image.shown) continue;
        const auto color = image_color(image.n);
        auto* card = new QWidget;
        card->setObjectName(QStringLiteral("image card"));
        card->setAccessibleName(QStringLiteral("Image #%1").arg(image.n));
        card->setAttribute(Qt::WA_StyledBackground);
        card->setStyleSheet(QStringLiteral("QWidget[objectName=\"image card\"] { border: 1px solid %1; border-radius: %2px; }")
                                .arg(qs(kit::qt::css(color)))
                                .arg(px(theme::tokens()->radius(theme::RadiusRole::Card))));
        card->setMaximumWidth(card_width);
        auto* col = vbox(card, SpaceRole::Xs);
        const auto s = px(theme::sp(SpaceRole::S));
        col->setContentsMargins(s, s, s, s);
        auto* header = new QWidget;
        auto* hl = hbox(header, SpaceRole::Xs);
        hl->addWidget(pixmap_label(kit::colored(Icon::Image, theme::text_size(TextRole::Body), color), "image"));
        auto* number = new QLabel(QStringLiteral("#%1").arg(image.n));
        auto bold = kit::qt::font(TextRole::Caption);
        bold.setBold(true);
        number->setFont(bold);
        number->setStyleSheet(QStringLiteral("color: %1").arg(qs(kit::qt::css(color))));
        hl->addWidget(number);
        hl->addStretch(1);
        auto* remove = button(pixmap_label(kit::muted(Icon::X, theme::text_size(TextRole::Caption)), "remove"), ButtonKind::ghost(),
                              "Remove image");
        const auto n = image.n;
        connect(remove, &QPushButton::clicked, this, [this, n] { send(msg::RemoveImage{n}); });
        hl->addWidget(remove);
        col->addWidget(header);
        auto* thumb = new QLabel;
        thumb->setAccessibleName(QStringLiteral("thumbnail"));
        thumb->setAlignment(Qt::AlignCenter);
        if (QPixmap pix(qs(image.image.path.string())); !pix.isNull())
            thumb->setPixmap(pix.scaled(card_width, px(theme::text_size(TextRole::Body) * 4.0F), Qt::KeepAspectRatio, Qt::SmoothTransformation));
        col->addWidget(thumb);
        auto* size = new QLabel(image.image.size ? QStringLiteral("%1x%2").arg(image.image.size->first).arg(image.image.size->second) : QString());
        size->setFont(kit::qt::font(TextRole::Caption));
        size->setStyleSheet(QStringLiteral("color: %1").arg(qs(kit::qt::css(color))));
        size->setAlignment(Qt::AlignCenter);
        col->addWidget(size);
        l->addWidget(card);
    }
    l->addStretch(1);
    return box;
}

QWidget* ComposerView::build_controls(const State& state) {
    auto* row = new QWidget;
    row->setObjectName(QStringLiteral("composer controls"));
    row->setAccessibleName(QStringLiteral("composer controls"));
    auto* l = hbox(row, SpaceRole::Xs);
    auto* mention = button(pixmap_label(kit::tinted(Icon::Plus, TextRole::Heading, Tone::Muted), "plus"), ButtonKind::ghost(), MENTION_LABEL);
    mention->setToolTip(qs(MENTION_LABEL));
    connect(mention, &QPushButton::clicked, this, [this] { send(msg::MentionPressed{}); });
    l->addWidget(mention);
    for (const auto& c : state.chips()) l->addWidget(chip(state, c, false));
    const bool rules_on = state.workflows_enabled();
    auto* rules = kit::widgets::text_button("Rules", ButtonKind::ghost(), rules_on ? kit::widgets::Callback([this] { send(msg::OpenRules{}); }) : nullptr);
    rules->setAccessibleName(QStringLiteral("Rules"));
    if (!rules_on) rules->setToolTip(qs(WORKFLOWS_UNAVAILABLE));
    l->addWidget(rules);
    l->addStretch(1);
    const bool busy = state.is_busy();
    const auto on = busy || state.can_send();
    const auto glyph = busy ? kit::tinted(Icon::Square, TextRole::Caption, on ? Tone::OnAccent : Tone::Faint)
                            : kit::tinted(Icon::ArrowUp, TextRole::Heading, on ? Tone::OnAccent : Tone::Faint);
    const auto label = busy ? STOP_LABEL : SEND_LABEL;
    auto* action = button(pixmap_label(glyph, busy ? "stop" : "send"), ButtonKind::accent(), label);
    action->setObjectName(busy ? QStringLiteral("composer stop") : QStringLiteral("composer send"));
    action->setToolTip(qs(label));
    action->setEnabled(on);
    const auto side = px(2.0F * theme::text_size(TextRole::Body));
    action->setMinimumSize(side, side);
    connect(action, &QPushButton::clicked, this, [this, busy] { send(busy ? Message{msg::Interrupt{}} : Message{msg::Send{}}); });
    l->addWidget(action);
    return row;
}

QWidget* ComposerView::chip(const State& state, const Chip& c, bool compact) {
    static constexpr Icon ICONS[] = {Icon::Bot, Icon::Sparkles, Icon::GitFork, Icon::Brain, Icon::Shield, Icon::FileText, Icon::Folder, Icon::Files};
    auto* content = new QWidget;
    auto* row = hbox(content, SpaceRole::S);
    const auto size = compact ? TextRole::Caption : TextRole::Body;
    row->addWidget(pixmap_label(kit::tinted(ICONS[static_cast<std::size_t>(c.picker)], size, Tone::Muted), "icon"));
    row->addWidget(kit::widgets::label(c.label, compact ? TextRole::Caption : TextRole::Label, Tone::Normal));
    auto* t = toggle(state, c.picker, content, c.open, c.enabled, compact);
    if (!c.enabled) t->setToolTip(qs(WORKFLOWS_UNAVAILABLE));
    return t;
}

QWidget* ComposerView::toggle(const State& state, PickerId picker, QWidget* content, bool open, bool enabled, bool compact) {
    auto* inner = new QWidget;
    auto* row = hbox(inner, compact ? SpaceRole::Xs : SpaceRole::S);
    row->addWidget(content);
    row->addWidget(pixmap_label(kit::tinted(open ? Icon::ChevronUp : Icon::ChevronDown, TextRole::Caption, Tone::Faint), "chevron"));
    auto* b = button(inner, ButtonKind::tab(open), "picker");
    b->setEnabled(enabled);
    connect(b, &QPushButton::clicked, this, [this, picker] { send(msg::Toggle{picker}); });
    auto* pop = new kit::widgets::Popover(b, open ? menu(state, picker) : nullptr);
    pop->own_width().on_dismiss([this] { send(msg::PickerClosed{}); });
    static constexpr const char* NAMES[] = {"harness", "model", "workflow", "effort", "permissions", "mode", "folder", "project"};
    const auto name = QStringLiteral("%1 picker").arg(QString::fromLatin1(NAMES[static_cast<std::size_t>(picker)]));
    pop->setObjectName(name);
    pop->setAccessibleName(name);
    b->setAccessibleName(name);
    return pop;
}

QWidget* ComposerView::menu(const State& state, PickerId picker) {
    if (picker == PickerId::Effort) return effort_card(state);
    auto* box = new QWidget;
    box->setObjectName(QStringLiteral("picker menu"));
    box->setAccessibleName(QStringLiteral("picker menu"));
    box->setAttribute(Qt::WA_StyledBackground);
    kit::qt::style_container(*box, Surface::Popup);
    auto* l = vbox(box, SpaceRole::Xs);
    const auto xs = px(theme::sp(SpaceRole::Xs));
    l->setContentsMargins(xs, xs, xs, xs);
    const auto rows = state.menu_rows(picker);
    const auto current = state.menu_current(picker);
    const bool two_line = picker == PickerId::Access;
    for (std::size_t i = 0; i < rows.size(); ++i) {
        const auto& r = rows[i];
        auto label = picker == PickerId::Mode ? capitalized(r.label) : r.label;
        QPushButton* b = nullptr;
        if (two_line && r.detail) {
            b = kit::widgets::two_line_row(label, *r.detail, current == i, nullptr);
        } else {
            auto* content = new QWidget;
            auto* row = hbox(content, SpaceRole::S);
            row->addWidget(kit::widgets::label(label, TextRole::Body, Tone::Normal));
            if (r.detail) row->addWidget(kit::widgets::label(*r.detail, TextRole::Caption, Tone::Muted));
            row->addStretch(1);
            if (current == i) row->addWidget(pixmap_label(kit::icon(Icon::Check, theme::text_size(TextRole::Body)), "current"));
            b = button(content, ButtonKind::row(false), label);
        }
        b->setEnabled(r.enabled);
        connect(b, &QPushButton::clicked, this, [this, picker, i] { send(msg::Pick{picker, i}); });
        l->addWidget(b);
    }
    const auto actions = state.menu_actions(picker);
    for (std::size_t n = 0; n < actions.size(); ++n) {
        const auto at = rows.size() + n;
        auto* b = kit::widgets::menu_row(std::nullopt, actions[n], false, [this, picker, at] { send(msg::Pick{picker, at}); });
        l->addWidget(b);
    }
    const auto role = picker == PickerId::Access || picker == PickerId::Folder ? theme::LayoutRole::ListWidth : theme::LayoutRole::NavWidth;
    const auto w = theme::width(role) / (picker == PickerId::Mode ? 1.5F : 1.0F);
    box->setMinimumWidth(px(w));
    return box;
}

QWidget* ComposerView::effort_card(const State& state) {
    auto* box = new QWidget;
    box->setObjectName(QStringLiteral("effort card"));
    box->setAccessibleName(QStringLiteral("Effort"));
    box->setAttribute(Qt::WA_StyledBackground);
    kit::qt::style_container(*box, Surface::Popup);
    auto* l = vbox(box, SpaceRole::M);
    const auto m = px(theme::sp(SpaceRole::M));
    l->setContentsMargins(m, m, m, m);
    auto* header = new QWidget;
    auto* hl = hbox(header, SpaceRole::Xs);
    hl->addWidget(pixmap_label(kit::tinted(Icon::Brain, TextRole::Body, Tone::Muted), "effort"));
    auto* heading = new QWidget;
    auto* col = vbox(heading, SpaceRole::Xs);
    auto* title = kit::widgets::label(state.effort_title(), TextRole::Heading, Tone::Accent);
    title->setAlignment(Qt::AlignCenter);
    col->addWidget(title);
    if (const auto model = state.effort_model()) {
        auto* name = kit::widgets::text_button(*model + " \u203a", ButtonKind::ghost(), [this] { send(msg::Toggle{PickerId::Model}); });
        name->setAccessibleName(QStringLiteral("Model"));
        col->addWidget(name, 0, Qt::AlignCenter);
    }
    hl->addWidget(heading, 1);
    l->addWidget(header);
    auto* slider = new QSlider(Qt::Horizontal);
    slider->setAccessibleName(QStringLiteral("Effort level"));
    const auto levels = state.effort_levels();
    slider->setRange(0, static_cast<int>(levels > 0 ? levels - 1 : 0));
    slider->setSingleStep(1);
    slider->setPageStep(1);
    slider->setTickPosition(QSlider::TicksBelow);
    slider->setTickInterval(1);
    slider->setValue(static_cast<int>(state.effort_shown().value_or(0)));
    connect(slider, &QSlider::valueChanged, this, [this](int v) { send(msg::EffortSlid{static_cast<std::uint8_t>(v)}); });
    connect(slider, &QSlider::sliderReleased, this, [this] { send(msg::EffortReleased{}); });
    l->addWidget(slider);
    box->setMinimumWidth(px(theme::width(theme::LayoutRole::NavWidth) * 1.2F));
    return box;
}

} // namespace loa::ui::session::composer
