#pragma once

// Shared widgets, all styled through the `style.hpp` recipes and sized by token roles, plus the
// component conventions screens implement. Port of `loa-ui-kit/src/widgets/mod.rs`.
// Factories return parentless (or `parent`-owned) widgets; callbacks may be empty, which
// disables the control like iced's `on_press_maybe(None)`.

#include <functional>
#include <optional>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/kit/widgets/form.hpp"
#include "loa/ui/kit/widgets/popover.hpp"
#include "loa/ui/kit/widgets/virtual_list.hpp"

// Complete types so callers can treat the returned buttons and labels as `QWidget*`.
#include <QLabel>
#include <QPushButton>

namespace loa::ui::kit::widgets {

// The message convention of screen modules: handled locally, or emitted to the app.
template <typename L, typename E> struct Msg {
    std::variant<L, E> v;
    static Msg local(L l) { return {std::variant<L, E>(std::in_place_index<0>, std::move(l))}; }
    // Rust `Msg::Emit` (`emit` is a Qt macro).
    static Msg event(E e) { return {std::variant<L, E>(std::in_place_index<1>, std::move(e))}; }
    friend bool operator==(const Msg&, const Msg&) = default;
};

// A screen over `Msg`: `reduce` applies local messages, `guard` admits events the view emits
// directly while they still hold for the current state.
template <typename L, typename E> class Screen {
  public:
    virtual ~Screen() = default;
    virtual std::optional<E> reduce(L m) = 0;
    [[nodiscard]] virtual bool guard(const E& e) const = 0;
    std::optional<E> update(Msg<L, E> m) {
        if (m.v.index() == 0) return reduce(std::get<0>(std::move(m.v)));
        auto e = std::get<1>(std::move(m.v));
        if (guard(e)) return e;
        return std::nullopt;
    }
};

using Callback = std::function<void()>;

// `s` at text role `r` in `tone`.
[[nodiscard]] QLabel* label(std::string_view s, theme::TextRole r, Tone tone, QWidget* parent = nullptr);
// `content` in a card container (padding `m`).
[[nodiscard]] QWidget* card(QWidget* content, QWidget* parent = nullptr);
[[nodiscard]] QWidget* badge(std::string_view s, Tone tone, QWidget* parent = nullptr);
// A labelled button of `kind`; disabled without `on`.
[[nodiscard]] QPushButton* text_button(std::string_view s, ButtonKind kind, Callback on, QWidget* parent = nullptr);
// A ghost icon button with a tooltip; disabled without `on`.
[[nodiscard]] QPushButton* icon_button(Icon icon, std::string_view tip, Callback on, QWidget* parent = nullptr);
// A full-width row with an optional icon.
[[nodiscard]] QPushButton* menu_row(std::optional<Icon> icon, std::string_view s, bool selected, Callback on, QWidget* parent = nullptr);
// A row with a title over a faint detail line.
[[nodiscard]] QPushButton* two_line_row(std::string_view title, std::string_view detail, bool selected, Callback on, QWidget* parent = nullptr);
[[nodiscard]] QWidget* mono_block(std::string_view s, QWidget* parent = nullptr);
// The fill role of a usage bar: accent below half, warn from half, danger from 80%.
[[nodiscard]] theme::ColorRole meter_role(float fraction);
[[nodiscard]] QWidget* meter(float fraction, QWidget* parent = nullptr);
[[nodiscard]] QWidget* section_header(std::string_view title, const std::vector<QWidget*>& actions, QWidget* parent = nullptr);
// An icon, a label, a toned status word and an optional faint detail.
[[nodiscard]] QWidget* status_row(Icon icon, std::string_view name, std::string_view status, Tone tone,
                                  std::optional<std::string_view> detail, QWidget* parent = nullptr);
[[nodiscard]] QWidget* empty_state(Icon icon, std::string_view title, std::string_view hint, QWidget* parent = nullptr);
// A chevron header that shows `body` while open (`body` is owned either way).
[[nodiscard]] QWidget* collapsible(std::string_view title, bool open, Callback toggle, QWidget* body, QWidget* parent = nullptr);

// One tab of a `tab_strip`: `trailing` (a close button, a marker) sits after the pill.
struct Tab {
    std::string_view label;
    std::optional<Icon> icon;
    bool active = false;
    Callback on;
    QWidget* trailing = nullptr;
};
// A row of pill tabs.
[[nodiscard]] QWidget* tab_strip(const std::vector<Tab>& tabs, QWidget* parent = nullptr);

// Width of a splitter grab area.
[[nodiscard]] float splitter_width();

} // namespace loa::ui::kit::widgets
