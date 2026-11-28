#include "loa/ui/session/decisions/view.hpp"

#include <QCheckBox>
#include <QComboBox>
#include <QHBoxLayout>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QVBoxLayout>

#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/kit/widgets.hpp"

namespace loa::ui::session::decisions {

namespace kit = loa::ui::kit;
namespace w = loa::ui::kit::widgets;
using kit::ButtonKind;
using kit::Surface;
using kit::theme::ColorRole;
using kit::theme::SpaceRole;
using kit::theme::TextRole;

namespace {

// Fixed sizes from `crates/loa-ui-session/src/decisions`: `settings.rs` CONTROL_WIDTH and TIMEOUT_WIDTH, and the
// plan editor's `.height(320)` in `card.rs`.
constexpr int CONTROL_WIDTH = 240;
constexpr int TIMEOUT_WIDTH = 96;
constexpr int PLAN_EDITOR_HEIGHT = 320;

QString qs(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }
int px(SpaceRole r) { return static_cast<int>(kit::theme::sp(r)); }

QVBoxLayout* column(QWidget* parent, SpaceRole spacing = SpaceRole::M) {
    auto* l = new QVBoxLayout(parent);
    l->setContentsMargins(0, 0, 0, 0);
    l->setSpacing(px(spacing));
    return l;
}

QHBoxLayout* row(SpaceRole spacing = SpaceRole::S) {
    auto* l = new QHBoxLayout;
    l->setContentsMargins(0, 0, 0, 0);
    l->setSpacing(px(spacing));
    return l;
}

// Labels and buttons the kit made without an accessible name are named by their text.
void name_from_text(QWidget* root) {
    for (auto* l : root->findChildren<QLabel*>())
        if (l->accessibleName().isEmpty()) l->setAccessibleName(l->text());
    for (auto* b : root->findChildren<QPushButton*>())
        if (b->accessibleName().isEmpty()) b->setAccessibleName(b->text());
}

// Rebuilds `body` in place; the old one goes later, since a button of it may be emitting.
QWidget* replace(QVBoxLayout* layout, QWidget*& body) {
    if (body) {
        layout->removeWidget(body);
        body->hide();
        body->deleteLater();
    }
    body = new QWidget;
    body->setAccessibleName(QStringLiteral("decision body"));
    layout->addWidget(body);
    return body;
}

// The focused input's name and cursor, so a rebuild keeps typing where it was.
struct Focus {
    QString name;
    int cursor = -1;
};

Focus remember(QWidget* root) {
    Focus f;
    if (auto* fw = root ? root->focusWidget() : nullptr; fw && root->isAncestorOf(fw)) {
        f.name = fw->objectName();
        if (auto* line = qobject_cast<QLineEdit*>(fw)) f.cursor = line->cursorPosition();
        if (auto* edit = qobject_cast<QPlainTextEdit*>(fw)) f.cursor = edit->textCursor().position();
    }
    return f;
}

void restore(QWidget* root, const Focus& f) {
    if (f.name.isEmpty()) return;
    for (auto* child : root->findChildren<QWidget*>(f.name)) {
        if (!child->isVisibleTo(root) && child->parentWidget() && !child->parentWidget()->isVisibleTo(root)) continue;
        child->setFocus();
        if (auto* line = qobject_cast<QLineEdit*>(child); line && f.cursor >= 0) line->setCursorPosition(f.cursor);
        if (auto* edit = qobject_cast<QPlainTextEdit*>(child); edit && f.cursor >= 0) {
            auto c = edit->textCursor();
            c.setPosition(std::min(f.cursor, static_cast<int>(edit->toPlainText().size())));
            edit->setTextCursor(c);
        }
        return;
    }
}

class Builder {
  public:
    Builder(CardView* view, const Card& c, bool active) : view_(view), c_(c), id_(c.decision.id), active_(active) {}

