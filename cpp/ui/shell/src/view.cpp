#include "loa/ui/shell/view.hpp"

#include <algorithm>
#include <cmath>
#include <format>

#include <QHBoxLayout>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QMouseEvent>
#include <QSignalBlocker>
#include <QPushButton>
#include <QScrollArea>
#include <QScrollBar>
#include <QVBoxLayout>

#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/widgets.hpp"

namespace loa::ui::shell {

namespace {

using kit::Surface;
using kit::Tone;
using kit::theme::SpaceRole;
using kit::theme::TextRole;
using loa::view::theme::Icon;
namespace kw = kit::widgets;
namespace is = loa::view::shell::sidebar_intent;
namespace in = loa::view::shell::intent;

int px(float v) { return static_cast<int>(std::lround(v)); }
int spx(SpaceRole r) { return px(kit::theme::sp(r)); }
QString qs(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }

QVBoxLayout* column(QWidget* w, int spacing = 0) {
    auto* l = new QVBoxLayout(w);
    l->setContentsMargins(0, 0, 0, 0);
    l->setSpacing(spacing);
    return l;
}
QHBoxLayout* row(QWidget* w, int spacing = 0) {
    auto* l = new QHBoxLayout(w);
    l->setContentsMargins(0, 0, 0, 0);
    l->setSpacing(spacing);
    return l;
}
QWidget* surface(Surface s, const char* name, QWidget* parent = nullptr) {
    auto* w = new QWidget(parent);
    w->setObjectName(QString::fromLatin1(name));
    w->setAccessibleName(QString::fromLatin1(name));
    w->setAttribute(Qt::WA_StyledBackground);
    kit::qt::style_container(*w, s);
    return w;
}
QLabel* icon_label(Icon i, TextRole r, Tone t) {
    auto* l = new QLabel;
    l->setPixmap(kit::tinted(i, r, t));
    l->setAccessibleName(qs(kit::icon_file(i)));
    return l;
}

// A splitter grab area: reports the press, the cursor's x in the frame while held, and the release.
class Edge final : public QWidget {
  public:
    Edge(ShellView* frame, layout::Pane pane, std::function<void(layout::Message)> send)
        : frame_(frame), pane_(pane), send_(std::move(send)) {
        setAccessibleName(pane == layout::Pane::Sidebar ? QStringLiteral("sidebar edge") : QStringLiteral("panel edge"));
        setCursor(Qt::SplitHCursor);
        setFixedWidth(px(kw::splitter_width()));
    }

  protected:
    void mousePressEvent(QMouseEvent*) override { send_(layout::msg::DragStart{pane_}); }
    void mouseMoveEvent(QMouseEvent* e) override {
        send_(layout::msg::DragMove{static_cast<float>(frame_->mapFromGlobal(e->globalPosition().toPoint()).x())});
    }
    void mouseReleaseEvent(QMouseEvent*) override { send_(layout::msg::DragEnd{}); }

