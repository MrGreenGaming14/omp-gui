// Qt view of the agents and background jobs panes. Port of `State::view` in
// `crates/loa-ui-panel/src/agents.rs`.

#include "loa/ui/panel/agents_view.hpp"

#include <array>
#include <format>

#include "loa/ui/kit/text.hpp"
#include "pane_widgets.hpp"

namespace loa::ui::panel {

using namespace pw;
using agents::JobKind;
using agents::JobStatus;

AgentsView::AgentsView(QWidget* parent) : QWidget(parent) {
    auto* l = new QVBoxLayout(this);
    l->setContentsMargins(0, 0, 0, 0);
    scroll_ = new QScrollArea(this);
    scroll_->setWidgetResizable(true);
    scroll_->setFrameShape(QFrame::NoFrame);
    scroll_->setAccessibleName(QStringLiteral("agents list"));
    l->addWidget(scroll_);
}

void AgentsView::render(const agents::State& state) {
    namespace m = agents::msg;
    auto* list = new QWidget;
    auto* col = new QVBoxLayout(list);
    col->setContentsMargins(spi(SpaceRole::L), spi(SpaceRole::Xs), spi(SpaceRole::L), spi(SpaceRole::Xs));
    col->setSpacing(spi(SpaceRole::Xs));

    std::size_t running = 0;
    for (const auto& [_, j] : state.jobs()) running += j.running() ? 1 : 0;
    auto* header = hrow({kw::label(std::format("{} running", running), TextRole::Caption, Tone::Muted),
                         state.waiting() && running > 0 ? kw::badge("wait blocked", Tone::Muted) : nullptr},
                        spi(SpaceRole::S));
    stretch(header);
    header->setFixedHeight(px(list_row_height()));
    col->addWidget(header);
    if (state.jobs().empty())
        col->addWidget(kw::label(state.kind() == agents::PaneKind::Agents ? "No subagents" : "No background jobs", TextRole::Label, Tone::Muted));

    for (const auto& [n, job] : state.jobs()) {
        static constexpr std::array<Tone, 4> TONES{Tone::Muted, Tone::Ok, Tone::Danger, Tone::Muted};
        const auto tone = TONES[static_cast<std::size_t>(job.status())];
        const bool can_open = job.kind() == JobKind::Subagent && job.id().has_value();
        const bool has_transcript = job.kind() == JobKind::Subagent && job.session().has_value();
        auto send = [this](agents::Message msg) { return [this, msg] { emit message(msg); }; };

        auto* icon = new QLabel;
        icon->setPixmap(kit::icon(job.kind() == JobKind::Shell ? Icon::Terminal : Icon::Cpu, theme::text_size(TextRole::Body)));
        auto* title = hrow({icon, kw::label(job.label(), TextRole::Body, Tone::Normal)}, spi(SpaceRole::S));
        QWidget* lead = can_open ? content_button(title, job.label(), ButtonKind::row(false), send(m::Open{n})) : title;

        auto* head = hrow({lead, kw::label(loa::core::as_str(job.status()), TextRole::Caption, tone)}, spi(SpaceRole::S));
        if (auto c = job.exit_code(); c && *c != 0 && !job.running()) add(head, kw::label(std::format("exit {}", *c), TextRole::Caption, Tone::Danger));
        if (state.is_awaited(n)) add(head, kw::badge("waiting on", Tone::Muted));
        if (auto ms = state.elapsed_ms(n)) add(head, kw::label(kit::text::format_duration(*ms), TextRole::Caption, Tone::Muted));
        stretch(head);
        if (can_open) add(head, action("Open", send(m::Open{n})));
        if (has_transcript) {
            auto* b = action("Transcript", send(m::Detach{n}));
            b->setToolTip(QStringLiteral("Read-only copy in a window"));
            add(head, b);
        }
        if (job.running()) add(head, action("Cancel", send(m::Cancel{n})));
        if (!job.output().empty()) add(head, action(job.expanded() ? "Collapse" : "Expand", send(m::Toggle{n})));
        head->setMinimumHeight(px(list_row_height()));
        col->addWidget(head);

        if (job.expanded()) {
            const bool full = state.is_full(n);
            const std::string_view out = job.output();
            auto* body = vcol({kw::label(full ? out : out.substr(job.tail_start()), TextRole::Mono, Tone::Normal)}, spi(SpaceRole::S));
            if (job.line_count() > agents::TAIL_LINES) {
                const auto label = full ? std::format("Show last {} lines", agents::TAIL_LINES) : std::format("Show all {} lines", job.line_count());
                add(body, action(label, send(m::ToggleFull{n})));
            }
            col->addWidget(surface(body, Surface::Raised, spi(SpaceRole::S)));
        } else if (const auto last = job.last_line(); !last.empty()) {
            auto* line = hrow({kw::label(last, TextRole::Caption, Tone::Muted)});
            line->layout()->setContentsMargins(spi(SpaceRole::Xl), 0, 0, 0);
            col->addWidget(line);
        }
    }
    col->addStretch(1);
    if (auto* old = scroll_->takeWidget()) {
        old->hide();
        old->deleteLater();
    }
    scroll_->setWidget(list);
}

} // namespace loa::ui::panel