    QPushButton* action(std::string_view label, std::optional<Message> on, bool primary, bool selected) const {
        const auto kind = primary ? ButtonKind::accent() : selected ? ButtonKind::tab(true) : ButtonKind::secondary();
        auto* b = w::text_button(label, kind, on ? w::Callback([v = view_, m = *on] { emit v->message(m); }) : w::Callback{});
        b->setEnabled(on.has_value());
        return b;
    }

    // A numbered option button.
    QPushButton* option(std::size_t i, std::string_view label, bool primary) const {
        const auto text = std::to_string(i + 1) + "  " + std::string(label);
        auto on = active_ ? std::optional<Message>(msg::Choose{id_, i}) : std::nullopt;
        auto* b = action(text, on, primary, c_.selected == i);
        b->setAccessibleName(qs(label));
        return b;
    }

    QLineEdit* text_field(std::string_view placeholder) const {
        auto* input = new QLineEdit(qs(c_.text));
        input->setObjectName(qs(text_field_id(id_)));
        input->setAccessibleName(qs(placeholder));
        input->setPlaceholderText(qs(placeholder));
        input->setFont(kit::qt::font(TextRole::Label));
        input->setReadOnly(!active_);
        QObject::connect(input, &QLineEdit::textEdited, input,
                         [v = view_, id = id_](const QString& t) { emit v->message(msg::Text{id, t.toStdString()}); });
        QObject::connect(input, &QLineEdit::returnPressed, input, [v = view_, id = id_, active = active_] {
            if (active) emit v->message(msg::SubmitText{id});
        });
        return input;
    }

    template <class C>
    QWidget* choices(std::span<const C> list, bool (*primary)(C)) const {
        auto* box = new QWidget;
        box->setAccessibleName(QStringLiteral("choices"));
        auto* r = row();
        box->setLayout(r);
        for (std::size_t i = 0; i < list.size(); ++i) r->addWidget(option(i, label_of(list[i]), primary(list[i])));
        r->addStretch();
        return box;
    }

    std::optional<Message> on(Message m) const { return active_ ? std::optional(std::move(m)) : std::nullopt; }

    void approval(QVBoxLayout* out) const {
        if (c_.diff) {
            auto* well = new QWidget;
            well->setObjectName(QStringLiteral("diffPreview"));
            well->setAccessibleName(QStringLiteral("diff preview"));
            kit::qt::style_container(*well, Surface::Well);
            auto* lines = column(well, SpaceRole::Xs);
            lines->setContentsMargins(px(SpaceRole::S), px(SpaceRole::Xs), px(SpaceRole::S), px(SpaceRole::Xs));
            const auto& t = *kit::theme::tokens();
            for (const auto& r : preview_rows(c_.diff->first, c_.show_full)) {
                const auto tone = r.path ? Tone::Muted
                                  : r.tag == panel::diff::Tag::Insert ? Tone::Ok
                                  : r.tag == panel::diff::Tag::Delete ? Tone::Danger
                                                                      : Tone::Normal;
                auto* l = w::label(r.text, TextRole::Mono, tone);
                l->setTextFormat(Qt::PlainText);
                if (!r.path && r.tag != panel::diff::Tag::Equal) {
                    const auto bg = t.color(r.tag == panel::diff::Tag::Insert ? ColorRole::AddedBg : ColorRole::RemovedBg);
                    l->setAutoFillBackground(true);
                    l->setStyleSheet(qs("background-color: " + kit::qt::css(bg) + ";"));
                }
                lines->addWidget(l);
            }
            out->addWidget(well);
            if (auto toggle = preview_toggle(c_)) out->addWidget(action(*toggle, msg::ToggleDiff{id_}, false, false));
        } else {
            out->addWidget(w::mono_block(c_.decision.body));
        }
        if (!c_.decision.is_waiting()) return;
        out->addWidget(choices<ApprovalChoice>(c_.decision.approval_choices(), [](ApprovalChoice k) { return k != ApprovalChoice::Deny; }));
        out->addWidget(text_field("Reason for denying (optional)"));
    }

