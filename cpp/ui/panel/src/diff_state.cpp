// The diff tab state. Port of `crates/loa-ui-panel/src/diff/mod.rs` (`State`).

#include <algorithm>
#include <cmath>

#include "loa/ui/kit/theme.hpp"
#include "loa/ui/panel/diff.hpp"

namespace loa::ui::panel::diff {

namespace theme = loa::ui::kit::theme;

float row_height() { return theme::text_size(theme::TextRole::Mono) + 2.0F * theme::sp(theme::SpaceRole::Xs); }

Task State::set_note(std::optional<std::string> note) {
    note_ = std::move(note);
    return note_ ? set_files({}) : Task::none();
}

Task State::set_files(std::vector<FileDiff> files) {
    files_ = std::move(files);
    return select(0);
}

const FileDiff* State::selected() const noexcept { return selected_ < files_.size() ? &files_[selected_] : nullptr; }

Range State::visible_rows() const {
    const auto rh = row_height();
    const auto first = static_cast<std::size_t>(std::max(std::floor(scroll_offset_ / rh), 0.0F));
    const auto shown = static_cast<std::size_t>(std::max(std::ceil(viewport_height_ / rh), 1.0F));
    const auto len = rows_.size();
    return {std::min(first > OVERSCAN ? first - OVERSCAN : 0, len), std::min(first + shown + OVERSCAN, len)};
}

std::pair<Task, std::optional<Event>> State::update(Message message) {
    if (const auto* m = std::get_if<msg::SelectFile>(&message)) {
        if (m->index < files_.size() && m->index != selected_) return {select(m->index), std::nullopt};
    } else if (const auto* m = std::get_if<msg::SetMode>(&message)) {
        if (m->mode != mode_) {
            // Rows map to a similar fraction in both modes, so the same part stays in view.
            const auto rh = row_height();
            const auto fraction = scroll_offset_ / (static_cast<float>(std::max<std::size_t>(rows_.size(), 1)) * rh);
            mode_ = m->mode;
            rebuild_rows();
            scroll_offset_ = fraction * static_cast<float>(rows_.size()) * rh;
        }
    } else if (const auto* m = std::get_if<msg::Expand>(&message)) {
        if (selected_ < files_.size() && files_[selected_].expand(m->fold)) rebuild_rows();
    } else if (const auto* m = std::get_if<msg::Scrolled>(&message)) {
        scroll_offset_ = m->offset;
        viewport_height_ = m->height;
    } else if (std::holds_alternative<msg::Open>(message)) {
        if (const auto* f = selected()) return {Task::none(), Event{loa::view::panel::diff_intent::OpenFile{std::string(f->path())}}};
    } else if (std::holds_alternative<msg::ToggleCollapsed>(message)) {
        collapsed_ = !collapsed_;
    }
    return {Task::none(), std::nullopt};
}

Task State::select(std::size_t index) {
    selected_ = index;
    collapsed_ = false;
    scroll_offset_ = 0;
    rebuild_rows();
    return Task::of(loa::ui::shell::op::SnapTo{std::string(SCROLL_ID), 0.0F, 0.0F});
}

void State::rebuild_rows() {
    const auto* f = selected();
    rows_ = !f ? std::vector<Row>{} : mode_ == Mode::Unified ? f->unified_rows() : f->split_rows();
}

} // namespace loa::ui::panel::diff