  private:
    ShellView* frame_;
    layout::Pane pane_;
    std::function<void(layout::Message)> send_;
};

void clear(QLayout* l) {
    while (auto* item = l->takeAt(0)) {
        if (auto* w = item->widget()) {
            w->hide();
            w->deleteLater();
        }
        delete item;
    }
}

} // namespace

std::optional<palette::KeyPress> to_key_press(const QKeyEvent& event) {
    using palette::Modifiers;
    using palette::Named;
    palette::KeyPress press;
    const auto m = event.modifiers();
#if defined(__APPLE__)
    if (m & Qt::ControlModifier) press.modifiers = press.modifiers | Modifiers::logo();
    if (m & Qt::MetaModifier) press.modifiers = press.modifiers | Modifiers::ctrl();
#else
    if (m & Qt::ControlModifier) press.modifiers = press.modifiers | Modifiers::ctrl();
    if (m & Qt::MetaModifier) press.modifiers = press.modifiers | Modifiers::logo();
#endif
    if (m & Qt::AltModifier) press.modifiers = press.modifiers | Modifiers::alt();
    if (m & Qt::ShiftModifier) press.modifiers = press.modifiers | Modifiers::shift();
    press.repeat = event.isAutoRepeat();
    const int key = event.key();
    static const std::pair<int, Named> named[] = {
        {Qt::Key_Escape, Named::Escape}, {Qt::Key_Return, Named::Enter}, {Qt::Key_Enter, Named::Enter}, {Qt::Key_Tab, Named::Tab},
        {Qt::Key_Backtab, Named::Tab}, {Qt::Key_Space, Named::Space}, {Qt::Key_Backspace, Named::Backspace},
        {Qt::Key_Delete, Named::Delete}, {Qt::Key_Up, Named::ArrowUp}, {Qt::Key_Down, Named::ArrowDown}, {Qt::Key_Left, Named::ArrowLeft},
        {Qt::Key_Right, Named::ArrowRight}, {Qt::Key_PageUp, Named::PageUp}, {Qt::Key_PageDown, Named::PageDown},
        {Qt::Key_Home, Named::Home}, {Qt::Key_End, Named::End}, {Qt::Key_Insert, Named::Insert}, {Qt::Key_Shift, Named::Shift},
        {Qt::Key_Control, Named::Control}, {Qt::Key_Alt, Named::Alt}, {Qt::Key_AltGr, Named::AltGraph}, {Qt::Key_Super_L, Named::Super},
        {Qt::Key_Super_R, Named::Super}, {Qt::Key_Meta, Named::Meta}, {Qt::Key_Hyper_L, Named::Hyper}, {Qt::Key_Hyper_R, Named::Hyper},
    };
    for (const auto& [k, n] : named) {
        if (k == key) {
            press.key = n;
            return press;
        }
    }
    if (key >= Qt::Key_F1 && key <= Qt::Key_F12) {
        press.key = static_cast<Named>(static_cast<int>(Named::F1) + (key - Qt::Key_F1));
        return press;
    }
    if (key > 0x20 && key < 0x7F) {
        // Qt reports letters uppercase; the chord lowercases them.
        const auto c = static_cast<char>(key);
        press.key = std::string(1, c);
        press.latin = static_cast<char32_t>(c >= 'A' && c <= 'Z' ? c + ('a' - 'A') : c);
        return press;
    }
    if (key >= 0x7F && key < 0x01000000) {
        const auto c = static_cast<char32_t>(key);
        press.key = QString::fromUcs4(&c, 1).toUtf8().toStdString();
        return press;
    }
    if (const auto text = event.text(); !text.isEmpty() && text.at(0).isPrint()) {
        press.key = text.toUtf8().toStdString();
        return press;
    }
    return std::nullopt;
}

// ---- ShellView -----------------------------------------------------------------------------

ShellView::ShellView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("shell"));
    frame_ = row(this);
    search_ = new QLineEdit;
    search_->setObjectName(qs(sidebar::SEARCH_ID));
    search_->setAccessibleName(QStringLiteral("Search"));
    search_->setPlaceholderText(QStringLiteral("Search"));
    connect(search_, &QLineEdit::textEdited, this, [this](const QString& t) { send_sidebar(sidebar::msg::Search{t.toStdString()}); });
}

void ShellView::set_center(QWidget* center) {
    if (center_) center_->deleteLater();
    center_ = center;
}

void ShellView::set_panel_body(QWidget* body) {
    if (panel_body_) panel_body_->deleteLater();
    panel_body_ = body;
}

void ShellView::resizeEvent(QResizeEvent* event) {
    QWidget::resizeEvent(event);
    send(layout::msg::FrameSized{{static_cast<float>(width()), static_cast<float>(height())}});
}

QWidget* ShellView::edge(layout::Pane pane) { return new Edge(this, pane, [this](layout::Message m) { send(std::move(m)); }); }

