// The peers area's widgets: the Peers panel tab, a peer's session and a peer's shared folder in
// the center (the `view` functions of `areas/peers/{mod,remote,folder}.rs`).

// `loa/p2p/node.hpp` (through `peers.hpp`) declares a member named `emit`: before any Qt header.
#include "areas/peers/peers.hpp"

#include <QHBoxLayout>
#include <QLabel>
#include <QLineEdit>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QScrollArea>
#include <QVBoxLayout>
#include <QWidget>

#include "loa/core/schema.hpp"
#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/widgets.hpp"
#include "loa/ui/session/decisions/view.hpp"
#include "loa/ui/session/transcript/view.hpp"
#include "loa/ui/settings/peers/view.hpp"

namespace loa::app::areas::peers {

namespace {

namespace kit = loa::ui::kit;
namespace theme = loa::ui::kit::theme;
namespace widgets = loa::ui::kit::widgets;
using loa::view::theme::Icon;
using theme::SpaceRole;
using theme::TextRole;

int px(float v) { return static_cast<int>(v + 0.5F); }
int gap(SpaceRole role) { return px(theme::sp(role)); }

QVBoxLayout* column(QWidget* w, SpaceRole role) {
    auto* layout = new QVBoxLayout(w);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(gap(role));
    return layout;
}

QHBoxLayout* row(SpaceRole role) {
    auto* layout = new QHBoxLayout;
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(gap(role));
    return layout;
}

QLabel* glyph(Icon icon, TextRole size, kit::Tone tone) {
    auto* label = new QLabel;
    label->setPixmap(kit::tinted(icon, size, tone));
    return label;
}

// The screen title over a caption, then `actions`.
QHBoxLayout* header(std::string_view title, std::string_view caption, const std::vector<QWidget*>& actions) {
    auto* titles = new QWidget;
    auto* stack = column(titles, SpaceRole::Xs);
    stack->addWidget(widgets::label(title, TextRole::Title, kit::Tone::Normal));
    stack->addWidget(widgets::label(caption, TextRole::Caption, kit::Tone::Muted));
    auto* bar = row(SpaceRole::M);
    bar->addWidget(titles, 1);
    for (auto* action : actions) bar->addWidget(action);
    return bar;
}

} // namespace

QWidget* State::view_tab(std::string_view id, const Ctx&, const Emit& send) const {
    if (id != TAB) return nullptr;
    auto* view = new screen::PeersView;
    view->setAccessibleName(QStringLiteral("peers"));
    view->render(panel_);
    QObject::connect(view, &screen::PeersView::message, view, [send](const screen::Message& m) { send(message(msg::Panel{m})); });
    return view;
}

QWidget* State::view_screen(const Ctx& ctx, const Emit& send) const {
    if (folder_) return folder_view(*folder_, send);
    if (remote_) return remote_view(*remote_, ctx, send);
    return nullptr;
}

QWidget* State::remote_view(const Remote& remote, const Ctx&, const Emit& send) const {
    auto out = [send](RemoteMessage m) { send(message(msg::Remote{std::move(m)})); };
    auto* root = new QWidget;
    root->setAccessibleName(QStringLiteral("peer session"));
    auto* layout = column(root, SpaceRole::S);

    std::string access;
    for (auto action : remote.view->actions()) access += (access.empty() ? "" : ", ") + std::string(trust::as_str(action));
    std::string harness = "harness";
    if (const auto* root_node = remote.dom.root())
        if (auto h = root_node->str_attr(core::attr::HARNESS)) harness = std::string(*h);
    const auto subtitle = remote.peer_name + " · " + harness + " · " + (access.empty() ? "some prompts" : access);
    std::vector<QWidget*> actions;
    if (remote.view->can(trust::Action::fork))
        actions.push_back(widgets::text_button("Fork to this node", kit::ButtonKind::secondary(), [out] { out(remote_msg::Fork{}); }));
    actions.push_back(widgets::icon_button(Icon::X, "Close", [out] { out(remote_msg::Close{}); }));
    auto* top = new QWidget;
    auto* top_layout = header(remote.title(), subtitle, actions);
    top_layout->setContentsMargins(gap(SpaceRole::Xl), gap(SpaceRole::L), gap(SpaceRole::Xl), gap(SpaceRole::L));
    top->setLayout(top_layout);
    layout->addWidget(top);

    auto* transcript = new chat::TranscriptView;
    const auto* cards_state = &remote.cards;
    transcript->set_slot([cards_state, out](chat::Slot slot) -> QWidget* {
        const auto* card = cards_state->card(slot.node);
        if (!card) return widgets::label("Decision no longer available", TextRole::Body, kit::Tone::Muted);
        auto* view = new cards::CardView;
        view->render(*card, *cards_state, cards_state->focused() == slot.node);
        QObject::connect(view, &cards::CardView::message, view, [out](const cards::Message& m) { out(remote_msg::Card{m}); });
        return view;
    });
    transcript->render(remote.chat, remote.dom);
    QObject::connect(transcript, &chat::TranscriptView::message, transcript,
                     [out](const chat::Message& m) { out(remote_msg::Chat{m}); });
    layout->addWidget(transcript, 1);

    QWidget* footer = nullptr;
    if (remote.ended) {
        footer = widgets::label("No longer following " + remote.peer_name + ": " + *remote.ended, TextRole::Caption, kit::Tone::Muted);
    } else if (remote.can_drive()) {
        footer = new QWidget;
        auto* bar = row(SpaceRole::S);
        footer->setLayout(bar);
        auto* input = new QLineEdit;
        input->setAccessibleName(QStringLiteral("prompt"));
        input->setPlaceholderText(QString::fromStdString("Prompt " + remote.peer_name + "'s harness"));
        input->setText(QString::fromStdString(remote.input));
        input->setFont(kit::qt::font(TextRole::Body));
        input->setEnabled(!remote.sending);
        // The typed text reaches the state with the send, so a re-render doesn't take the focus mid-word.
        auto submit = [out, input] {
            out(remote_msg::Input{input->text().toStdString()});
            out(remote_msg::Send{});
        };
        QObject::connect(input, &QLineEdit::returnPressed, input, submit);
        bar->addWidget(input, 1);
        auto* send_button = widgets::icon_button(Icon::ArrowUp, "Send", remote.sending ? widgets::Callback{} : widgets::Callback{submit});
        bar->addWidget(send_button);
    } else {
        footer = widgets::label("View only: " + remote.peer_name + "'s grant doesn't let this node prompt or answer", TextRole::Caption,
                                kit::Tone::Muted);
    }
    auto* footer_box = new QWidget;
    auto* footer_layout = new QHBoxLayout(footer_box);
    footer_layout->setContentsMargins(gap(SpaceRole::Xl), gap(SpaceRole::S), gap(SpaceRole::Xl), gap(SpaceRole::L));
    footer->setMaximumWidth(px(theme::width(theme::LayoutRole::ComposerWidth)));
    footer_layout->addWidget(footer);
    layout->addWidget(footer_box);
    return root;
}

QWidget* State::folder_view(const Browser& browser, const Emit& send) {
    auto out = [send](FolderMessage m) { send(message(msg::Folder{std::move(m)})); };
    auto* root = new QWidget;
    root->setAccessibleName(QStringLiteral("peer folder"));
    auto* layout = column(root, SpaceRole::M);
    layout->setContentsMargins(gap(SpaceRole::Xl), gap(SpaceRole::L), gap(SpaceRole::Xl), gap(SpaceRole::L));

    const std::string access = browser.can_write ? "read, write" : "read only";
    layout->addLayout(header(browser.folder, "Shared by " + browser.peer_name + " · " + access,
                             {widgets::text_button("Refresh", kit::ButtonKind::secondary(), [out] { out(folder_msg::Refresh{}); }),
                              widgets::icon_button(Icon::X, "Close", [out] { out(folder_msg::Close{}); })}));

    // Breadcrumbs: the shared folder's name, then each folder entered.
    std::vector<std::string> crumbs;
    {
        std::string_view folder = browser.folder;
        while (folder.size() > 1 && folder.ends_with('/')) folder.remove_suffix(1);
        const auto slash = folder.rfind('/');
        auto name = slash == std::string_view::npos ? folder : folder.substr(slash + 1);
        crumbs.emplace_back(name.empty() ? browser.folder : std::string(name));
        std::string_view rest = browser.path;
        while (!rest.empty()) {
            const auto cut = rest.find('/');
            auto part = rest.substr(0, cut);
            if (!part.empty()) crumbs.emplace_back(part);
            if (cut == std::string_view::npos) break;
            rest.remove_prefix(cut + 1);
        }
    }
    auto* crumb_row = row(SpaceRole::Xs);
    for (std::size_t depth = 0; depth < crumbs.size(); ++depth) {
        if (depth > 0) crumb_row->addWidget(glyph(Icon::ChevronRight, TextRole::Caption, kit::Tone::Muted));
        crumb_row->addWidget(widgets::text_button(crumbs[depth], kit::ButtonKind::row(depth + 1 == crumbs.size()),
                                                  [out, depth] { out(folder_msg::Crumb{depth}); }));
    }
    crumb_row->addStretch(1);
    layout->addLayout(crumb_row);

    auto* panes = row(SpaceRole::M);
    auto* list = new QWidget;
    auto* list_layout = column(list, SpaceRole::Xs);
    if (browser.entries.empty())
        list_layout->addWidget(widgets::label(browser.loading ? "Listing…" : "This folder is empty", TextRole::Label, kit::Tone::Muted));
    for (const auto& entry : browser.entries) {
        const bool selected = browser.file && browser.file->path == join(browser.path, entry.name);
        const auto name = entry.name;
        widgets::Callback on = entry.dir ? widgets::Callback([out, name] { out(folder_msg::Enter{name}); })
                                         : widgets::Callback([out, name] { out(folder_msg::Open{name}); });
        list_layout->addWidget(widgets::two_line_row(entry.name, entry.dir ? "Folder" : size_text(entry.size), selected, on));
    }
    list_layout->addStretch(1);
    auto* scroll = new QScrollArea;
    scroll->setWidgetResizable(true);
    scroll->setWidget(list);
    auto* files = widgets::card(scroll);
    files->setMaximumWidth(px(theme::width(theme::LayoutRole::ListWidth)));
    panes->addWidget(files, 1);

    QWidget* preview = nullptr;
    if (!browser.file) {
        preview = widgets::card(widgets::label("Pick a file to open it here", TextRole::Label, kit::Tone::Muted));
    } else {
        const auto& file = *browser.file;
        const auto* text = std::get_if<std::string>(&file.preview);
        const bool editable = browser.can_write && text;
        auto* body = new QWidget;
        auto* body_layout = column(body, SpaceRole::S);
        auto* bar = row(SpaceRole::S);
        bar->addWidget(glyph(Icon::FileText, TextRole::Body, kit::Tone::Muted));
        bar->addWidget(widgets::label(file.path, TextRole::Label, kit::Tone::Normal), 1);
        QPlainTextEdit* editor = nullptr;
        if (text) {
            editor = new QPlainTextEdit(QString::fromStdString(*text));
            editor->setAccessibleName(QStringLiteral("file text"));
            editor->setFont(kit::qt::font(TextRole::Mono));
            editor->setReadOnly(!editable);
        }
        if (editable) {
            const char* state = file.saving ? "Saving…" : file.dirty ? "Save" : "Saved";
            // The edited text reaches the state with the save, so a re-render doesn't take the
            // focus mid-edit; the button arms as soon as the text changes.
            auto* save = widgets::text_button(state, kit::ButtonKind::primary(), [out, editor] {
                out(folder_msg::Edit{editor->toPlainText().toStdString()});
                out(folder_msg::Save{});
            });
            save->setEnabled(file.dirty && !file.saving);
            if (!file.saving)
                QObject::connect(editor, &QPlainTextEdit::textChanged, save, [save] {
                    save->setText(QStringLiteral("Save"));
                    save->setEnabled(true);
                });
            bar->addWidget(save);
        } else {
            bar->addWidget(widgets::label("View only", TextRole::Caption, kit::Tone::Muted));
        }
        bar->addWidget(widgets::icon_button(Icon::X, "Close file", [out] { out(folder_msg::CloseFile{}); }));
        body_layout->addLayout(bar);
        if (editor) {
            body_layout->addWidget(editor, 1);
        } else {
            const auto size = std::get<Binary>(file.preview).size;
            body_layout->addWidget(
                widgets::label("Binary file, " + size_text(size) + "; no text preview", TextRole::Label, kit::Tone::Muted), 1);
        }
        preview = widgets::card(body);
    }
    panes->addWidget(preview, 2);
    layout->addLayout(panes, 1);
    return root;
}

} // namespace loa::app::areas::peers
