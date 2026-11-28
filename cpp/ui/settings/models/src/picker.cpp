// Port of `State` in `crates/loa-ui-settings/src/models/picker.rs`.

#include "loa/ui/settings/models/picker.hpp"

#include <algorithm>
#include <cctype>
#include <ranges>

#include <Qt>

namespace loa::ui::settings::models::picker {

namespace {

std::string lower(std::string_view s) {
    std::string out(s);
    for (auto& c : out) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    return out;
}

// Whether every whitespace separated term of `query` appears in `hay`.
bool matches(std::string_view query, std::string_view hay) {
    std::size_t i = 0;
    while (i < query.size()) {
        while (i < query.size() && std::isspace(static_cast<unsigned char>(query[i]))) ++i;
        const auto start = i;
        while (i < query.size() && !std::isspace(static_cast<unsigned char>(query[i]))) ++i;
        if (i > start && hay.find(query.substr(start, i - start)) == std::string_view::npos) return false;
    }
    return true;
}

} // namespace

std::pair<State, shell::Task> State::open(Catalog catalog, Target target, std::optional<ModelRef> current) {
    State s;
    s.catalog_ = std::move(catalog);
    s.target_ = std::move(target);
    s.current_ = std::move(current);
    std::vector<std::string_view> providers;
    for (const auto& m : s.catalog_.models) {
        auto it = std::ranges::find(providers, std::string_view(m.model.provider));
        if (it == providers.end()) {
            providers.push_back(m.model.provider);
            s.provider_rank_.push_back(providers.size() - 1);
        } else {
            s.provider_rank_.push_back(static_cast<std::size_t>(it - providers.begin()));
        }
        std::string hay = m.model.provider + " " + m.provider_name + " " + m.model.id + " " + m.name;
        for (const auto& r : s.catalog_.roles)
            if (r.model == m.model) hay += " " + r.role;
        s.haystacks_.push_back(lower(hay));
    }
    s.refilter();
    s.selected_ = 0;
    if (s.current_) {
        for (std::size_t i = 0; i < s.rows_.size(); ++i) {
            const auto& r = s.rows_[i];
            if (r.kind == Row::Kind::Model && s.catalog_.models[r.index].model == *s.current_) {
                s.selected_ = i;
                break;
            }
        }
    }
    return {std::move(s), shell::Task::of(shell::op::Focus{std::string(SEARCH_ID)})};
}

std::optional<std::size_t> State::selected() const noexcept {
    if (rows_.empty()) return std::nullopt;
    return selected_;
}

std::vector<Group> State::groups() const {
    std::vector<Group> groups;
    for (std::size_t i = 0; i < rows_.size(); ++i) {
        const auto& row = rows_[i];
        const std::string_view title =
            row.kind == Row::Kind::Role ? std::string_view("Roles") : std::string_view(catalog_.models[row.index].provider_display_name());
        if (!groups.empty() && groups.back().title == title) {
            auto& g = groups.back();
            g.rows = std::span<const Row>(rows_).subspan(g.start, i - g.start + 1);
        } else {
            groups.push_back({title, i, std::span<const Row>(rows_).subspan(i, 1)});
        }
    }
    return groups;
}

std::vector<std::string_view> State::roles_of(const ModelRef& model) const {
    std::vector<std::string_view> out;
    for (const auto& r : catalog_.roles)
        if (r.model == model) out.emplace_back(r.role);
    return out;
}

shell::Task State::scroll() const {
    const float y = rows_.size() < 2 ? 0.0f : static_cast<float>(selected_) / static_cast<float>(rows_.size() - 1);
    return shell::Task::of(shell::op::SnapTo{std::string(LIST_ID), std::nullopt, y});
}

std::optional<Message> State::key(int qt_key) {
    switch (qt_key) {
    case Qt::Key_Up: return Message::local(local::Up{});
    case Qt::Key_Down: return Message::local(local::Down{});
    case Qt::Key_Escape: return Message::event(loa::view::settings::models::picker::intent::Close{});
    default: return std::nullopt;
    }
}

std::optional<Intent> State::pick(std::size_t index) const {
    namespace vi = loa::view::settings::models::picker::intent;
    if (index >= rows_.size()) return std::nullopt;
    const auto row = rows_[index];
    if (std::holds_alternative<loa::view::settings::models::picker::target::Session>(target_)) {
        if (row.kind == Row::Kind::Role) {
            const auto& role = catalog_.roles[row.index];
            return vi::SetSessionModel{{role.model, role.role}};
        }
        return vi::SetSessionModel{{catalog_.models[row.index].model, std::nullopt}};
    }
    // Role rows exist only for the session target.
    if (row.kind == Row::Kind::Role) return std::nullopt;
    const auto& role = std::get<loa::view::settings::models::picker::target::Role>(target_);
    return vi::SetRoleModel{role.role, catalog_.models[row.index].model};
}

void State::refilter() {
    const auto query = lower(query_);
    rows_.clear();
    if (std::holds_alternative<loa::view::settings::models::picker::target::Session>(target_)) {
        for (std::size_t i = 0; i < catalog_.roles.size(); ++i) {
            const auto& r = catalog_.roles[i];
            if (matches(query, lower(r.role + " " + r.model.selector()))) rows_.push_back(Row::role(i));
        }
    }
    std::vector<std::size_t> models;
    for (std::size_t i = 0; i < haystacks_.size(); ++i)
        if (matches(query, haystacks_[i])) models.push_back(i);
    std::ranges::stable_sort(models, {}, [this](std::size_t i) { return provider_rank_[i]; });
    for (auto i : models) rows_.push_back(Row::model(i));
}

std::optional<Intent> State::reduce(Local m) {
    return std::visit(
        [this](auto&& l) -> std::optional<Intent> {
            using T = std::decay_t<decltype(l)>;
            if constexpr (std::is_same_v<T, local::Query>) {
                query_ = std::move(l.text);
                refilter();
                selected_ = 0;
            } else if constexpr (std::is_same_v<T, local::Up>) {
                selected_ = selected_ == 0 ? 0 : selected_ - 1;
            } else if constexpr (std::is_same_v<T, local::Down>) {
                selected_ = std::min(selected_ + 1, rows_.empty() ? std::size_t{0} : rows_.size() - 1);
            } else if constexpr (std::is_same_v<T, local::Submit>) {
                return pick(selected_);
            } else {
                return pick(l.index);
            }
            return std::nullopt;
        },
        std::move(m));
}

bool State::guard(const Intent& e) const { return std::holds_alternative<loa::view::settings::models::picker::intent::Close>(e); }

} // namespace loa::ui::settings::models::picker