void ShellView::render(const layout::State& state) {
    // The persistent pieces survive the rebuild so focus and caret stay put.
    search_->setParent(nullptr);
    if (center_) center_->setParent(nullptr);
    if (panel_body_) panel_body_->setParent(nullptr);
    clear(frame_);

    const auto full = state.full();
    const auto show = [&](layout::Pane p) { return !full || full == p; };
    if (show(layout::Pane::Sidebar)) {
        auto* sb = build_sidebar(state);
        if (full == layout::Pane::Sidebar) sb->setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Expanding);
        else sb->setFixedWidth(px(state.sidebar_width()));
        frame_->addWidget(sb);
    }
    if (!full) frame_->addWidget(edge(layout::Pane::Sidebar));
    if (show(layout::Pane::Center)) {
        auto* center = new QWidget;
        center->setAccessibleName(QStringLiteral("center"));
        auto* col = column(center);
        if (state.project_tabs()) col->addWidget(build_project_tabs(state));
        col->addWidget(build_topbar(state));
        center_slot_ = surface(Surface::Ground, "center slot");
        auto* slot = column(center_slot_);
        if (center_) slot->addWidget(center_);
        col->addWidget(center_slot_, 1);
        frame_->addWidget(center, 1);
    }
    if (state.panel_open() && show(layout::Pane::Panel)) {
        if (!full) frame_->addWidget(edge(layout::Pane::Panel));
        auto* panel = build_panel(state);
        if (full == layout::Pane::Panel) frame_->addWidget(panel, 1);
        else {
            panel->setFixedWidth(px(std::min(state.shown_panel_width(), 1.0e6F)));
            frame_->addWidget(panel);
        }
    }
}

