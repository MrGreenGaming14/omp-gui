#pragma once

#include <doctest/doctest.h>

#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/ui/session/composer/state.hpp"

namespace composer_test {

using namespace loa::ui::session::composer;
namespace intent = loa::view::session::composer::intent;

inline std::optional<Intent> act(State& state, Message message) { return state.update(std::move(message)).second; }

// Types `text` a key at a time; `\n` is Shift+Enter. Returns the events.
inline std::vector<Intent> type_text(State& state, std::string_view text) {
    std::vector<Intent> out;
    for (std::size_t i = 0; i < text.size();) {
        const auto b = static_cast<unsigned char>(text[i]);
        const std::size_t n = b < 0x80 ? 1 : b >= 0xF0 ? 4 : b >= 0xE0 ? 3 : 2;
        char32_t c = n == 1 ? b : b & (0x3F >> (n - 1));
        for (std::size_t k = 1; k < n; ++k) c = (c << 6) | (static_cast<unsigned char>(text[i + k]) & 0x3F);
        i += n;
        Edit e = c == '\n' ? Edit{edit::Enter{}} : Edit{edit::Insert{c}};
        if (auto ev = act(state, msg::Edit{action::Edit{e}})) out.push_back(std::move(*ev));
    }
    return out;
}

inline loa::view::session::Prompt submitted(std::optional<Intent> event) {
    REQUIRE(event.has_value());
    const auto* s = std::get_if<intent::Submit>(&*event);
    REQUIRE(s != nullptr);
    return s->prompt;
}

inline std::vector<std::string> attached(const loa::view::session::Prompt& prompt) {
    std::vector<std::string> out;
    for (const auto& a : prompt.attachments) out.push_back(a.path.string());
    return out;
}

inline std::size_t shown_images(const State& state) {
    std::size_t n = 0;
    for (const auto& image : state.images()) n += image.shown ? 1 : 0;
    return n;
}

} // namespace composer_test
