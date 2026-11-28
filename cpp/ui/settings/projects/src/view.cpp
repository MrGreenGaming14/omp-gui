// Qt views of the project screen and the New project screen. Port of `State::view` in
// `crates/loa-ui-settings/src/projects/{mod,create}.rs`.

#include "loa/ui/settings/projects/view.hpp"

#include <format>

#include <QCheckBox>
#include <QLabel>
#include <QLineEdit>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QScrollArea>
#include <QVBoxLayout>

#include "loa/runtime/time.hpp"
#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/qt.hpp"
#include "settings_widgets.hpp"

namespace loa::ui::settings::projects {

using namespace sw;
using loa::view::theme::Icon;

namespace {


QWidget* title_row(std::string_view title) {
    auto* icon = new QLabel;
    icon->setPixmap(kit::icon(Icon::Folder, theme::text_size(TextRole::Title)));
    return hrow({icon, kw::label(title, TextRole::Title, Tone::Normal)});
}

int editor_height() { return px(theme::sp(SpaceRole::Xl) * 6.0F); }

// A whole-row button around `body` with trailing `extra`, as iced's row buttons with their own controls.
QWidget* row_button(QWidget* body, QWidget* extra, Callback on) {
    auto* b = new QPushButton;
    b->setAccessibleName(QStringLiteral("row"));
    b->setObjectName(QStringLiteral("settings-row"));
    auto* l = new QHBoxLayout(b);
    l->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::S), spi(SpaceRole::M), spi(SpaceRole::S));
    l->setSpacing(spi(SpaceRole::M));
    l->addWidget(body, 1);
    if (extra) l->addWidget(extra);
    b->setMinimumHeight(body->sizeHint().height() + 2 * spi(SpaceRole::S));
    kit::qt::style_button(*b, ButtonKind::row(false));
    if (on) QObject::connect(b, &QPushButton::clicked, b, [on = std::move(on)] { on(); });
    else b->setEnabled(false);
    return b;
}

QWidget* card_of(QWidget* content) {
    auto* c = kw::card(content);
    return c;
}

} // namespace

ProjectView::ProjectView(QWidget* parent) : QWidget(parent) {
    auto* l = new QVBoxLayout(this);
    l->setContentsMargins(0, 0, 0, 0);
    setAccessibleName(QStringLiteral("project"));
}