QWidget* ShellView::build_sidebar(const layout::State& frame) {
    const auto& s = frame.sidebar();
    auto* root = surface(Surface::Side, "sidebar");
    auto* col = column(root, spx(SpaceRole::Xs));
    col->setContentsMargins(spx(SpaceRole::S), spx(SpaceRole::S), spx(SpaceRole::S), spx(SpaceRole::S));

    const auto machine = s.describe(s.filter());
    col->addWidget(kw::two_line_row(machine.name, machine.sub, s.machines_open(), [this] { send_sidebar(sidebar::msg::ToggleMachines{}); }));
    col->addWidget(kw::menu_row(Icon::Plus, "New session", false, [this] { send_sidebar(sidebar::msg::Emit{is::NewSession{}}); }));
    if (search_->text().toStdString() != s.query()) search_->setText(qs(s.query()));
    col->addWidget(search_);

    auto* lists = new QWidget;
    lists->setAccessibleName(QStringLiteral("sidebar lists"));
    auto* l = column(lists, spx(SpaceRole::Xs) / 2);
    if (s.machines_open()) {
        auto* menu = surface(Surface::Popup, "machines");
        auto* ml = column(menu, spx(SpaceRole::Xs) / 2);
        std::vector<sidebar::NodeFilter> filters{sidebar::NodeFilter{}};
        for (const auto& n : s.nodes().list()) filters.push_back(sidebar::NodeFilter{n.key});
        for (const auto& f : filters) {
            const auto d = s.describe(f);
            const auto [count, waiting] = s.tally(f);
            const auto tally = waiting > 0 ? std::format("{} need you", waiting) : std::format("{} sessions", count);
            ml->addWidget(kw::two_line_row(d.name, d.sub + " · " + tally, f == s.filter(), [this, f] { send_sidebar(sidebar::msg::PickMachine{f}); }));
        }
        ml->addWidget(kw::text_button("Manage peers", kit::ButtonKind::ghost(), [this] { send_sidebar(sidebar::msg::OpenPeers{}); }));
        l->addWidget(menu);
    }
    l->addWidget(kw::section_header("Projects", {kw::icon_button(Icon::Plus, "New project", [this] {
                                        send_sidebar(sidebar::msg::Emit{is::NewProject{}});
                                    })}));
    const auto session_row = [&](const sidebar::SessionSummary& summary) {
        const bool selected = !s.shown_project && s.active() == summary.id;
        std::string title = summary.title;
        if (summary.waiting) title += " · Needs you";
        else if (s.row_running(summary)) title += " · Running";
        if (!s.filter().node) title += std::format(" · {}", s.nodes().name_of(summary.node));
        auto* r = new QWidget;
        r->setAccessibleName(qs(summary.title));
        auto* rl = row(r);
        const auto id = summary.id;
        rl->addWidget(kw::menu_row(std::nullopt, title, selected, [this, id] { send_sidebar(sidebar::msg::OpenSession{id}); }), 1);
        rl->addWidget(kw::icon_button(Icon::Folder, "Move to project", [this, id] { send_sidebar(sidebar::msg::ToggleMoveMenu{id}); }));
        rl->addWidget(kw::icon_button(Icon::Pin, summary.pinned ? "Unpin" : "Pin", [this, id] { send_sidebar(sidebar::msg::TogglePin{id}); }));
        rl->addWidget(kw::icon_button(Icon::X, "Delete", [this, id] { send_sidebar(sidebar::msg::Emit{is::DeleteSession{id}}); }));
        l->addWidget(r);
        if (s.move_menu() != summary.id) return;
        const auto choice = [&](std::string_view name, std::optional<std::string> project) {
            const bool current = summary.project == project;
            l->addWidget(kw::menu_row(current ? std::optional(Icon::Check) : std::nullopt, name, current,
                                      [this, id, project] { send_sidebar(sidebar::msg::MoveTo{id, project}); }));
        };
        choice("No project", std::nullopt);
        for (const auto& p : s.all_projects()) choice(p, p);
    };
    const auto projects = s.projects();
    if (projects.empty()) l->addWidget(kw::label(s.all_projects().empty() ? "No projects yet" : "No match", TextRole::Label, Tone::Faint));
    const auto highlighted = s.highlighted_project();
    for (const auto project : projects) {
        const std::string p(project);
        const bool on = highlighted == project;
        auto* r = new QWidget;
        r->setAccessibleName(qs(project));
        auto* rl = row(r);
        rl->addWidget(kw::menu_row(Icon::Folder, project, on, [this, p] { send(layout::open_project(p)); }), 1);
        rl->addWidget(kw::icon_button(Icon::Plus, "New session here", [this, p] { send_sidebar(sidebar::msg::Emit{is::NewSessionIn{p}}); }));
        rl->addWidget(kw::icon_button(Icon::Pin, s.pinned_projects().contains(project) ? "Unpin" : "Pin",
                                      [this, p] { send_sidebar(sidebar::msg::ToggleProjectPin{p}); }));
        rl->addWidget(kw::icon_button(Icon::X, "Delete project", [this, p] { send_sidebar(sidebar::msg::Emit{is::DeleteProject{p}}); }));
        l->addWidget(r);
        if (!on) continue;
        const auto [shown, more] = s.project_sessions(project);
        if (shown.empty()) l->addWidget(kw::label("No sessions yet", TextRole::Label, Tone::Faint));
        for (const auto* session : shown) session_row(*session);
        if (more > 0)
            l->addWidget(kw::menu_row(std::nullopt, std::format("All {} sessions", shown.size() + more), false, [this, p] { send(layout::open_project(p)); }));
    }
    const auto pinned = s.pinned();
    if (!pinned.empty()) {
        l->addWidget(kw::label("Pinned", TextRole::Caption, Tone::Faint));
        for (const auto* session : pinned) session_row(*session);
    }
    l->addWidget(kw::label("Recents", TextRole::Caption, Tone::Faint));
    const auto recents = s.recents();
    if (recents.empty()) l->addWidget(kw::label(s.query().empty() ? "No sessions on this machine" : "No match", TextRole::Label, Tone::Faint));
    const auto limit = s.recents_limit();
    for (std::size_t i = 0; i < recents.size() && i < limit; ++i) session_row(*recents[i]);
    if (recents.size() > limit) {
        const auto hidden = std::min(recents.size() - limit, sidebar::RECENTS_PAGE);
        l->addWidget(kw::menu_row(std::nullopt, std::format("Show {} more", hidden), false, [this] { send_sidebar(sidebar::msg::ShowMoreRecents{}); }));
    }
    l->addStretch(1);
    auto* scroll = new QScrollArea;
    scroll->setAccessibleName(QStringLiteral("sidebar scroll"));
    scroll->setFrameShape(QFrame::NoFrame);
    scroll->setWidgetResizable(true);
    scroll->setVerticalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    scroll->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    scroll->setWidget(lists);
    col->addWidget(scroll, 1);

    // Footer: updates, usage, configs, settings.
    if (s.updates() > 0)
        col->addWidget(kw::menu_row(Icon::ArrowUp, std::format("Updates {}", s.updates()), false, [this] { send_sidebar(sidebar::msg::Emit{is::OpenUpdates{}}); }));
    std::string usage = "Usage";
    if (const auto& u = s.usage()) usage += std::format(" {} {:.0f}%", u->label, std::clamp(u->fraction, 0.0F, 1.0F) * 100.0F);
    col->addWidget(kw::menu_row(Icon::Gauge, usage, false, [this] { send_sidebar(sidebar::msg::Emit{is::OpenUsage{}}); }));
    if (const auto& u = s.usage()) col->addWidget(kw::meter(std::clamp(u->fraction, 0.0F, 1.0F)));
    col->addWidget(kw::menu_row(Icon::SlidersHorizontal, "Configs", false, [this] { send_sidebar(sidebar::msg::Emit{is::OpenConfigs{}}); }));
    col->addWidget(kw::menu_row(Icon::Settings, "Settings", false, [this] { send_sidebar(sidebar::msg::Emit{is::OpenSettings{}}); }));
    return root;
}

