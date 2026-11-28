#pragma once

// Generic editing components: a form view over any `Form`, validated `FormState`, a list
// editor, and a `Picker` over any `PickerItem`. Port of `loa-ui-kit/src/widgets/form.rs`.

#include <concepts>
#include <cstddef>
#include <expected>
#include <functional>
#include <map>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "loa/ui/kit/icons.hpp"

class QWidget;

namespace loa::ui::kit::widgets {

// One editable field of a `Form`.
struct Field {
    std::string_view key;
    std::string_view label;
    std::string_view placeholder;
    friend bool operator==(const Field&, const Field&) = default;
};

// Anything editable as labelled text fields (settings drafts, host limits).
class Form {
  public:
    virtual ~Form() = default;
    [[nodiscard]] virtual std::span<const Field> fields() const = 0;
    [[nodiscard]] virtual std::string get(std::string_view key) const = 0;
    virtual std::expected<void, std::string> set(std::string_view key, std::string_view value) = 0;
};

// Validation for a `FormState`: `validate(existing)` yields the output or errors per field.
template <typename F>
concept FormSpec = requires(const F& f, std::span<const std::string> existing) {
    typename F::Field;
    typename F::Output;
    { f.validate(existing) } -> std::same_as<std::expected<typename F::Output, std::map<typename F::Field, std::string>>>;
};

// A form's values (`spec`) plus the errors of its last submit.
template <FormSpec F> struct FormState {
    F spec{};
    std::map<typename F::Field, std::string> errors{};

    FormState() = default;
    explicit FormState(F s) : spec(std::move(s)) {}

    [[nodiscard]] std::optional<std::string_view> error(typename F::Field f) const {
        const auto it = errors.find(f);
        if (it == errors.end()) return std::nullopt;
        return it->second;
    }

    // The output when valid; otherwise records the errors.
    std::optional<typename F::Output> submit(std::span<const std::string> existing) {
        auto r = spec.validate(existing);
        if (r) {
            errors.clear();
            return std::move(*r);
        }
        errors = std::move(r.error());
        return std::nullopt;
    }
};

// An option a `Picker` lists.
template <typename T>
concept PickerItem = requires(const T& t) {
    { t.label() } -> std::convertible_to<std::string>;
};

template <typename T> std::optional<std::string> picker_detail(const T& t) {
    if constexpr (requires { { t.detail() } -> std::convertible_to<std::optional<std::string>>; }) return t.detail();
    else return std::nullopt;
}

template <typename T> std::optional<Icon> picker_icon(const T& t) {
    if constexpr (requires { { t.icon() } -> std::convertible_to<std::optional<Icon>>; }) return t.icon();
    else return std::nullopt;
}

struct PickerMsg {
    enum class Kind { Toggle, Pick } kind = Kind::Toggle;
    std::size_t index = 0;
    static PickerMsg toggle() { return {Kind::Toggle, 0}; }
    static PickerMsg pick(std::size_t i) { return {Kind::Pick, i}; }
    friend bool operator==(const PickerMsg&, const PickerMsg&) = default;
};

// One row of a picker menu, after `PickerItem` is applied.
struct PickerRow {
    std::string text;
    std::optional<Icon> icon;
    bool selected = false;
};

// The picker head button plus its popover menu (non-template half of `Picker::view`).
[[nodiscard]] QWidget* picker_view(std::string_view current, bool open, const std::vector<PickerRow>& rows,
                                   std::function<void(PickerMsg)> on, QWidget* parent = nullptr);

// A button showing the selected option that opens a menu of all of them above it.
template <PickerItem T> struct Picker {
    std::vector<T> options;
    std::optional<std::size_t> selected;
    bool open = false;

    Picker() = default;
    Picker(std::vector<T> o, std::optional<std::size_t> s) : options(std::move(o)), selected(s) {}

    [[nodiscard]] const T* selected_item() const {
        return selected && *selected < options.size() ? &options[*selected] : nullptr;
    }

    // Applies `m`; returns the newly picked option.
    const T* update(PickerMsg m) {
        if (m.kind == PickerMsg::Kind::Toggle) {
            open = !open;
            return nullptr;
        }
        if (m.index < options.size()) {
            selected = m.index;
            open = false;
        }
        return selected_item();
    }

    [[nodiscard]] QWidget* view(std::string_view placeholder, std::function<void(PickerMsg)> on, QWidget* parent = nullptr) const {
        std::vector<PickerRow> rows;
        for (std::size_t i = 0; i < options.size(); ++i) {
            std::string text = options[i].label();
            if (auto d = picker_detail(options[i])) text += " \u00b7 " + *d;
            rows.push_back({std::move(text), picker_icon(options[i]), selected == i});
        }
        const auto* s = selected_item();
        const std::string current = s ? std::string(s->label()) : std::string(placeholder);
        return picker_view(current, open, rows, std::move(on), parent);
    }
};

// Labelled inputs for every field of `form`, with `errors` under their fields; Enter calls `on_submit`.
[[nodiscard]] QWidget* form_view(const Form& form, const std::map<std::string, std::string, std::less<>>& errors,
                                 std::function<void(std::string_view key, std::string value)> on_edit,
                                 std::function<void()> on_submit, QWidget* parent = nullptr);

// Rows of `items` with a remove button each (and up/down buttons with `on_move`), then an input
// that adds `draft`.
[[nodiscard]] QWidget* list_editor(std::span<const std::string> items, std::function<void(std::size_t)> on_remove,
                                   std::function<void(std::size_t from, std::size_t to)> on_move, std::string_view draft,
                                   std::function<void(std::string)> on_draft, std::function<void()> on_add,
                                   QWidget* parent = nullptr);

// Up and down buttons for row `index` of `len`, each disabled at its end; `on_move(from, to)`.
[[nodiscard]] QWidget* reorder(std::size_t index, std::size_t len, const std::function<void(std::size_t, std::size_t)>& on_move,
                               QWidget* parent = nullptr);

} // namespace loa::ui::kit::widgets