void ProjectView::render(const State& state) {
    namespace l = local;
    namespace in = vs::intent;
    auto send = [this](Message m) { return [this, m] { emit message(m); }; };
    auto loc = [send](Local m) { return send(Message{std::move(m)}); };
    auto emit_intent = [send](Intent i) { return send(Message{std::move(i)}); };

    const auto* o = state.overview();
    const std::optional<std::size_t> sessions = o ? std::optional(o->sessions.size()) : std::nullopt;

    auto delete_view = [&] {
        std::string kept;
        if (!sessions || *sessions == 0) kept = "Its instructions, ideas and pins are removed.";
        else if (*sessions == 1) kept = "Its instructions, ideas and pins are removed. Its session is kept, in no project.";
        else kept = std::format("Its instructions, ideas and pins are removed. Its {} sessions are kept, in no project.", *sessions);
        QWidget* actions = nullptr;
        if (state.confirm_delete()) {
            actions = hrow({kw::label(std::format("Delete {}?", state.project()), TextRole::Caption, Tone::Danger)});
            stretch(actions);
            add(actions, action("Cancel", ButtonKind::ghost(), loc(l::CancelDelete{})));
            add(actions, action("Delete", ButtonKind::danger(), loc(l::ConfirmDelete{})));
        } else {
            actions = hrow({});
            stretch(actions);
            add(actions, action("Delete project", ButtonKind::secondary(), loc(l::Delete{})));
        }
        return section("Delete project", vcol({kw::label(kept, TextRole::Label, Tone::Muted), actions}, spi(SpaceRole::S)));
    };

    // Header: title, folder and session count, New chat.
    QWidget* header = nullptr;
    {
        const std::optional<std::string> cwd = o ? o->cwd : std::nullopt;
        auto* left = vcol({title_row(state.project())});
        if (o) {
            const auto n = o->sessions.size();
            const auto count = n == 1 ? std::string("1 session") : std::format("{} sessions", n);
            auto* details = hrow({kw::label(std::format("{}  \u00b7  {}", cwd.value_or("No folder"), count), TextRole::Caption, Tone::Muted),
                                  action(cwd ? "Change folder\u2026" : "Set folder\u2026", ButtonKind::ghost(),
                                         emit_intent(in::ChangeFolder{state.project(), cwd}))});
            add(left, details);
        }
        auto* new_chat = kw::text_button("New chat", ButtonKind::secondary(), emit_intent(in::NewChat{state.project(), cwd}));
        new_chat->setIcon(kit::qicon(Icon::Plus, TextRole::Body, Tone::Normal));
        header = hrow({left});
        stretch(header);
        add(header, new_chat);
    }

    auto* col = vcol({header}, spi(SpaceRole::Xl));
    if (state.confirm_delete()) add(col, card_of(delete_view()));

    if (const auto* history = state.omp_history()) {
        QWidget* content = nullptr;
        if (std::holds_alternative<vs::omp_history::Loading>(*history))
            content = kw::label("Loading terminal history\u2026", TextRole::Label, Tone::Muted);
        else if (const auto* f = std::get_if<vs::omp_history::Failed>(history))
            content = kw::label(f->message, TextRole::Label, Tone::Danger);
        else if (const auto& rows = std::get<vs::omp_history::Ready>(*history).rows; rows.empty())
            content = kw::label("No saved terminal conversations.", TextRole::Label, Tone::Muted);
        else {
            const bool busy = state.omp_history_busy();
            auto* list = vcol({}, 0);
            for (const auto& r : rows) {
                auto* body = vcol({kw::label(r.title, TextRole::Body, Tone::Normal),
                                   kw::label(loa::runtime::time::ago(state.now(), r.modified), TextRole::Caption, Tone::Muted)});
                if (state.confirm_delete_history() == r.id) {
                    auto* actions = hrow({kw::label("Delete this terminal conversation?", TextRole::Caption, Tone::Danger)});
                    stretch(actions);
                    add(actions, action("Cancel", ButtonKind::ghost(), loc(l::CancelDeleteOmpHistory{})));
                    add(actions, action("Delete", ButtonKind::danger(), busy ? Callback{} : loc(l::ConfirmDeleteOmpHistory{})));
                    add(list, vcol({body, actions}, spi(SpaceRole::S)));
                    continue;
                }
                auto* del = action("Delete", ButtonKind::ghost(), busy ? Callback{} : loc(l::DeleteOmpHistory{r.id}));
                auto* row = row_button(body, del, busy ? Callback{} : emit_intent(in::OpenOmpHistory{r.id}));
                row->setAccessibleName(qs(r.title));
                add(list, row);
            }
            content = card_of(list);
        }
        add(col, section("OMP terminal history",
                         vcol({kw::label("Open a terminal conversation to import it into this project.", TextRole::Label, Tone::Muted), content},
                              spi(SpaceRole::S))));
    }

    if (!o) {
        add(col, kw::label("Loading project\u2026", TextRole::Label, Tone::Muted));
    } else {
        const int gap = spi(SpaceRole::S);
        auto hint = [](std::string_view s) { return kw::label(s, TextRole::Label, Tone::Muted); };
        // Settings.
        auto* instructions = vcol({hint("Given to every chat in this project, like a CLAUDE.md for the whole project."),
                                   editor(state.instructions(), "Conventions, constraints, where things live\u2026",
                                          [this](std::string t) { emit message(Message{Local{l::Instructions{std::move(t)}}}); },
                                          "project-instructions", editor_height())},
                                  gap);
        auto* ideas = vcol({hint("Your own notes and ideas for this project. Chats see them only while shared.")}, gap);
        if (!o->ideas) add(ideas, kw::label(o->ideas.error(), TextRole::Caption, Tone::Danger));
        if (state.ideas_changed()) {
            auto* changed = hrow({kw::label("Changed outside Loa \u2014 reload before saving", TextRole::Caption, Tone::Danger)});
            stretch(changed);
            add(changed, action("Reload", ButtonKind::secondary(), loc(l::ReloadIdeas{})));
            add(ideas, changed);
        }
        if (state.ideas_reloading()) add(ideas, kw::label("Reloading the notes\u2026", TextRole::Caption, Tone::Muted));
        std::function<void(std::string)> on_ideas;
        if (state.ideas_editable()) on_ideas = [this](std::string t) { emit message(Message{Local{l::Ideas{std::move(t)}}}); };
        add(ideas, editor(state.ideas(), "Features to try, open questions, plans\u2026", std::move(on_ideas), "project-ideas", editor_height()));
        add(ideas, check(state.ideas_in_context(), "Use ideas as agent context",
                         [this](bool on) { emit message(Message{Local{l::ShareIdeas{on}}}); }));

        std::vector<std::string> pin_labels;
        for (const auto& p : state.pinned()) pin_labels.push_back(p.note ? std::format("{}  \u00b7  {}", p.path, *p.note) : p.path);
        auto* pins = vcol({}, gap);
        if (state.pinned().empty()) add(pins, hint("No pinned files. Type the path of a file every chat should know about."));
        add(pins, kw::list_editor(
                      pin_labels, [this](std::size_t i) { emit message(Message{Local{l::RemovePin{i}}}); }, {}, state.new_pin_path(),
                      [this](std::string p) { emit message(Message{Local{l::PinPath{std::move(p)}}}); },
                      [this] { emit message(Message{Local{l::AddPin{}}}); }));
        auto* note = field("Why it matters (optional)", state.new_pin_note(),
                           [this](std::string n) { emit message(Message{Local{l::PinNote{std::move(n)}}}); }, "project-pin-note");
        QObject::connect(note, &QLineEdit::returnPressed, note, [this] { emit message(Message{Local{l::AddPin{}}}); });
        add(pins, note);

        auto* model = field("@smol", state.digest_model(), [this](std::string m) { emit message(Message{Local{l::DigestModel{std::move(m)}}}); },
                            "project-digest-model");
        model->setMaximumWidth(px(theme::width(LayoutRole::LabelColumn) * 2.0F));
        auto* digest = vcol({hint("Model or role that writes session digests. Empty uses omp's smol role."), model}, gap);

        const bool dirty = state.is_dirty();
        auto* actions = hrow({});
        if (state.error()) add(actions, kw::label(*state.error(), TextRole::Caption, Tone::Danger));
        else if (!dirty && !state.saving()) add(actions, kw::label("All changes saved", TextRole::Caption, Tone::Muted));
        stretch(actions);
        if (dirty) add(actions, action("Revert", ButtonKind::ghost(), state.saving() ? Callback{} : loc(l::Revert{})));
        add(actions, action(state.saving() ? "Saving\u2026" : "Save", ButtonKind::primary(),
                            dirty && !state.saving() && !state.ideas_reloading() ? loc(l::Save{}) : Callback{}));

        add(col, vcol({section("Shared instructions", instructions), section("Ideas", ideas), section("Pinned files", pins),
                       section("Digest model", digest), actions},
                      spi(SpaceRole::L)));

        // Sessions.
        QWidget* rows = nullptr;
        if (o->sessions.empty()) {
            rows = kw::label("No sessions in this project yet.", TextRole::Label, Tone::Muted);
        } else {
            auto* list = vcol({}, 0);
            for (const auto& s : o->sessions) {
                const bool titled = s.title && s.title->find_first_not_of(" \t\r\n") != std::string::npos;
                const std::string title = titled ? *s.title : std::string("Untitled session");
                std::string meta;
                if (s.harness) meta = *s.harness + "  \u00b7  ";
                meta += s.turns == 1 ? std::string("1 turn") : std::format("{} turns", s.turns);
                meta += "  \u00b7  " + loa::runtime::time::ago(state.now(), s.last_active);
                std::string digest_state;
                Tone tone = Tone::Muted;
                if (s.excluded) digest_state = "not in context";
                else if (s.digest_state == DigestState::Fresh) digest_state = s.digest_model ? "digest by " + *s.digest_model : "digest up to date";
                else if (s.digest_state == DigestState::Stale) digest_state = "digest out of date", tone = Tone::Warn;
                else digest_state = "no digest yet";
                auto* meta_row = hrow({kw::label(meta, TextRole::Caption, Tone::Muted), kw::label("  \u00b7  ", TextRole::Caption, Tone::Muted),
                                       kw::label(digest_state, TextRole::Caption, tone)},
                                      0);
                stretch(meta_row);
                auto* body = vcol({kw::label(title, TextRole::Body, Tone::Normal), meta_row});
                if (s.digest) {
                    add(body, kw::label(s.digest->summary, TextRole::Label, Tone::Muted));
                    for (std::size_t i = 0; i < s.digest->decisions.size() && i < SHOWN_DECISIONS; ++i)
                        add(body, kw::label("\u2022 " + s.digest->decisions[i], TextRole::Caption, Tone::Muted));
                }
                auto* include = check(!s.excluded, "In context", [this, id = s.id](bool included) {
                    emit message(Message{Local{l::SetIncluded{id, included}}});
                });
                auto* row = row_button(body, include, emit_intent(in::OpenSession{s.id}));
                row->setAccessibleName(qs(meta));
                add(list, row);
            }
            rows = card_of(list);
        }
        add(col, section("Sessions", vcol({hint("Each session's digest is shared with new chats in this project. Turn a session off to keep it out."),
                                           rows},
                                          gap)));
        if (!state.confirm_delete()) add(col, delete_view());
    }
    content_ = swap_content(*this, content_, sw::scroll(col, true));
}

