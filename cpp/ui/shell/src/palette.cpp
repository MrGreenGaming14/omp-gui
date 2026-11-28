#include "loa/ui/shell/palette.hpp"

#include <algorithm>
#include <limits>
#include <numeric>
#include <tuple>

namespace loa::ui::kit::widgets {
// From `virtual_list.hpp`, declared here so state code stays free of Qt headers.
std::optional<float> scroll_target(float offset, float height, float top, float bottom);
} // namespace loa::ui::kit::widgets

namespace loa::ui::shell::palette {

namespace {
template <class... F> struct Overloaded : F... {
    using F::operator()...;
};
} // namespace

State::State(Platform platform) : platform_(platform) {}

void State::set_commands(std::vector<Command> commands) {
    commands_ = std::move(commands);
    refilter();
}

std::vector<const Command*> State::entries() const {
    std::vector<const Command*> out;
    out.reserve(rows_.size());
    for (const auto& r : rows_) out.push_back(&commands_[r.command]);
    return out;
}

std::optional<std::string> State::hint(const Command& c) const {
    if (c.keys) return c.keys;
    if (c.shortcut) return c.shortcut->label(platform_);
    return std::nullopt;
}

float State::list_height() const {
    float sum = 0;
    for (const auto& r : rows_) sum += r.height();
    return std::min(sum, MAX_LIST_HEIGHT);
}

std::pair<Task, std::optional<Event>> State::update(Message message) {
    using R = std::pair<Task, std::optional<Event>>;
    const bool was_open = open_;
    const auto close = [&]() -> R {
        open_ = false;
        return {Task::none(), was_open ? std::optional<Event>(event::Closed{}) : std::nullopt};
    };
    return std::visit(
        Overloaded{
            [&](msg::Open) -> R { return {open(false), std::nullopt}; },
            [&](msg::OpenShortcuts) -> R { return {open(true), std::nullopt}; },
            [&](msg::Toggle) -> R { return was_open ? close() : R{open(false), std::nullopt}; },
            [&](msg::Close) -> R { return close(); },
            [&](msg::Scrolled s) -> R {
                scroll_offset_ = s.y;
                return {Task::none(), std::nullopt};
            },
            [&](msg::QueryChanged& q) -> R {
                if (!was_open) return {};
                query_ = std::move(q.query);
                refilter();
                return {scroll_to(0), std::nullopt};
            },
            [&](msg::SelectNext) -> R { return was_open ? R{step(true), std::nullopt} : R{}; },
            [&](msg::SelectPrevious) -> R { return was_open ? R{step(false), std::nullopt} : R{}; },
            [&](msg::Submit) -> R {
                if (!was_open || !selected_) return {};
                return {Task::none(), run(*selected_)};
            },
            [&](msg::Activate a) -> R { return was_open ? R{Task::none(), run(a.row)} : R{}; },
        },
        message);
}

Task State::open(bool shortcuts_only) {
    shortcuts_only_ = shortcuts_only;
    open_ = true;
    query_.clear();
    refilter();
    return Task::batch({Task::of(op::Focus{std::string(INPUT_ID)}), scroll_to(0)});
}

Task State::scroll_to(float y) {
    scroll_offset_ = y;
    return Task::of(op::ScrollTo{std::string(LIST_ID), std::nullopt, y});
}

std::optional<Event> State::run(std::size_t index) {
    if (index >= rows_.size()) return std::nullopt;
    const auto& c = commands_[rows_[index].command];
    if (!c.enabled) return std::nullopt;
    auto id = c.id;
    open_ = false;
    std::erase(recent_, id);
    recent_.insert(recent_.begin(), id);
    if (recent_.size() > MAX_RECENT) recent_.resize(MAX_RECENT);
    return event::Run{std::move(id)};
}

// Moves to the next (or previous) enabled row, wrapping, and scrolls it into view.
Task State::step(bool forward) {
    const auto n = rows_.size();
    if (n == 0) return Task::none();
    const auto start = selected_ ? (forward ? *selected_ + 1 : *selected_ + n - 1) : (forward ? 0 : n - 1);
    std::optional<std::size_t> next;
    for (std::size_t k = 0; k < n; ++k) {
        const auto i = forward ? (start + k) % n : (start + n - k) % n;
        if (commands_[rows_[i].command].enabled) {
            next = i;
            break;
        }
    }
    if (!next) return Task::none();
    selected_ = next;
    // A section's header counts as part of its first row, so scrolling up shows it.
    float top = 0;
    for (std::size_t i = 0; i < *next; ++i) top += rows_[i].height();
    const auto target = kit::widgets::scroll_target(scroll_offset_, list_height(), top, top + rows_[*next].height());
    return target ? scroll_to(*target) : Task::none();
}

// Ranks by match class, score, recency and registration, then groups sections by their best result.
void State::refilter() {
    matcher_.set_query(query_);
    struct Ranked {
        std::tuple<kit::text::Class, std::int32_t, std::size_t, std::size_t> key;
        std::vector<std::size_t> indices;
    };
    std::vector<Ranked> ranked;
    for (std::size_t i = 0; i < commands_.size(); ++i) {
        const auto& c = commands_[i];
        if (shortcuts_only_ && !c.keys && !c.shortcut) continue;
        auto found = matcher_.matches(c.title);
        if (!found) continue;
        const auto it = std::ranges::find(recent_, c.id);
        const auto recency = it == recent_.end() ? std::numeric_limits<std::size_t>::max() : static_cast<std::size_t>(it - recent_.begin());
        ranked.push_back({{found->cls, -found->score, recency, i}, std::move(found->indices)});
    }
    std::ranges::sort(ranked, {}, &Ranked::key);
    std::vector<std::string_view> sections;
    struct Keyed {
        std::size_t rank, command;
        std::vector<std::size_t> indices;
    };
    std::vector<Keyed> keyed;
    keyed.reserve(ranked.size());
    for (auto& r : ranked) {
        const auto i = std::get<3>(r.key);
        const std::string_view section = commands_[i].section;
        auto at = std::ranges::find(sections, section);
        if (at == sections.end()) at = sections.insert(sections.end(), section);
        keyed.push_back({static_cast<std::size_t>(at - sections.begin()), i, std::move(r.indices)});
    }
    std::ranges::stable_sort(keyed, {}, &Keyed::rank);
    rows_.clear();
    std::optional<std::size_t> previous;
    for (auto& k : keyed) {
        const auto& entry = commands_[k.command];
        Row row;
        row.command = k.command;
        row.starts_section = previous != k.rank;
        previous = k.rank;
        for (const auto& run : kit::text::highlight_runs(entry.title, k.indices)) row.runs.push_back({run.begin, run.end, run.matched});
        row.hint = hint(entry);
        rows_.push_back(std::move(row));
    }
    selected_.reset();
    for (std::size_t i = 0; i < rows_.size(); ++i) {
        if (commands_[rows_[i].command].enabled) {
            selected_ = i;
            break;
        }
    }
}

} // namespace loa::ui::shell::palette
