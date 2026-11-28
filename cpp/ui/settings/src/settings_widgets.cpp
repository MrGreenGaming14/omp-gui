// Shared settings widgets. Port of the helpers in `crates/loa-ui-settings/src/lib.rs`.

#include "settings_widgets.hpp"

#include <QApplication>
#include <QCheckBox>
#include <QFrame>
#include <QLabel>
#include <QLineEdit>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QScrollArea>
#include <QTextCursor>

#include "loa/ui/kit/qt.hpp"

namespace loa::ui::settings::sw {

QWidget* box(QBoxLayout::Direction dir, std::initializer_list<QWidget*> items, int spacing) {
    auto* w = new QWidget;
    auto* l = new QBoxLayout(dir, w);
    l->setContentsMargins(0, 0, 0, 0);
    l->setSpacing(spacing);
    for (auto* item : items)
        if (item) l->addWidget(item);
    return w;
}

void add(QWidget* parent, QWidget* w) {
    if (w && parent->layout()) parent->layout()->addWidget(w);
}

void stretch(QWidget* parent) {
    if (auto* l = qobject_cast<QBoxLayout*>(parent->layout())) l->addStretch(1);
}

QPushButton* action(std::string_view s, ButtonKind kind, Callback on) { return kw::text_button(s, kind, std::move(on)); }

QLineEdit* field(std::string_view placeholder, std::string_view value, std::function<void(std::string)> on_input,
                 std::string_view name) {
    auto* input = new QLineEdit(qs(value));
    input->setPlaceholderText(qs(placeholder));
    input->setFont(kit::qt::font(TextRole::Body));
    input->setAccessibleName(qs(name.empty() ? placeholder : name));
    if (!name.empty()) input->setObjectName(qs(name));
    if (on_input)
        QObject::connect(input, &QLineEdit::textEdited, input, [on = std::move(on_input)](const QString& v) { on(v.toStdString()); });
    else
        input->setReadOnly(true);
    return input;
}

QCheckBox* check(bool on, std::string_view s, std::function<void(bool)> toggle) {
    auto* box = new QCheckBox(qs(s));
    box->setChecked(on);
    box->setFont(kit::qt::font(TextRole::Label));
    box->setAccessibleName(qs(s));
    if (toggle)
        QObject::connect(box, &QCheckBox::clicked, box, [t = std::move(toggle)](bool checked) { t(checked); });
    else
        box->setEnabled(false);
    return box;
}

QPlainTextEdit* editor(std::string_view text, std::string_view placeholder, std::function<void(std::string)> on_edit,
                       std::string_view name, int height) {
    auto* e = new QPlainTextEdit;
    e->setPlainText(qs(text));
    e->setPlaceholderText(qs(placeholder));
    e->setFont(kit::qt::font(TextRole::Body));
    e->setObjectName(qs(name));
    e->setAccessibleName(qs(name));
    e->setFixedHeight(height);
    if (on_edit)
        QObject::connect(e, &QPlainTextEdit::textChanged, e, [e, on = std::move(on_edit)] { on(e->toPlainText().toStdString()); });
    else
        e->setReadOnly(true);
    return e;
}

QWidget* section(std::string_view title, QWidget* body) {
    auto* w = vcol({kw::section_header(title, {}), body}, spi(SpaceRole::S));
    w->setAccessibleName(qs(title));
    return w;
}

QWidget* errors(const std::vector<std::string>& lines) {
    auto* w = new QFrame;
    w->setObjectName(QStringLiteral("settings-errors"));
    w->setAccessibleName(QStringLiteral("errors"));
    auto* l = new QVBoxLayout(w);
    l->setContentsMargins(spi(SpaceRole::S), spi(SpaceRole::S), spi(SpaceRole::S), spi(SpaceRole::S));
    l->setSpacing(spi(SpaceRole::Xs));
    for (const auto& line : lines) l->addWidget(kw::label(line, TextRole::Label, Tone::Danger));
    kit::qt::style_container(*w, Surface::ErrorBox);
    return w;
}

QScrollArea* scroll(QWidget* content, bool center) {
    auto* area = new QScrollArea;
    area->setWidgetResizable(true);
    area->setFrameShape(QFrame::NoFrame);
    auto* inner = new QWidget;
    auto* l = new QHBoxLayout(inner);
    const int m = spi(SpaceRole::M);
    l->setContentsMargins(m, m, m, m);
    if (center) {
        content->setMaximumWidth(px(theme::width(LayoutRole::ContentWidth)));
        l->addStretch(1);
        l->addWidget(content, 100);
        l->addStretch(1);
    } else {
        l->addWidget(content);
    }
    area->setWidget(inner);
    return area;
}

void FocusKeeper::capture(const QWidget& root) {
    name_.clear();
    cursor_ = -1;
    auto* focus = QApplication::focusWidget();
    if (!focus || !root.isAncestorOf(focus) || focus->objectName().isEmpty()) return;
    name_ = focus->objectName();
    if (auto* line = qobject_cast<QLineEdit*>(focus)) cursor_ = line->cursorPosition();
    else if (auto* text = qobject_cast<QPlainTextEdit*>(focus)) cursor_ = text->textCursor().position();
}

void FocusKeeper::restore(QWidget& root) const {
    if (name_.isEmpty()) return;
    auto* w = root.findChild<QWidget*>(name_);
    if (!w) return;
    w->setFocus();
    if (cursor_ < 0) return;
    if (auto* line = qobject_cast<QLineEdit*>(w)) line->setCursorPosition(cursor_);
    else if (auto* text = qobject_cast<QPlainTextEdit*>(w)) {
        auto c = text->textCursor();
        c.setPosition(std::min(cursor_, static_cast<int>(text->document()->characterCount()) - 1));
        text->setTextCursor(c);
    }
}

QWidget* swap_content(QWidget& host, QWidget* old, QWidget* fresh) {
    FocusKeeper keep;
    if (old) keep.capture(*old);
    host.layout()->addWidget(fresh);
    if (old) {
        host.layout()->removeWidget(old);
        old->hide();
        old->deleteLater();
    }
    keep.restore(*fresh);
    return fresh;
}

} // namespace loa::ui::settings::sw
