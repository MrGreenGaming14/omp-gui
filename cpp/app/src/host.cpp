#include "loa/app/host.hpp"

#include <algorithm>

#include <asio.hpp>

#include <QApplication>
#include <QClipboard>
#include <QDir>
#include <QImage>
#include <QImageReader>
#include <QKeyEvent>
#include <QHBoxLayout>
#include <QLabel>
#include <QMimeData>
#include <QPointer>
#include <QScrollArea>
#include <QStackedWidget>
#include <QTimer>
#include <QVBoxLayout>

#include "loa/app/panes.hpp"
#include "loa/app/routes.hpp"
#include "loa/app_runtime/session.hpp"
#include "loa/runtime/log.hpp"
#include "loa/runtime/time.hpp"
#include "loa/ui/app/asio_bridge.hpp"
#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/kit/widgets.hpp"
#include "loa/ui/panel/view.hpp"
#include "loa/ui/session/composer/view.hpp"
#include "loa/ui/session/transcript/view.hpp"
#include "loa/ui/shell/view.hpp"
#include "loa/ui/terminal/runner.hpp"
#include "loa/ui/terminal/view.hpp"

namespace loa::app {

namespace {

namespace kit = ui::kit;
namespace theme = ui::kit::theme;
namespace widgets = ui::kit::widgets;

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

int px(float v) { return static_cast<int>(v + 0.5F); }

QString q(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }

// Removes every child widget of `layout`.
void clear(QVBoxLayout* layout) {
    while (auto* item = layout->takeAt(0)) {
        if (auto* w = item->widget()) w->deleteLater();
        delete item;
    }
}

// A vertical layout on `w` with the gap of `role` and no margins.
QVBoxLayout* column(QWidget* w, theme::SpaceRole role) {
    auto* layout = new QVBoxLayout(w);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(px(theme::sp(role)));
    return layout;
}

// The clipboard's image saved as a PNG in `dir`; `nullopt` when it holds none.
std::expected<std::optional<Pasted>, std::string> read_clipboard_image(const std::filesystem::path& dir) {
    const auto* mime = QApplication::clipboard()->mimeData();
    if (!mime || !mime->hasImage()) return std::optional<Pasted>{};
    const auto image = qvariant_cast<QImage>(mime->imageData());
    if (image.isNull()) return std::optional<Pasted>{};
    std::error_code ec;
    std::filesystem::create_directories(dir, ec);
    if (ec) return std::unexpected(dir.string() + ": " + ec.message());
    const auto path = dir / (app_runtime::new_session_id() + ".png");
    if (!image.save(QString::fromStdString(path.string()), "PNG")) return std::unexpected(path.string() + ": can't write");
    return std::optional<Pasted>(
        Pasted{path, static_cast<std::uint32_t>(image.width()), static_cast<std::uint32_t>(image.height())});
}

std::optional<std::pair<std::uint32_t, std::uint32_t>> image_size(const std::filesystem::path& path) {
    QImageReader reader(QString::fromStdString(path.string()));
    const auto size = reader.size();
    if (!size.isValid()) return std::nullopt;
    return std::pair{static_cast<std::uint32_t>(size.width()), static_cast<std::uint32_t>(size.height())};
}

} // namespace

// ---- AppView ---------------------------------------------------------------------------------

AppView::AppView(Emit send, QWidget* parent) : QWidget(parent), emit_(std::move(send)) {
    setAccessibleName(QStringLiteral("Loa"));
    auto* root = column(this, theme::SpaceRole::Xs);
    shell_ = new ui::shell::ShellView(this);
    root->addWidget(shell_);
    connect(shell_, &ui::shell::ShellView::message, this,
            [this](const ui::shell::layout::Message& m) { emit_(msg::Shell{m}); });

    center_ = new QWidget;
    center_->setAccessibleName(QStringLiteral("Center"));
    auto* center = column(center_, theme::SpaceRole::Xs);
    pages_ = new QStackedWidget(center_);
    center->addWidget(pages_, 1);
    notice_ = widgets::label("", theme::TextRole::Caption, kit::Tone::Muted, center_);
    notice_->setAccessibleName(QStringLiteral("Notice"));
    notice_->setWordWrap(true);
    notice_->setContentsMargins(px(theme::sp(theme::SpaceRole::Xl)), px(theme::sp(theme::SpaceRole::Xs)),
                                px(theme::sp(theme::SpaceRole::Xl)), px(theme::sp(theme::SpaceRole::Xs)));
    center->addWidget(notice_);

    screen_page_ = new QWidget;
    screen_page_->setAccessibleName(QStringLiteral("Area screen"));
    column(screen_page_, theme::SpaceRole::Xs);
    pages_->addWidget(screen_page_);

    session_page_ = new QWidget;
    session_page_->setAccessibleName(QStringLiteral("Session"));
    session_layout_ = column(session_page_, theme::SpaceRole::Xs);
    transcript_ = new ui::session::transcript::TranscriptView;
    connect(transcript_, &ui::session::transcript::TranscriptView::message, this,
            [this](const ui::session::transcript::Message& m) { emit_(msg::Chat{m}); });
    session_layout_->addWidget(transcript_, 1);
    pages_->addWidget(session_page_);

    start_page_ = new QWidget;
    start_page_->setAccessibleName(QStringLiteral("Start"));
    start_page_->setObjectName(QStringLiteral("Start"));
    start_layout_ = column(start_page_, theme::SpaceRole::L);
    pages_->addWidget(start_page_);

    composer_ = new ui::session::composer::ComposerView;
    connect(composer_, &ui::session::composer::ComposerView::message, this,
            [this](const ui::session::composer::Message& m) { emit_(msg::Composer{m}); });
    shell_->set_center(center_);

    panel_ = new QStackedWidget;
    panel_->setAccessibleName(QStringLiteral("Panel"));
    diff_ = new ui::panel::DiffView;
    connect(diff_, &ui::panel::DiffView::message, this, [this](const ui::panel::diff::Message& m) { emit_(msg::Diff{m}); });
    files_ = new ui::panel::FilesView;
    connect(files_, &ui::panel::FilesView::message, this,
            [this](const ui::panel::files::Message& m) { emit_(msg::Files{m}); });
    terminal_ = new ui::terminal::TerminalView;
    connect(terminal_, &ui::terminal::TerminalView::message, this,
            [this](const ui::terminal::Message& m) { emit_(msg::Terminal{m}); });
    area_tab_ = new QWidget;
    area_tab_->setAccessibleName(QStringLiteral("Area tab"));
    column(area_tab_, theme::SpaceRole::Xs);
    for (auto* w : {static_cast<QWidget*>(diff_), static_cast<QWidget*>(files_), static_cast<QWidget*>(terminal_), area_tab_})
        panel_->addWidget(w);
    shell_->set_panel_body(panel_);

    scrim_ = new QWidget(this);
    scrim_->setAccessibleName(QStringLiteral("Scrim"));
    kit::qt::style_container(*scrim_, kit::Surface::Scrim);
    scrim_->hide();
    overlay_ = new QWidget(this);
    overlay_->setAccessibleName(QStringLiteral("Overlay"));
    column(overlay_, theme::SpaceRole::M);
    overlay_->hide();

    palette_ = new ui::shell::PaletteView(this);
    connect(palette_, &ui::shell::PaletteView::message, this,
            [this](const ui::shell::palette::Message& m) { emit_(msg::Palette{m}); });
    palette_->hide();
}

AppView::~AppView() {
    // The composer may sit outside the layouts between renders.
    if (composer_ && !composer_->parent()) delete composer_;
}

void AppView::resizeEvent(QResizeEvent* event) {
    QWidget::resizeEvent(event);
    place_overlays();
}

void AppView::keyPressEvent(QKeyEvent* event) {
    if (auto key = ui::shell::to_key_press(*event)) {
        emit_(msg::Keyboard{std::move(*key)});
        return;
    }
    QWidget::keyPressEvent(event);
}

void AppView::place_overlays() {
    scrim_->setGeometry(rect());
    const int top = px(theme::width(theme::LayoutRole::NavWidth) / 4.0F);
    const int w = std::min(width(), px(theme::width(theme::LayoutRole::WindowWidth)));
    const int h = std::min(height() - top, px(theme::width(theme::LayoutRole::WindowHeight)));
    overlay_->setGeometry((width() - w) / 2, top, w, std::max(0, h));
    const int pw = std::min(width(), px(theme::width(theme::LayoutRole::PopupWidth)));
    palette_->setGeometry((width() - pw) / 2, top, pw, std::max(0, height() - 2 * top));
}

void AppView::render(const App& app) {
    shell_->render(app.ctx.shell);
    render_center(app);
    render_panel(app);
    render_overlay(app);
    if (app.palette.is_open()) {
        palette_->render(app.palette);
        palette_->show();
        palette_->raise();
    } else {
        palette_->hide();
    }
    place_overlays();
}

void AppView::render_center(const App& app) {
    const auto& ctx = app.ctx;
    const auto& notice = ctx.notice();
    notice_->setText(notice ? q(*notice) : QString());
    notice_->setVisible(notice.has_value());

    QWidget* screen = nullptr;
    if (auto id = ctx.screen())
        if (const auto* area = app.areas.find(*id)) screen = area->view_screen(ctx, emit_);
    if (screen) {
        if (screen_) screen_->deleteLater();
        screen_ = screen;
        screen_page_->layout()->addWidget(screen_);
        pages_->setCurrentWidget(screen_page_);
        return;
    }
    if (const auto* active = ctx.runtime.sessions().active()) {
        const std::string session = active->id().str();
        const auto* decisions = app.areas.port<DecisionsPort>();
        transcript_->set_slot([decisions, session, send = emit_](ui::session::transcript::Slot slot) -> QWidget* {
            QWidget* card = decisions ? decisions->card(session, slot.node, send) : nullptr;
            return card ? card : widgets::label("Decision no longer available", theme::TextRole::Body, kit::Tone::Muted);
        });
        transcript_->render(app.sessions.chat, active->dom());
        for (auto** old : {&child_strip_, &child_body_})
            if (*old) {
                (*old)->deleteLater();
                *old = nullptr;
            }
        if (const auto* sessions = app.areas.port<SessionsPort>())
            if (auto child = sessions->view_child_center(ctx, emit_)) {
                child_strip_ = child->first;
                session_layout_->insertWidget(0, child_strip_);
                // A picked subagent's tab takes the place of the conversation and its composer.
                child_body_ = child->second;
                if (child_body_) session_layout_->insertWidget(1, child_body_, 1);
            }
        if (composer_->parent() != session_page_) session_layout_->addWidget(composer_);
        transcript_->setVisible(!child_body_);
        composer_->setVisible(!child_body_);
        composer_->render(ctx.composer);
        pages_->setCurrentWidget(session_page_);
        return;
    }
    render_start(app);
    pages_->setCurrentWidget(start_page_);
}

// The center with no session open: a heading, the composer and the recent sessions of the
// active project (of every project without one).
void AppView::render_start(const App& app) {
    const auto& ctx = app.ctx;
    if (composer_->parent()) composer_->setParent(nullptr);
    clear(start_layout_);
    const bool starting = ctx.runtime.sessions().starting().has_value();
    const auto& project = ctx.runtime.project();
    // Where new sessions go: the active project, else the folder.
    const auto place = project ? *project : chip(*ctx.services.host.git, ctx.runtime.cwd()).path;
    start_layout_->addStretch(1);
    auto* heading = widgets::label("What should we work on?", theme::TextRole::Title, kit::Tone::Normal);
    heading->setAlignment(Qt::AlignHCenter);
    start_layout_->addWidget(heading);
    auto* status = widgets::label(starting ? "Starting a session in " + place + "…" : "in " + place,
                                  theme::TextRole::Body, kit::Tone::Muted);
    status->setAlignment(Qt::AlignHCenter);
    start_layout_->addWidget(status);
    start_layout_->addWidget(composer_);
    composer_->render(ctx.composer);
    const auto now = loa::runtime::time::now_ms();
    auto* recents = new QWidget;
    recents->setAccessibleName(QStringLiteral("Recent sessions"));
    auto* rows = column(recents, theme::SpaceRole::S);
    std::size_t shown = 0;
    for (const auto* summary : ctx.shell.sidebar().recents()) {
        if (shown == RECENTS) break;
        if (project && summary->project != *project) continue;
        auto id = summary->id;
        rows->addWidget(widgets::two_line_row(summary->title, loa::runtime::time::ago(now, summary->updated), false,
                                              [send = emit_, id] { send(msg::OpenRecent{id}); }));
        ++shown;
    }
    if (shown > 0) {
        start_layout_->addWidget(widgets::label(project ? "Recent in " + place : std::string("Recent"),
                                                theme::TextRole::Caption, kit::Tone::Muted));
        start_layout_->addWidget(recents);
    } else {
        delete recents;
    }
    start_layout_->addStretch(1);
}

void AppView::render_panel(const App& app) {
    const auto tab = app.ctx.shell.active_tab();
    if (!tab) return;
    if (const auto* pane = panes::find(app, *tab)) {
        switch (pane->kind) {
        case panes::Kind::Diff:
            diff_->render(app.diff);
            panel_->setCurrentWidget(diff_);
            break;
        case panes::Kind::Files:
            files_->render(app.files);
            panel_->setCurrentWidget(files_);
            break;
        case panes::Kind::Terminal:
            terminal_->render(app.terminal);
            panel_->setCurrentWidget(terminal_);
            break;
        }
    } else {
        auto* layout = static_cast<QVBoxLayout*>(area_tab_->layout());
        clear(layout);
        QWidget* body = nullptr;
        for (const auto& area : app.areas.iter())
            if ((body = area->view_tab(*tab, app.ctx, emit_))) break;
        layout->addWidget(body ? body : widgets::label("Unknown tab", theme::TextRole::Body, kit::Tone::Muted));
        panel_->setCurrentWidget(area_tab_);
    }
}

void AppView::render_overlay(const App& app) {
    const auto& overlay = app.windows.overlay;
    scrim_->setVisible(overlay.has_value());
    overlay_->setVisible(overlay.has_value());
    if (!overlay) return;
    auto* layout = static_cast<QVBoxLayout*>(overlay_->layout());
    clear(layout);
    auto close = [send = emit_] { send(msg::Window{window_msg::CloseOverlay{}}); };
    auto* frame = new QWidget;
    frame->setAccessibleName(QStringLiteral("Window"));
    kit::qt::style_container(*frame, kit::Surface::Popup);
    auto* body = column(frame, theme::SpaceRole::M);
    const int pad = px(theme::sp(theme::SpaceRole::Xl));
    body->setContentsMargins(pad, pad, pad, pad);
    layout->addWidget(frame);
    auto header = [&](std::string_view title) {
        body->addWidget(widgets::section_header(title, {widgets::icon_button(loa::view::theme::Icon::X, "Close", close)}));
    };
    const auto& ctx = app.ctx;
    std::visit(overloaded{
                   [&](const overlay::Usage&) { body->addWidget(usage::view(app.usage, loa::runtime::time::now_ms(), emit_), 1); },
                   [&](const overlay::Settings& s) {
                       auto* nav = new QWidget;
                       auto* tabs = column(nav, theme::SpaceRole::Xs);
                       for (auto tab : SETTINGS_TABS)
                           tabs->addWidget(widgets::menu_row(std::nullopt, label(tab), tab == s.tab, [send = emit_, tab] {
                               send(msg::Window{window_msg::OpenSettings{tab}});
                           }));
                       body->addWidget(nav);
                       header(label(s.tab));
                       QWidget* content = nullptr;
                       switch (s.tab) {
                       case SettingsTab::General: {
                           auto* row = new QWidget;
                           auto* choices = column(row, theme::SpaceRole::Xs);
                           const auto& current = ctx.shell.theme_state().preference().as_str();
                           choices->addWidget(widgets::label("Theme", theme::TextRole::Body, kit::Tone::Normal));
                           choices->addWidget(
                               widgets::label("Follows your system unless set", theme::TextRole::Label, kit::Tone::Faint));
                           for (const auto& [id, name] : ctx.shell.theme_state().themes()) {
                               const auto kind = id == current ? kit::ButtonKind::primary() : kit::ButtonKind::secondary();
                               choices->addWidget(widgets::text_button(name, kind, [send = emit_, id] {
                                   send(msg::Window{window_msg::SetTheme{loa::view::theme::ThemeRef{id}}});
                               }));
                           }
                           content = row;
                           break;
                       }
                       case SettingsTab::Peers:
                           if (const auto* peers = app.areas.find(area_id::PEERS)) content = peers->view_tab("peers", ctx, emit_);
                           break;
                       case SettingsTab::Harnesses:
                           if (const auto* models = app.areas.port<ModelsPort>())
                               content = models->page_body(to::models::Page::SignIn, emit_);
                           break;
                       case SettingsTab::Updates:
                           if (const auto* updater = app.areas.find(area_id::UPDATER)) content = updater->view_screen(ctx, emit_);
                           break;
                       }
                       if (content) body->addWidget(content, 1);
                   },
                   [&](const overlay::Configs& c) {
                       auto* nav = new QWidget;
                       auto* tabs = column(nav, theme::SpaceRole::Xs);
                       for (auto tab : CONFIGS_TABS)
                           tabs->addWidget(widgets::menu_row(std::nullopt, label(tab), tab == c.tab, [send = emit_, tab] {
                               send(msg::Window{window_msg::OpenConfigs{tab}});
                           }));
                       body->addWidget(nav);
                       header(label(c.tab));
                       body->addWidget(widgets::label(blurb(c.tab), theme::TextRole::Label, kit::Tone::Muted));
                       QWidget* content = nullptr;
                       if (c.tab == ConfigsTab::Models) {
                           if (const auto* models = app.areas.port<ModelsPort>())
                               content = models->page_body(to::models::Page::Settings, emit_);
                       } else if (c.tab == ConfigsTab::Tools) {
                           if (const auto* tools = app.areas.find(area_id::TOOLS)) content = tools->view_tab("tools", ctx, emit_);
                       } else if (c.tab == ConfigsTab::Agents) {
                           if (const auto* agents = app.areas.port<AgentsPort>()) content = agents->configs_body(ctx, emit_);
                       }
                       if (!content) {
                           const auto& docs = app.windows.docs;
                           if (docs.files.empty()) {
                               content = widgets::label("Nothing yet. omp reads these from " + std::string(places(c.tab)) + ".",
                                                        theme::TextRole::Body, kit::Tone::Muted);
                           } else {
                               auto* list = new QWidget;
                               auto* rows = column(list, theme::SpaceRole::Xs);
                               for (std::size_t i = 0; i < docs.files.size(); ++i)
                                   rows->addWidget(widgets::two_line_row(docs.files[i].name, docs.files[i].scope,
                                                                         i == docs.selected, [send = emit_, i] {
                                                                             send(msg::Window{window_msg::PickDoc{i}});
                                                                         }));
                               const auto& file = docs.files[std::min(docs.selected, docs.files.size() - 1)];
                               rows->addWidget(widgets::text_button(docs.full ? "Exit full screen" : "Full screen",
                                                                    kit::ButtonKind::ghost(), [send = emit_] {
                                                                        send(msg::Window{window_msg::ToggleDocFull{}});
                                                                    }));
                               rows->addWidget(widgets::label(file.path.string(), theme::TextRole::Caption, kit::Tone::Muted));
                               auto* scroll = new QScrollArea;
                               scroll->setWidgetResizable(true);
                               scroll->setWidget(widgets::mono_block(file.text));
                               rows->addWidget(scroll, 1);
                               content = list;
                           }
                       }
                       body->addWidget(content, 1);
                   },
               },
               *overlay);
    scrim_->raise();
    overlay_->raise();
}

void AppView::apply(const UiOp& op) {
    std::visit(overloaded{
                   [&](const ui_op::Pane& p) {
                       ui::shell::Task task{{p.op}};
                       switch (p.target) {
                       case ui_op::Pane::Target::Palette: palette_->apply(task); break;
                       case ui_op::Pane::Target::Diff: diff_->apply(task); break;
                       // The frame's own operations (tab strip scrolls, the system theme query)
                       // land on its next render.
                       case ui_op::Pane::Target::Shell:
                       case ui_op::Pane::Target::Files: break;
                       }
                   },
                   [&](const ui_op::Composer& c) { composer_->apply(c.task); },
                   [&](const ui_op::Chat& c) {
                       if (!c.detached) transcript_->apply(c.task);
                   },
                   [&](const ui_op::Terminal& t) { terminal_->apply(t.task); },
                   [](const auto&) {},
               },
               op);
}

// ---- AppHost ---------------------------------------------------------------------------------

AppHost::AppHost(std::unique_ptr<App> app, ui::app::AsioBridge& bridge) : app_(std::move(app)), bridge_(bridge) {
    app_->read_clipboard_image = read_clipboard_image;
    app_->image_size = image_size;
    auto post = [this, alive = std::weak_ptr<bool>(alive_)](ui::terminal::Message m) {
        bridge_.post([this, alive, m = std::move(m)]() mutable {
            if (alive.lock()) dispatch(msg::Terminal{std::move(m)});
        });
    };
    runner_ = std::make_unique<ui::terminal::Runner>(bridge_.executor().context().get_executor(), std::move(post));
}

AppHost::~AppHost() {
    *alive_ = false;
    alive_.reset();
    for (auto& [key, running] : subscriptions_) {
        running.stop.request_stop();
        if (running.timer) running.timer->deleteLater();
    }
    for (auto& [id, window] : detached_)
        if (window) window->deleteLater();
    if (main_) delete main_.data();
}

Emit AppHost::emitter() {
    return [this, alive = std::weak_ptr<bool>(alive_)](Message message) {
        bridge_.post([this, alive, message = std::move(message)]() mutable {
            if (alive.lock()) dispatch(std::move(message));
        });
    };
}

void AppHost::start(Task task) {
    run(std::move(task));
    render();
    resubscribe();
}

void AppHost::dispatch(Message message) {
    run(app_->update(std::move(message)));
    render();
    resubscribe();
}

void AppHost::run(Task task) {
    for (auto& message : task.messages) {
        bridge_.post([this, alive = std::weak_ptr<bool>(alive_), message = std::move(message)]() mutable {
            if (alive.lock()) dispatch(std::move(message));
        });
    }
    // The executor logs what a job throws.
    for (auto& job : task.jobs) job(emitter(), bridge_.executor());
    for (auto& op : task.ops) {
        std::visit(overloaded{
                       [&](const ui_op::OpenWindow& o) { open_window(o); },
                       [&](const ui_op::FocusWindow& f) {
                           if (auto it = detached_.find(f.id); it != detached_.end() && it->second) {
                               it->second->raise();
                               it->second->activateWindow();
                           }
                       },
                       [&](const ui_op::CloseWindow& c) {
                           if (auto it = detached_.find(c.id); it != detached_.end()) {
                               if (it->second) it->second->deleteLater();
                               detached_.erase(it);
                           }
                       },
                       [&](const ui_op::Exit&) { QApplication::quit(); },
                       [&](const ui_op::FocusNext& f) {
                           auto* from = QApplication::focusWidget();
                           if (!from) return;
                           // The next widget in the chain that takes Tab focus, as Tab itself moves.
                           for (auto* next = f.forward ? from->nextInFocusChain() : from->previousInFocusChain();
                                next && next != from; next = f.forward ? next->nextInFocusChain() : next->previousInFocusChain()) {
                               if ((next->focusPolicy() & Qt::TabFocus) && next->isVisible() && next->isEnabled() &&
                                   next->window() == from->window()) {
                                   next->setFocus(f.forward ? Qt::TabFocusReason : Qt::BacktabFocusReason);
                                   return;
                               }
                           }
                       },
                       [&](const ui_op::Terminal& t) {
                           runner_->run(t.task, app_->terminal.backend());
                           if (view_) view_->apply(op);
                       },
                       [&](const auto&) {
                           if (view_) view_->apply(op);
                       },
                   },
                   op);
    }
}

void AppHost::open_window(const ui_op::OpenWindow& op) {
    if (app_->windows.main == op.id) {
        auto* window = new QWidget;
        window->setWindowTitle(QStringLiteral("Loa"));
        window->setAccessibleName(QStringLiteral("Loa"));
        window->resize(px(op.width), px(op.height));
        window->setMinimumSize(px(theme::width(theme::LayoutRole::MinWidth)), px(theme::width(theme::LayoutRole::MinHeight)));
        auto* layout = column(window, theme::SpaceRole::Xs);
        view_ = new AppView(emitter(), window);
        layout->addWidget(view_);
        main_ = window;
        window->show();
        render();
        return;
    }
    auto* window = new QWidget;
    window->setAttribute(Qt::WA_DeleteOnClose);
    window->resize(px(op.width), px(op.height));
    window->setMinimumSize(px(theme::width(theme::LayoutRole::MinWidth)), px(theme::width(theme::LayoutRole::MinHeight)));
    column(window, theme::SpaceRole::Xs);
    detached_[op.id] = window;
    connect(window, &QObject::destroyed, this, [this, id = op.id, alive = std::weak_ptr<bool>(alive_)] {
        if (!alive.lock()) return;
        if (detached_.erase(id) > 0) dispatch(msg::Window{window_msg::Closed{id}});
    });
    window->show();
    render_detached();
}

void AppHost::render_detached() {
    for (const auto& [id, what] : app_->windows.detached) {
        auto it = detached_.find(id);
        if (it == detached_.end() || !it->second) continue;
        QWidget* window = it->second;
        window->setWindowTitle(QString::fromStdString(app_->title(id)));
        auto* layout = static_cast<QVBoxLayout*>(window->layout());
        clear(layout);
        auto* header = new QWidget;
        auto* row = new QHBoxLayout(header);
        row->addWidget(widgets::label(app_->detached_title(what), theme::TextRole::Body, kit::Tone::Normal), 1);
        row->addWidget(widgets::text_button("Reattach", kit::ButtonKind::ghost(), [send = emitter(), id] {
            send(msg::Window{window_msg::Reattach{id}});
        }));
        layout->addWidget(header);
        QWidget* body = nullptr;
        if (const auto* tab = std::get_if<detached::Tab>(&what)) {
            for (const auto& area : app_->areas.iter())
                if ((body = area->view_tab(tab->id, app_->ctx, emitter()))) break;
        } else {
            const auto& key = std::get<detached::Transcript>(what).key;
            if (auto found = app_->sessions.detached_transcripts.find(key); found != app_->sessions.detached_transcripts.end()) {
                auto* chat = new ui::session::transcript::TranscriptView;
                connect(chat, &ui::session::transcript::TranscriptView::message, this,
                        [send = emitter(), key](const ui::session::transcript::Message& m) {
                            send(msg::DetachedChat{key, m});
                        });
                chat->set_slot([](ui::session::transcript::Slot) -> QWidget* {
                    return widgets::label("Decision", theme::TextRole::Caption, kit::Tone::Muted);
                });
                chat->render(found->second.chat, found->second.dom);
                body = chat;
            }
        }
        layout->addWidget(body ? body
                               : widgets::label("This transcript is no longer open", theme::TextRole::Body, kit::Tone::Muted),
                          1);
    }
}

void AppHost::render() {
    if (!view_) return;
    view_->render(*app_);
    if (main_) main_->setWindowTitle(QString::fromStdString(app_->title(app_->windows.main.value_or(0))));
}

void AppHost::resubscribe() {
    std::map<std::string, Subscription> wanted;
    for (auto& sub : app_->subscription()) {
        auto key = std::visit([](const auto& s) { return s.key; }, sub);
        wanted.emplace(std::move(key), std::move(sub));
    }
    for (auto it = subscriptions_.begin(); it != subscriptions_.end();) {
        if (wanted.contains(it->first)) {
            ++it;
            continue;
        }
        it->second.stop.request_stop();
        if (it->second.timer) it->second.timer->deleteLater();
        it = subscriptions_.erase(it);
    }
    for (auto& [key, sub] : wanted) {
        if (subscriptions_.contains(key)) continue;
        Running running;
        std::visit(overloaded{
                       [&](subscription::Every& every) {
                           auto* timer = new QTimer(this);
                           timer->setInterval(static_cast<int>(every.period.count()));
                           connect(timer, &QTimer::timeout, this, [this, message = every.message] { dispatch(message); });
                           timer->start();
                           running.timer = timer;
                       },
                       [&](subscription::Stream& stream) {
                           stream.run(emitter(), running.stop.get_token(), bridge_.executor());
                       },
                   },
                   sub);
        subscriptions_.emplace(key, std::move(running));
    }
}

} // namespace loa::app