    void question(QVBoxLayout* out) const {
        const auto& d = c_.decision;
        auto* body = w::label(d.body, TextRole::Body, Tone::Normal);
        body->setWordWrap(true);
        out->addWidget(body);
        if (!d.is_waiting()) return;
        auto* opts = new QWidget;
        opts->setAccessibleName(QStringLiteral("options"));
        auto* ol = column(opts, SpaceRole::S);
        for (std::size_t i = 0; i < d.options.size(); ++i) {
            if (d.multi) {
                auto* box = new QCheckBox(qs(std::to_string(i + 1) + " " + d.options[i]));
                box->setAccessibleName(qs(d.options[i]));
                box->setChecked(i < c_.picked.size() && c_.picked[i]);
                box->setEnabled(active_);
                QObject::connect(box, &QCheckBox::clicked, box, [v = view_, id = id_, i] { emit v->message(msg::Choose{id, i}); });
                ol->addWidget(box);
            } else {
                ol->addWidget(option(i, d.options[i], false));
            }
        }
        out->addWidget(opts);
        auto* answer = row();
        answer->addWidget(text_field("Type your own answer"), 1);
        answer->addWidget(action("Send", on(msg::SubmitText{id_}), true, false));
        if (d.multi) answer->addWidget(action("Submit", on(msg::Submit{id_}), true, false));
        answer->addWidget(action("Skip", on(msg::Skip{id_}), false, false));
        out->addLayout(answer);
    }

    void plan(QVBoxLayout* out) const {
        if (c_.editor) {
            auto* editor = new QPlainTextEdit(qs(*c_.editor));
            editor->setObjectName(qs("decision-" + loa::core::to_string(id_) + "-editor"));
            editor->setAccessibleName(QStringLiteral("plan editor"));
            editor->setFont(kit::qt::font(TextRole::Mono));
            editor->setFixedHeight(PLAN_EDITOR_HEIGHT);
            editor->setReadOnly(!active_);
            QObject::connect(editor, &QPlainTextEdit::textChanged, editor,
                             [v = view_, id = id_, editor] { emit v->message(msg::EditPlan{id, editor->toPlainText().toStdString()}); });
            out->addWidget(editor);
            auto* buttons = row();
            buttons->addWidget(action("Submit edited plan", on(msg::Submit{id_}), true, false));
            buttons->addWidget(action("Cancel", on(msg::CancelEdit{id_}), false, false));
            buttons->addStretch();
            out->addLayout(buttons);
            return;
        }
        auto* md = new QLabel;
        md->setAccessibleName(QStringLiteral("plan"));
        md->setTextFormat(Qt::MarkdownText);
        md->setText(qs(c_.decision.body));
        md->setWordWrap(true);
        md->setFont(kit::qt::font(TextRole::Body));
        md->setTextInteractionFlags(Qt::TextBrowserInteraction);
        md->setOpenExternalLinks(false);
        QObject::connect(md, &QLabel::linkActivated, md, [v = view_](const QString& url) { emit v->message(msg::Link{url.toStdString()}); });
        out->addWidget(md);
        if (!c_.decision.is_waiting()) return;
        out->addWidget(choices<PlanChoice>(Choice<PlanChoice>::ALL, [](PlanChoice k) { return k == PlanChoice::Approve; }));
        out->addWidget(text_field("Feedback for rejecting"));
    }

    void form(QVBoxLayout* out) const {
        const auto& d = c_.decision;
        if (!d.body.empty()) {
            auto* body = w::label(d.body, TextRole::Body, Tone::Normal);
            body->setWordWrap(true);
            out->addWidget(body);
        }
        if (!d.is_waiting()) return;
        auto* skip = action("Skip", on(msg::Skip{id_}), false, false);
        std::string problem;
        if (c_.form && *c_.form) {
            out->addWidget(fields(**c_.form));
            auto* buttons = row();
            buttons->addWidget(action("Submit", on(msg::Submit{id_}), true, false));
            buttons->addWidget(skip);
            buttons->addStretch();
            out->addLayout(buttons);
            return;
        }
        problem = c_.form ? "This form can't be shown: " + c_.form->error().message() : "This form declares no fields";
        auto* error = w::label(problem, TextRole::Label, Tone::Danger);
        error->setWordWrap(true);
        out->addWidget(error);
        out->addWidget(skip);
    }

