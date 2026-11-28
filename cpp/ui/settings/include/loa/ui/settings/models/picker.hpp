#pragma once

// The model picker: a searchable popup of the models grouped by provider, showing the roles
// each serves. It picks the session's model (directly or through a role) or a role's model.
// Port of `crates/loa-ui-settings/src/models/picker.rs` (state; the view is `PickerView`).

#include <cstddef>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/kit/widgets.hpp"
#include "loa/ui/shell/task.hpp"
#include "loa/view/settings_models.hpp"

namespace loa::ui::settings::models::picker {

using loa::view::settings::models::Catalog;
using loa::view::settings::models::ModelRef;
using loa::view::settings::models::picker::Intent;
using loa::view::settings::models::picker::Target;

inline constexpr std::string_view SEARCH_ID = "loa-model-picker-search";
inline constexpr std::string_view LIST_ID = "loa-model-picker-list";

// One line of the filtered list: an index into the catalog's roles or models.
struct Row {
    enum class Kind : std::uint8_t { Role, Model };
    Kind kind = Kind::Model;
    std::size_t index = 0;
    static constexpr Row role(std::size_t i) { return {Kind::Role, i}; }
    static constexpr Row model(std::size_t i) { return {Kind::Model, i}; }
    friend bool operator==(const Row&, const Row&) = default;
};

namespace local {
struct Query {
    std::string text;
    friend bool operator==(const Query&, const Query&) = default;
};
struct Up {
    friend bool operator==(const Up&, const Up&) = default;
};
struct Down {
    friend bool operator==(const Down&, const Down&) = default;
};
// Picks the selected row.
struct Submit {
    friend bool operator==(const Submit&, const Submit&) = default;
};
// Picks this row of `State::rows`.
struct Pick {
    std::size_t index = 0;
    friend bool operator==(const Pick&, const Pick&) = default;
};
} // namespace local

using Local = std::variant<local::Query, local::Up, local::Down, local::Submit, local::Pick>;
using Message = kit::widgets::Msg<Local, Intent>;

// A heading and its rows: `start` indexes `State::rows`.
struct Group {
    std::string_view title;
    std::size_t start = 0;
    std::span<const Row> rows;
};

class State : public kit::widgets::Screen<Local, Intent> {
  public:
    // Opens the picker; the task focuses the search field.
    static std::pair<State, shell::Task> open(Catalog catalog, Target target, std::optional<ModelRef> current);

    [[nodiscard]] const Target& target() const noexcept { return target_; }
    [[nodiscard]] const Catalog& catalog() const noexcept { return catalog_; }
    [[nodiscard]] const std::optional<ModelRef>& current() const noexcept { return current_; }
    [[nodiscard]] const std::string& query() const noexcept { return query_; }
    [[nodiscard]] std::span<const Row> rows() const noexcept { return rows_; }
    // Index into `rows()` of the highlighted row.
    [[nodiscard]] std::optional<std::size_t> selected() const noexcept;

    // The rows under their headings: "Roles" first when picking for the session, then one
    // group per provider.
    [[nodiscard]] std::vector<Group> groups() const;
    // Roles resolving to `model`.
    [[nodiscard]] std::vector<std::string_view> roles_of(const ModelRef& model) const;
    // Keeps the selected row in view after a move.
    [[nodiscard]] shell::Task scroll() const;

    // Arrow keys and Escape while the search field has focus (iced subscription): Qt key code
    // to message, or nothing.
    [[nodiscard]] static std::optional<Message> key(int qt_key);

    std::optional<Intent> reduce(Local m) override;
    // Only closing is emitted directly.
    [[nodiscard]] bool guard(const Intent& e) const override;

  private:
    State() = default;
    [[nodiscard]] std::optional<Intent> pick(std::size_t index) const;
    void refilter();

    Catalog catalog_;
    Target target_;
    std::optional<ModelRef> current_;
    // Lowercase search text per model: ids, names and the roles it serves.
    std::vector<std::string> haystacks_;
    // Display order of each model's provider: its first appearance in the catalog.
    std::vector<std::size_t> provider_rank_;
    std::string query_;
    std::vector<Row> rows_;
    std::size_t selected_ = 0;
};

} // namespace loa::ui::settings::models::picker