QWidget* ShellView::build_topbar(const layout::State& frame) {
    auto* bar = surface(Surface::Ground, "top bar");
    bar->setFixedHeight(px(topbar::HEIGHT));
    auto* l = row(bar, spx(SpaceRole::S));
    l->setContentsMargins(spx(SpaceRole::M), 0, spx(SpaceRole::M), 0);
    const auto chevron = [] { return icon_label(Icon::ChevronRight, TextRole::Label, Tone::Muted); };
    const auto bold = [](std::string_view s) {
        auto* t = kw::label(s, TextRole::Body, Tone::Normal);
        auto f = t->font();
        f.setBold(true);
        t->setFont(f);
        return t;
    };
    const auto crumb = frame.breadcrumb();
    if (const auto& project = frame.sidebar().shown_project) {
        l->addWidget(kw::label("Projects", TextRole::Body, Tone::Muted));
        l->addWidget(chevron());
        l->addWidget(bold(*project));
    } else if (!crumb) {
        l->addWidget(kw::label("New session", TextRole::Body, Tone::Muted));
    } else {
        const auto& [session, node] = *crumb;
        if (session->project) {
            const auto place = node.local ? *session->project : std::format("{} on {}", *session->project, node.name);
            const auto p = *session->project;
            l->addWidget(kw::text_button(place, kit::ButtonKind::ghost(), [this, p] { send(layout::open_project(p)); }));
            l->addWidget(chevron());
        } else if (!node.local) {
            l->addWidget(kw::label(std::format("on {}", node.name), TextRole::Body, Tone::Muted));
            l->addWidget(chevron());
        }
        l->addWidget(bold(session->title));
        if (session->waiting) l->addWidget(kw::label("· Needs you", TextRole::Label, Tone::Warn));
    }
    l->addStretch(1);
    if (const auto& status = frame.session_status()) l->addWidget(kw::label(*status, TextRole::Caption, Tone::Faint));
    l->addWidget(kw::icon_button(Icon::PanelRight, frame.panel_open() ? "Hide panel" : "Show panel", [this] { send(layout::msg::TogglePanel{}); }));
    const bool full = frame.full() == layout::Pane::Center;
    l->addWidget(kw::icon_button(full ? Icon::Minimize : Icon::Maximize, full ? "Exit full screen" : "Full screen chat",
                                 [this] { send(layout::msg::ToggleFull{layout::Pane::Center}); }));
    return bar;
}

QWidget* ShellView::build_project_tabs(const layout::State& frame) {
    auto* strip = surface(Surface::Side, "project tabs");
    strip->setFixedHeight(px(layout::PROJECT_TABS_HEIGHT));
    auto* l = row(strip);
    l->setContentsMargins(spx(SpaceRole::S), 0, spx(SpaceRole::S), 0);
    const auto& active = frame.sidebar().active_project;
    std::vector<kw::Tab> tabs;
    for (const auto& p : frame.open_projects()) {
        auto* close = kw::icon_button(Icon::X, "Close tab", [this, p] { send(layout::msg::Emit{in::CloseProjectTab{p}}); });
        tabs.push_back(kw::Tab{p, std::nullopt, active == p, [this, p] { send(layout::open_project(p)); }, close});
    }
    l->addWidget(kw::tab_strip(tabs), 1);
    return strip;
}