    QWidget* fields(const Form& f) const {
        auto* box = new QWidget;
        box->setAccessibleName(QStringLiteral("form"));
        auto* l = column(box);
        for (std::size_t i = 0; i < f.fields.size() && i < f.values.size(); ++i) {
            const auto& field = f.fields[i];
            auto* cell = new QWidget;
            cell->setAccessibleName(qs(field.label));
            auto* cl = column(cell, SpaceRole::Xs);
            cl->addWidget(w::label(field.required ? field.label + " *" : field.label, TextRole::Label, Tone::Normal));
            if (field.description) cl->addWidget(w::label(*field.description, TextRole::Caption, Tone::Muted));
            const auto set = [v = view_, id = id_, i](FieldInput in) { emit v->message(msg::Field{id, i, std::move(in)}); };
            std::visit(
                [&](const auto& v) {
                    using T = std::decay_t<decltype(v)>;
                    if constexpr (std::is_same_v<T, form::value::Bool>) {
                        auto* b = new QCheckBox(qs(field.label));
                        b->setAccessibleName(qs(field.label));
                        b->setChecked(v.on);
                        b->setEnabled(active_);
                        QObject::connect(b, &QCheckBox::toggled, b, [set](bool on) { set(field_input::Bool{on}); });
                        cl->addWidget(b);
                    } else if constexpr (std::is_same_v<T, form::value::Choice>) {
                        auto* pick = new QComboBox;
                        pick->setAccessibleName(qs(field.label));
                        pick->setPlaceholderText(QStringLiteral("Choose…"));
                        for (const auto& label : field.ty.labels) pick->addItem(qs(label));
                        pick->setCurrentIndex(v.index ? static_cast<int>(*v.index) : -1);
                        pick->setEnabled(active_);
                        QObject::connect(pick, &QComboBox::activated, pick,
                                         [set](int n) { set(field_input::Choice{static_cast<std::size_t>(std::max(n, 0))}); });
                        cl->addWidget(pick);
                    } else if constexpr (std::is_same_v<T, form::value::Picks>) {
                        for (std::size_t n = 0; n < field.ty.labels.size() && n < v.picks.size(); ++n) {
                            auto* b = new QCheckBox(qs(field.ty.labels[n]));
                            b->setAccessibleName(qs(field.ty.labels[n]));
                            b->setChecked(v.picks[n]);
                            b->setEnabled(active_);
                            QObject::connect(b, &QCheckBox::clicked, b, [set, n] { set(field_input::Toggle{n}); });
                            cl->addWidget(b);
                        }
                    } else {
                        auto* input = new QLineEdit(qs(v.text));
                        input->setObjectName(qs(form_field_id(id_, i)));
                        input->setAccessibleName(qs(field.name));
                        input->setPlaceholderText(qs(field.name));
                        input->setReadOnly(!active_);
                        QObject::connect(input, &QLineEdit::textEdited, input,
                                         [set](const QString& t) { set(field_input::Text{t.toStdString()}); });
                        QObject::connect(input, &QLineEdit::returnPressed, input, [v = view_, id = id_, active = active_] {
                            if (active) emit v->message(msg::Submit{id});
                        });
                        cl->addWidget(input);
                    }
                },
                f.values[i]);
            if (i < f.errors.size() && f.errors[i]) cl->addWidget(w::label(form::message(*f.errors[i]), TextRole::Caption, Tone::Danger));
            l->addWidget(cell);
        }
        return box;
    }

