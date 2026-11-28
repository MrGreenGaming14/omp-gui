#pragma once

// Test helpers for UI modules: message delivery to a state, synthetic key presses, and
// one-shot clicks on a labelled control. Port of `loa-ui-kit/src/testkit.rs` (the iced frame
// runner has no Qt counterpart; Qt widgets keep their own state between renders).

#include <functional>
#include <optional>
#include <string_view>
#include <vector>

class QKeyEvent;
class QWidget;

namespace loa::ui::kit::testkit {

// Feeds `messages` to `update` in order, collecting the events it returns.
template <typename S, typename M, typename F>
auto deliver(S& state, std::vector<M> messages, F&& update) {
    using E = typename std::invoke_result_t<F&, S&, M>::value_type;
    std::vector<E> out;
    for (auto& m : messages)
        if (auto e = update(state, std::move(m))) out.push_back(std::move(*e));
    return out;
}

// Sends a press and release of Qt key `key` (with `modifiers`, typing `text`) to `target`.
void key(QWidget& target, int key, int modifiers = 0, std::string_view text = {});
// Sends a press of a character key, typing it unless Ctrl or Meta is held.
void type_char(QWidget& target, std::string_view c, int modifiers = 0);

// Clicks the enabled button under `root` whose text or accessible name is `label`; false when
// there is none.
bool click_label(QWidget& root, std::string_view label);

} // namespace loa::ui::kit::testkit
