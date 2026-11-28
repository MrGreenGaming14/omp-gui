// Surface input to RFB messages. Port of `loa-surfaces/src/vnc/input.rs`.

#include "loa/surfaces/vnc/input.hpp"

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdlib>
#include <optional>

#include "internal.hpp"
#include "loa/surfaces/vnc/keysym.hpp"
#include "loa/surfaces/vnc/proto.hpp"

namespace loa::surfaces::vnc {

namespace {

namespace cs = loa::contracts::surface;

// Surface coordinate to a framebuffer pixel, clamped to the framebuffer.
std::uint16_t coordinate(float v, std::uint32_t size) {
    const auto max = float(std::min<std::uint32_t>(size == 0 ? 0 : size - 1, 0xFFFF));
    // Truncation is the pixel the point lies in; NaN becomes 0.
    if (std::isnan(v)) return 0;
    return std::uint16_t(std::clamp(v, 0.0F, max));
}

// Adds `delta` to the accumulator and takes whole wheel clicks out of it.
std::int32_t take_clicks(float& acc, float delta, float step) {
    if (!std::isfinite(delta) || step <= 0.0F) return 0;
    acc += delta;
    const float clicks = std::trunc(acc / step);
    acc -= clicks * step;
    if (std::isnan(clicks)) return 0;
    return std::int32_t(std::clamp(clicks, -2147483648.0F, 2147483520.0F));
}

} // namespace

void InputState::reset() {
    const auto x = x_;
    const auto y = y_;
    *this = InputState{};
    x_ = x;
    y_ = y;
}

RfbResult<void> InputState::encode(const cs::Input& input, std::uint32_t width, std::uint32_t height, float scroll_step,
                                   std::vector<std::uint8_t>& buf) {
    // Modifiers the event asks for that are not held are pressed around it, released in reverse.
    const cs::Modifiers* wanted = nullptr;
    if (const auto* b = std::get_if<cs::input::MouseButton>(&input)) wanted = &b->modifiers;
    if (const auto* s = std::get_if<cs::input::Scroll>(&input)) wanted = &s->modifiers;
    if (const auto* k = std::get_if<cs::input::Key>(&input); k && !keysym::is_modifier(k->key)) wanted = &k->modifiers;
    std::array<std::optional<std::uint32_t>, 4> extra{};
    if (wanted) {
        if (wanted->shift && !held_.shift) extra[0] = keysym::SHIFT_L;
        if (wanted->ctrl && !held_.ctrl) extra[1] = keysym::CONTROL_L;
        if (wanted->alt && !held_.alt) extra[2] = keysym::ALT_L;
        if (wanted->meta && !held_.meta) extra[3] = keysym::SUPER_L;
    }
    for (const auto& sym : extra) {
        if (sym) Msg::key(true, *sym).write(buf);
    }

    if (const auto* m = std::get_if<cs::input::MouseMove>(&input)) {
        pointer(buf, m->x, m->y, width, height);
    } else if (const auto* b = std::get_if<cs::input::MouseButton>(&input)) {
        std::uint8_t bit = 0;
        switch (b->button) {
        case cs::MouseButton::Left: bit = 1; break;
        case cs::MouseButton::Middle: bit = 1U << 1U; break;
        case cs::MouseButton::Right: bit = 1U << 2U; break;
        case cs::MouseButton::Back: bit = 1U << 7U; break;
        case cs::MouseButton::Forward: return std::unexpected(RfbError::forward_button());
        }
        buttons_ = b->pressed ? std::uint8_t(buttons_ | bit) : std::uint8_t(buttons_ & ~bit);
        pointer(buf, b->x, b->y, width, height);
    } else if (const auto* s = std::get_if<cs::input::Scroll>(&input)) {
        pointer(buf, s->x, s->y, width, height);
        wheel(buf, take_clicks(scroll_y_, s->dy, scroll_step), WHEEL_DOWN, WHEEL_UP);
        wheel(buf, take_clicks(scroll_x_, s->dx, scroll_step), WHEEL_RIGHT, WHEEL_LEFT);
    } else if (const auto* k = std::get_if<cs::input::Key>(&input)) {
        const auto sym = keysym::keysym(k->key, k->code);
        if (!sym) return std::unexpected(RfbError::key(k->key));
        if (k->key == "Shift") held_.shift = k->pressed;
        else if (k->key == "Control") held_.ctrl = k->pressed;
        else if (k->key == "Alt") held_.alt = k->pressed;
        else if (k->key == "Meta") held_.meta = k->pressed;
        Msg::key(k->pressed, *sym).write(buf);
    } else if (const auto* t = std::get_if<cs::input::Text>(&input)) {
        const std::u32string chars = detail::utf8_decode(t->text);
        for (std::size_t i = 0; i < chars.size(); ++i) {
            const char32_t c = chars[i];
            if (c == U'\r' && i + 1 < chars.size() && chars[i + 1] == U'\n') continue;
            const auto sym = keysym::char_keysym(c);
            if (!sym) return std::unexpected(RfbError::key(detail::utf8_encode(c)));
            Msg::key(true, *sym).write(buf);
            Msg::key(false, *sym).write(buf);
        }
    }
    // Resize: RFB 3.8 has no client initiated resize without the
    // ExtendedDesktopSize extension; the surface keeps its size.

    for (auto it = extra.rbegin(); it != extra.rend(); ++it) {
        if (*it) Msg::key(false, **it).write(buf);
    }
    return {};
}

void InputState::pointer(std::vector<std::uint8_t>& buf, float x, float y, std::uint32_t width, std::uint32_t height) {
    x_ = coordinate(x, width);
    y_ = coordinate(y, height);
    Msg::pointer(buttons_, x_, y_).write(buf);
}

void InputState::wheel(std::vector<std::uint8_t>& buf, std::int32_t clicks, std::uint8_t positive,
                       std::uint8_t negative) const {
    const std::uint8_t bit = clicks > 0 ? positive : negative;
    const std::uint32_t count = clicks < 0 ? 0U - std::uint32_t(clicks) : std::uint32_t(clicks);
    for (std::uint32_t i = 0; i < count; ++i) {
        Msg::pointer(std::uint8_t(buttons_ | bit), x_, y_).write(buf);
        Msg::pointer(buttons_, x_, y_).write(buf);
    }
}

} // namespace loa::surfaces::vnc