  private:
    CardView* view_;
    const Card& c_;
    NodeId id_;
    bool active_;
};

} // namespace

KeyPress key_press(const QKeyEvent& event) {
    KeyPress k;
    if (event.key() == Qt::Key_Return || event.key() == Qt::Key_Enter) k.named = KeyPress::Named::Enter;
    else if (event.key() == Qt::Key_Escape) k.named = KeyPress::Named::Escape;
    else k.text = event.text().toStdString();
    const auto mods = event.modifiers();
    k.ctrl = mods.testFlag(Qt::ControlModifier);
    k.alt = mods.testFlag(Qt::AltModifier);
    k.logo = mods.testFlag(Qt::MetaModifier);
    return k;
}

CardView::CardView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("decision"));
    setAttribute(Qt::WA_StyledBackground, true);
    layout_ = new QVBoxLayout(this);
    layout_->setContentsMargins(px(SpaceRole::M), px(SpaceRole::M), px(SpaceRole::M), px(SpaceRole::M));
}

void CardView::render(const Card& card, const State& state, bool focused) {
    const Focus focus = remember(this);
    setObjectName(qs("decision-" + loa::core::to_string(card.decision.id)));
    kit::qt::style_container(*this, focused ? Surface::Attention : Surface::Card);
    auto* body = replace(layout_, body_);
    auto* out = column(body);
    const bool active = card.open() && !state.viewer().view_only;
    Builder b(this, card, active);

    const auto h = header(card);
    auto* head = row();
    auto* icon = new QLabel;
    icon->setAccessibleName(QStringLiteral("icon"));
    icon->setPixmap(kit::icon(h.icon, kit::theme::text_size(TextRole::Body)));
    head->addWidget(icon);
    head->addWidget(w::label(h.lead, TextRole::Body, Tone::Accent));
    if (h.tool) head->addWidget(w::label(*h.tool, TextRole::Body, Tone::Accent));
    if (h.title) head->addWidget(w::label(*h.title, TextRole::Label, Tone::Muted));
    head->addStretch();
    out->addLayout(head);

    auto* content = new QVBoxLayout;
    content->setSpacing(px(SpaceRole::M));
    switch (card.decision.kind) {
    case DecisionKind::Approval: b.approval(content); break;
    case DecisionKind::Question: b.question(content); break;
    case DecisionKind::Plan: b.plan(content); break;
    case DecisionKind::Form: b.form(content); break;
    }
    out->addLayout(content);

    if (const auto f = state.footer_of(card)) {
        out->addWidget(w::label(f->status, TextRole::Body, f->tone));
        if (f->json) {
            auto* json = w::label(*f->json, TextRole::Mono, Tone::Normal);
            json->setTextFormat(Qt::PlainText);
            out->addWidget(json);
        }
        if (f->reason) {
            auto* reason = w::label(*f->reason, f->reason_mono ? TextRole::Mono : TextRole::Label, Tone::Muted);
            reason->setWordWrap(true);
            reason->setTextFormat(Qt::PlainText);
            out->addWidget(reason);
        }
    }
    name_from_text(body);
    restore(this, focus);
}

DecisionsView::DecisionsView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("decisions"));
    layout_ = column(this);
}

void DecisionsView::render(const State& state) {
    // Keep card widgets by object name so typing survives a render.
    std::map<QString, CardView*> old;
    for (auto* v : findChildren<CardView*>(QString(), Qt::FindDirectChildrenOnly)) old[v->objectName()] = v;
    const auto focused = state.focused();
    std::vector<CardView*> order;
    for (const auto& [id, card] : state.cards()) {
        const auto name = qs("decision-" + loa::core::to_string(id));
        CardView* v = nullptr;
        if (auto it = old.find(name); it != old.end()) {
            v = it->second;
            old.erase(it);
        } else {
            v = new CardView(this);
            connect(v, &CardView::message, this, &DecisionsView::message);
        }
        v->render(card, state, focused == id);
        order.push_back(v);
    }
    for (auto& [_, v] : old) {
        layout_->removeWidget(v);
        v->hide();
        v->deleteLater();
    }
    for (std::size_t i = 0; i < order.size(); ++i) {
        layout_->removeWidget(order[i]);
        layout_->insertWidget(static_cast<int>(i), order[i]);
    }
}