CreateView::CreateView(QWidget* parent) : QWidget(parent) {
    auto* l = new QVBoxLayout(this);
    l->setContentsMargins(0, 0, 0, 0);
    setAccessibleName(QStringLiteral("new project"));
}

void CreateView::render(const create::State& state) {
    namespace l = create::local;
    namespace ci = vs::create_intent;
    using create::Local;
    using create::Message;
    auto send = [this](Message m) { return [this, m] { emit message(m); }; };
    const bool can = state.can_create();
    const int gap = spi(SpaceRole::S);
    auto hint = [](std::string_view s) { return kw::label(s, TextRole::Label, Tone::Muted); };

    auto* name = field("Launch plan", state.name(), [this](std::string n) { emit message(Message{Local{l::Name{std::move(n)}}}); },
                       create::NAME_ID);
    if (can) QObject::connect(name, &QLineEdit::returnPressed, name, [this] { emit message(Message{Local{l::Create{}}}); });
    auto* folder_row = hrow({field("/path/to/folder", state.folder(), [this](std::string f) { emit message(Message{Local{l::Folder{std::move(f)}}}); },
                                   "loa-new-project-folder"),
                             action("Choose\u2026", ButtonKind::secondary(), send(Message{Local{l::ChooseFolder{}}}))});
    auto* folder = vcol({hint("New chats in this project start here. Its .omgui folder links to every folder the project's chats run in."), folder_row},
                        gap);
    auto* instructions = vcol({hint("Optional. Given to every chat in this project, like a CLAUDE.md for the whole project."),
                               editor(state.instructions(), "Conventions, constraints, where things live\u2026",
                                      [this](std::string t) { emit message(Message{Local{l::Instructions{std::move(t)}}}); },
                                      "loa-new-project-instructions", editor_height())},
                              gap);
    auto* actions = hrow({});
    if (auto e = state.error()) add(actions, kw::label(*e, TextRole::Caption, Tone::Danger));
    stretch(actions);
    add(actions, action("Cancel", ButtonKind::ghost(), send(Message{CreateIntent{ci::Cancel{}}})));
    add(actions, action(state.creating() ? "Creating\u2026" : "Create project", ButtonKind::primary(),
                        can ? send(Message{Local{l::Create{}}}) : Callback{}));
    auto* content = vcol({title_row("New project"), section("Name", name), section("Folder", folder),
                          section("Shared instructions", instructions), actions},
                         spi(SpaceRole::L));
    content_ = swap_content(*this, content_, sw::scroll(content, true));
    // The name input takes focus when the screen opens.
    if (!focused_) {
        focused_ = true;
        if (auto* input = findChild<QLineEdit*>(qs(create::NAME_ID))) input->setFocus();
    }
}

} // namespace loa::ui::settings::projects