QWidget* ShellView::build_panel(const layout::State& frame) {
    auto* panel = surface(Surface::Side, "panel");
    auto* col = column(panel);
    auto* bar = new QWidget;
    bar->setAccessibleName(QStringLiteral("panel tabs"));
    bar->setFixedHeight(px(topbar::HEIGHT));
    auto* bl = row(bar, spx(SpaceRole::Xs));
    bl->setContentsMargins(spx(SpaceRole::S), 0, spx(SpaceRole::S), 0);
    const auto active = frame.active_tab();
    std::vector<kw::Tab> tabs;
    for (const auto& t : frame.tabs()) {
        QWidget* trailing = frame.is_detached(t.id) ? icon_label(Icon::ExternalLink, TextRole::Caption, Tone::Muted) : nullptr;
        const auto id = t.id;
        tabs.push_back(kw::Tab{t.title, t.icon, active == t.id, [this, id] { send(layout::msg::SelectTab{id}); }, trailing});
    }
    bl->addWidget(kw::tab_strip(tabs), 1);
    if (active && !frame.is_detached(*active)) {
        const std::string id(*active);
        auto* detach = kw::icon_button(Icon::ExternalLink, "Open in a window", [this, id] { send(layout::msg::Emit{in::DetachTab{id}}); });
        detach->setObjectName(qs(layout::PANEL_DETACH_ID));
        bl->addWidget(detach);
    }
    const bool is_full = frame.full() == layout::Pane::Panel;
    bl->addWidget(kw::icon_button(is_full ? Icon::Minimize : Icon::Maximize, is_full ? "Restore panel" : "Fill window",
                                  [this] { send(layout::msg::ToggleFull{layout::Pane::Panel}); }));
    col->addWidget(bar);

    panel_slot_ = new QWidget;
    panel_slot_->setAccessibleName(QStringLiteral("panel body"));
    auto* body = column(panel_slot_, spx(SpaceRole::M));
    if (active && frame.is_detached(*active)) {
        const std::string id(*active);
        const auto it = std::ranges::find(frame.tabs(), *active, &layout::PanelTab::id);
        const std::string title = it != frame.tabs().end() ? it->title : id;
        body->addStretch(1);
        body->addWidget(kw::label(title + " is in its own window", TextRole::Body, Tone::Normal), 0, Qt::AlignHCenter);
        auto* actions = new QWidget;
        actions->setAccessibleName(QStringLiteral("detached actions"));
        auto* al = row(actions, spx(SpaceRole::S));
        al->addWidget(kw::text_button("Show window", kit::ButtonKind::secondary(), [this, id] { send(layout::msg::Emit{in::ShowDetached{id}}); }));
        al->addWidget(kw::text_button("Reattach here", kit::ButtonKind::primary(), [this, id] { send(layout::msg::Emit{in::ReattachTab{id}}); }));
        body->addWidget(actions, 0, Qt::AlignHCenter);
        body->addStretch(1);
    } else if (active && panel_body_) {
        body->addWidget(panel_body_, 1);
    } else if (!active) {
        body->addWidget(kw::label("Nothing to show", TextRole::Body, Tone::Muted), 0, Qt::AlignHCenter | Qt::AlignTop);
        body->addStretch(1);
    }
    col->addWidget(panel_slot_, 1);
    return panel;
}

// ---- PaletteView ---------------------------------------------------------------------------

PaletteView::PaletteView(QWidget* parent) : QWidget(parent) {
    setObjectName(QStringLiteral("palette"));
    setAccessibleName(QStringLiteral("Command palette"));
    setAttribute(Qt::WA_StyledBackground);
    kit::qt::style_container(*this, Surface::Popup);
    setFixedWidth(px(kit::theme::width(kit::theme::LayoutRole::PopupWidth)));
    auto* col = column(this);
    auto* search = new QWidget;
    search->setAccessibleName(QStringLiteral("palette search"));
    auto* sl = row(search, spx(SpaceRole::S));
    sl->setContentsMargins(spx(SpaceRole::M), 0, spx(SpaceRole::M), 0);
    sl->addWidget(icon_label(Icon::Search, TextRole::Body, Tone::Muted));
    input_ = new QLineEdit;
    input_->setObjectName(qs(palette::INPUT_ID));
    input_->setAccessibleName(QStringLiteral("Type a command"));
    input_->setFrame(false);
    input_->installEventFilter(this);
    connect(input_, &QLineEdit::textEdited, this, [this](const QString& t) { emit message(palette::msg::QueryChanged{t.toStdString()}); });
    sl->addWidget(input_, 1);
    col->addWidget(search);
    scroll_ = new QScrollArea;
    scroll_->setObjectName(qs(palette::LIST_ID));
    scroll_->setAccessibleName(QStringLiteral("palette results"));
    scroll_->setFrameShape(QFrame::NoFrame);
    scroll_->setWidgetResizable(true);
    list_ = new QWidget;
    list_->setAccessibleName(QStringLiteral("palette rows"));
    rows_ = column(list_);
    scroll_->setWidget(list_);
    connect(scroll_->verticalScrollBar(), &QScrollBar::valueChanged, this,
            [this](int v) { emit message(palette::msg::Scrolled{static_cast<float>(v)}); });
    col->addWidget(scroll_);
}