UnattendedSettingsView::UnattendedSettingsView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("unattended settings"));
    layout_ = column(this);
}

void UnattendedSettingsView::render(const Unattended& policy) {
    auto* body = new QWidget;
    if (body_) {
        layout_->removeWidget(body_);
        body_->hide();
        body_->deleteLater();
    }
    body_ = body;
    body->setAccessibleName(QStringLiteral("unattended"));
    layout_->addWidget(body);
    auto* out = column(body);

    auto* toggle = row(SpaceRole::M);
    auto* label = w::label(SETTINGS_TOGGLE, TextRole::Label, Tone::Normal);
    toggle->addWidget(label, 1);
    auto* on = new QCheckBox;
    on->setAccessibleName(qs(SETTINGS_TOGGLE));
    on->setChecked(policy.enabled());
    connect(on, &QCheckBox::toggled, this, [this](bool v) { emit message(unattended::Enabled{v}); });
    toggle->addWidget(on);
    out->addLayout(toggle);
    auto* note = w::label(SETTINGS_NOTE, TextRole::Caption, Tone::Muted);
    note->setWordWrap(true);
    out->addWidget(note);

    for (const auto& r : settings_rows(policy)) {
        auto* line = row(SpaceRole::M);
        line->addWidget(w::label(r.label, TextRole::Label, Tone::Normal), 1);
        const auto kind = r.kind;
        if (r.choices.empty()) {
            auto* input = new QLineEdit(qs(r.typed));
            input->setAccessibleName(qs(r.label));
            input->setPlaceholderText(qs(SETTINGS_TYPED_PLACEHOLDER));
            input->setFixedWidth(CONTROL_WIDTH);
            connect(input, &QLineEdit::textEdited, this, [this, kind](const QString& t) {
                const auto typed = t.toStdString();
                const bool blank = t.trimmed().isEmpty();
                emit message(unattended::Answer{kind, blank ? std::nullopt : std::optional(typed)});
            });
            line->addWidget(input);
        } else {
            auto* pick = new QComboBox;
            pick->setAccessibleName(qs(r.label));
            pick->setFixedWidth(CONTROL_WIDTH);
            auto choices = r.choices;
            if (r.current && std::ranges::find(choices, *r.current) == choices.end()) choices.push_back(*r.current);
            int current = -1;
            for (std::size_t i = 0; i < choices.size(); ++i) {
                pick->addItem(qs(choices[i].label));
                if (r.current && choices[i] == *r.current) current = static_cast<int>(i);
            }
            pick->setCurrentIndex(current);
            connect(pick, &QComboBox::activated, this, [this, kind, choices](int i) {
                if (i >= 0 && static_cast<std::size_t>(i) < choices.size())
                    emit message(unattended::Answer{kind, choices[static_cast<std::size_t>(i)].answer});
            });
            line->addWidget(pick);
        }
        if (r.timeout) {
            auto* pick = new QComboBox;
            pick->setAccessibleName(qs(std::string(r.label) + " timeout"));
            pick->setFixedWidth(TIMEOUT_WIDTH);
            for (const auto& t : r.timeouts) pick->addItem(qs(t.to_string()));
            pick->setCurrentIndex(static_cast<int>(std::ranges::find(r.timeouts, *r.timeout) - r.timeouts.begin()));
            const auto timeouts = r.timeouts;
            connect(pick, &QComboBox::activated, this, [this, kind, timeouts](int i) {
                if (i >= 0 && static_cast<std::size_t>(i) < timeouts.size())
                    emit message(unattended::Timeout{kind, timeouts[static_cast<std::size_t>(i)]});
            });
            line->addWidget(pick);
        } else {
            line->addSpacing(96);
        }
        out->addLayout(line);
    }
    name_from_text(body);
}

} // namespace loa::ui::session::decisions
