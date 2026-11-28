#include "loa/ui/shell/layout.hpp"

#include <algorithm>
#include <limits>

namespace loa::ui::kit::widgets {
// From `widgets.hpp`, declared here so state code stays free of Qt headers.
float splitter_width();
} // namespace loa::ui::kit::widgets

namespace loa::ui::shell::layout {

namespace {
template <class... F> struct Overloaded : F... {
    using F::operator()...;
};
namespace is = loa::view::shell::sidebar_intent;
namespace in = loa::view::shell::intent;
using kit::widgets::splitter_width;
} // namespace

Message open_project(std::string project) {
    return msg::Emit{in::Sidebar{is::OpenProject{std::move(project)}}};
}

std::pair<State, Task> State::make(loa::view::theme::Appearance appearance) {
    return {State(std::move(appearance)), Task::of(op::QuerySystemTheme{})};
}

std::optional<Intent> State::set_appearance(loa::view::theme::Appearance appearance) {
    if (theme_.appearance() == appearance) return std::nullopt;
    return in::ThemeChanged{theme_.set_appearance(std::move(appearance))};
}

bool State::is_detached(std::string_view tab) const { return std::ranges::find(detached_, tab) != detached_.end(); }

std::optional<std::pair<const SessionSummary*, NodeStatus>> State::breadcrumb() const {
    if (!sidebar_.active()) return std::nullopt;
    const auto* s = sidebar_.session(*sidebar_.active());
    if (!s) return std::nullopt;
    const auto& nodes = sidebar_.nodes();
    if (const auto* n = nodes.get(s->node)) return std::pair{s, *n};
    return std::pair{s, NodeStatus{s->node, std::string(nodes.name_of(s->node)), false, false}};
}

void State::add_tab(PanelTab tab) {
    const auto it = std::ranges::find(tabs_, tab.id, &PanelTab::id);
    if (it != tabs_.end()) *it = std::move(tab);
    else tabs_.push_back(std::move(tab));
}

void State::show_tab(std::string_view id) {
    if (std::ranges::find(tabs_, id, &PanelTab::id) == tabs_.end()) return;
    active_ = std::string(id);
    set_panel_open(true);
}

Task State::reveal_active_tab() const {
    std::size_t index = 0;
    if (const auto a = active_tab()) index = static_cast<std::size_t>(std::ranges::find(tabs_, *a, &PanelTab::id) - tabs_.begin());
    const float x = tabs_.size() < 2 ? 0.0F : static_cast<float>(index) / static_cast<float>(tabs_.size() - 1);
    return Task::of(op::SnapTo{std::string(PANEL_TABS_ID), x, std::nullopt});
}

std::optional<std::string_view> State::active_tab() const {
    if (active_ && std::ranges::find(tabs_, *active_, &PanelTab::id) != tabs_.end()) return *active_;
    if (!tabs_.empty()) return tabs_.front().id;
    return std::nullopt;
}

void State::set_panel_open(bool open) {
    panel_open_ = open;
    if (dragging_ == Pane::Panel) dragging_.reset();
    if (!open && full_ == Pane::Panel) full_.reset();
}

float State::max_panel_width() const {
    if (!frame_) return std::numeric_limits<float>::infinity();
    return std::max(frame_->width - sidebar_width_ - MIN_CENTER_WIDTH - 2.0F * splitter_width(), MIN_PANEL_WIDTH);
}

float State::shown_panel_width() const { return std::min(panel_width_, max_panel_width()); }

std::pair<Task, std::optional<Event>> State::update(Message message) {
    using R = std::pair<Task, std::optional<Event>>;
    return std::visit(
        Overloaded{
            [&](msg::Sidebar& m) -> R {
                auto [task, event] = sidebar_.update(std::move(m.message));
                if (event) return {std::move(task), Intent{in::Sidebar{std::move(*event)}}};
                return {std::move(task), std::nullopt};
            },
            [&](msg::TabsWheel& m) -> R { return {wheel_tabs(m.id, m.delta), std::nullopt}; },
            [&](msg::Emit& m) -> R { return {Task::none(), std::move(m.intent)}; },
            [&](msg::SetTheme& m) -> R {
                theme_.set_preference(m.theme);
                return {Task::none(), Intent{in::SetTheme{std::move(m.theme)}}};
            },
            [&](msg::SystemTheme m) -> R {
                const auto before = theme_.variant();
                const auto generation = theme_.set_system_mode(m.mode);
                if (theme_.variant() == before) return {};
                return {Task::none(), Intent{in::ThemeChanged{generation}}};
            },
            [&](msg::SelectTab& m) -> R {
                if (std::ranges::find(tabs_, m.id, &PanelTab::id) != tabs_.end()) active_ = std::move(m.id);
                return {};
            },
            [&](msg::DragStart m) -> R {
                if (m.pane != Pane::Panel || panel_open_) dragging_ = m.pane;
                else dragging_.reset();
                return {};
            },
            [&](msg::DragMove m) -> R {
                if (dragging_ == Pane::Panel && frame_) {
                    panel_width_ = std::max(std::min(frame_->width - m.x - splitter_width() / 2.0F, max_panel_width()), MIN_PANEL_WIDTH);
                } else if (dragging_ == Pane::Sidebar) {
                    sidebar_width_ = std::clamp(m.x - splitter_width() / 2.0F, SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                }
                return {};
            },
            [&](msg::DragEnd) -> R {
                dragging_.reset();
                return {};
            },
            [&](msg::TogglePanel) -> R {
                set_panel_open(!panel_open_);
                return {};
            },
            [&](msg::ToggleFull m) -> R {
                if (full_ != m.pane) full_ = m.pane;
                else full_.reset();
                return {};
            },
            [&](msg::FrameSized m) -> R {
                frame_ = m.size;
                return {};
            },
        },
        message);
}

Task wheel_tabs(std::string_view id, ScrollDelta delta) {
    const float x = delta.unit == ScrollDelta::Unit::Lines ? -delta.y * WHEEL_LINE : -delta.y;
    if (x == 0.0F) return Task::none();
    return Task::of(op::ScrollBy{std::string(id), x, 0.0F});
}

} // namespace loa::ui::shell::layout