bool PaletteView::eventFilter(QObject* watched, QEvent* event) {
    if (watched == input_ && event->type() == QEvent::KeyPress) {
        switch (static_cast<QKeyEvent*>(event)->key()) {
        case Qt::Key_Up: emit message(palette::msg::SelectPrevious{}); return true;
        case Qt::Key_Down: emit message(palette::msg::SelectNext{}); return true;
        case Qt::Key_Return:
        case Qt::Key_Enter: emit message(palette::msg::Submit{}); return true;
        case Qt::Key_Escape: emit message(palette::msg::Close{}); return true;
        default: break;
        }
    }
    return QWidget::eventFilter(watched, event);
}

void PaletteView::render(const palette::State& state) {
    setVisible(state.is_open());
    input_->setPlaceholderText(state.shortcuts_only() ? QStringLiteral("Search keyboard shortcuts") : QStringLiteral("Type a command"));
    if (input_->text().toStdString() != state.query()) input_->setText(qs(state.query()));
    clear(rows_);
    if (state.rows().empty()) {
        rows_->addWidget(kw::label("No matching commands", TextRole::Body, Tone::Muted));
        scroll_->setFixedHeight(px(palette::ROW_HEIGHT));
        return;
    }
    const auto& rows = state.rows();
    for (std::size_t i = 0; i < rows.size(); ++i) {
        const auto& r = rows[i];
        const auto& c = state.command_of(r);
        if (r.starts_section) {
            auto* head = kw::label(c.section, TextRole::Caption, Tone::Muted);
            head->setFixedHeight(px(palette::HEADER_HEIGHT));
            head->setAlignment(Qt::AlignBottom | Qt::AlignLeft);
            rows_->addWidget(head);
        }
        // Matched runs in bold.
        QString html;
        for (const auto& run : r.runs) {
            const auto piece = qs(std::string_view(c.title).substr(run.begin, run.end - run.begin)).toHtmlEscaped();
            html += run.matched ? QStringLiteral("<b>%1</b>").arg(piece) : piece;
        }
        QWidget* w = nullptr;
        QHBoxLayout* content = nullptr;
        if (c.enabled) {
            auto* button = new QPushButton;
            button->setAccessibleName(qs(c.title));
            button->setObjectName(QStringLiteral("palette row"));
            kit::qt::style_button(*button, kit::ButtonKind::row(state.selected() == i));
            connect(button, &QPushButton::clicked, this, [this, i] { emit message(palette::msg::Activate{i}); });
            content = row(button, spx(SpaceRole::S));
            w = button;
        } else {
            // A row that can't run is static text, so it never reads as a broken control.
            w = new QWidget;
            w->setAccessibleName(qs(c.title));
            content = row(w, spx(SpaceRole::S));
        }
        auto* title = kw::label(c.title, TextRole::Body, c.enabled ? Tone::Normal : Tone::Muted);
        title->setTextFormat(Qt::RichText);
        title->setText(html);
        content->addWidget(title, 1);
        if (r.hint) content->addWidget(kw::label(*r.hint, TextRole::Caption, Tone::Muted));
        w->setFixedHeight(px(palette::ROW_HEIGHT));
        rows_->addWidget(w);
    }
    rows_->addStretch(1);
    scroll_->setFixedHeight(px(state.list_height()));
}

void PaletteView::apply(const Task& task) {
    for (const auto& o : task.ops) {
        if (const auto* f = std::get_if<op::Focus>(&o); f && f->id == palette::INPUT_ID) input_->setFocus();
        if (const auto* s = std::get_if<op::ScrollTo>(&o); s && s->id == palette::LIST_ID && s->y) {
            const QSignalBlocker block(scroll_->verticalScrollBar());
            scroll_->verticalScrollBar()->setValue(px(*s->y));
        }
    }
}

} // namespace loa::ui::shell
