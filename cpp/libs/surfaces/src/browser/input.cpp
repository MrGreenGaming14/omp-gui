#include "loa/surfaces/browser/input.hpp"

#include <algorithm>
#include <array>
#include <cmath>
#include <cstring>

namespace loa::surfaces::browser {

namespace cs = loa::contracts::surface;
namespace json = loa::runtime::json;

namespace {

// Presses closer than this in time and space count as one multi-click.
constexpr auto MULTI_CLICK_TIME = std::chrono::milliseconds(500);
constexpr float MULTI_CLICK_DISTANCE = 4.0F;

struct ButtonInfo {
    cs::MouseButton button;
    std::string_view cdp;
    std::int64_t bit;
};

// Each button with its CDP name and its bit of the CDP `buttons` field, in bit order.
constexpr std::array<ButtonInfo, 5> BUTTONS = {{
    {cs::MouseButton::Left, "left", 1},
    {cs::MouseButton::Right, "right", 2},
    {cs::MouseButton::Middle, "middle", 4},
    {cs::MouseButton::Back, "back", 8},
    {cs::MouseButton::Forward, "forward", 16},
}};

// Bits of the CDP `modifiers` field.
std::int64_t modifier_bits(cs::Modifiers m) {
    return std::int64_t{m.alt} | std::int64_t{m.ctrl} << 1 | std::int64_t{m.meta} << 2 | std::int64_t{m.shift} << 3;
}

MouseParams mouse(std::string type, float x, float y, std::int64_t held, std::optional<cs::Modifiers> modifiers) {
    MouseParams p;
    p.type = std::move(type);
    p.x = double(x);
    p.y = double(y);
    p.buttons = held;
    if (modifiers) p.modifiers = modifier_bits(*modifiers);
    return p;
}

// Rust `char::is_control` over UTF-8: C0, DEL and C1.
bool all_control(std::string_view text) {
    for (std::size_t i = 0; i < text.size(); ++i) {
        const auto c = static_cast<unsigned char>(text[i]);
        if (c < 0x20 || c == 0x7f) continue;
        if (c == 0xc2 && i + 1 < text.size()) {
            const auto n = static_cast<unsigned char>(text[i + 1]);
            if (n >= 0x80 && n <= 0x9f) {
                ++i;
                continue;
            }
        }
        return false;
    }
    return true;
}

} // namespace

json::Value MouseParams::to_json() const {
    json::Value v = json::Object{};
    v.set("type", type);
    v.set("x", x);
    v.set("y", y);
    if (modifiers) v.set("modifiers", *modifiers);
    if (button) v.set("button", *button);
    if (buttons) v.set("buttons", *buttons);
    if (click_count) v.set("clickCount", *click_count);
    if (delta_x) v.set("deltaX", *delta_x);
    if (delta_y) v.set("deltaY", *delta_y);
    v.set("pointerType", pointer_type);
    return v;
}

json::Value KeyParams::to_json() const {
    json::Value v = json::Object{};
    v.set("type", type);
    if (modifiers) v.set("modifiers", *modifiers);
    if (text) v.set("text", *text);
    if (unmodified_text) v.set("unmodifiedText", *unmodified_text);
    if (code) v.set("code", *code);
    if (key) v.set("key", *key);
    if (windows_virtual_key_code) v.set("windowsVirtualKeyCode", *windows_virtual_key_code);
    if (native_virtual_key_code) v.set("nativeVirtualKeyCode", *native_virtual_key_code);
    if (location) v.set("location", *location);
    return v;
}

std::optional<Command> InputMapper::map(const cs::Input& input, Clock::time_point now) {
    if (const auto* m = std::get_if<cs::input::MouseMove>(&input)) {
        // CDP wants one `button` on moves: the first held one, else none.
        auto params = mouse("mouseMoved", m->x, m->y, held_, std::nullopt);
        const auto* held = std::ranges::find_if(BUTTONS, [&](const ButtonInfo& b) { return (held_ & b.bit) != 0; });
        params.button = std::string(held == BUTTONS.end() ? std::string_view("none") : held->cdp);
        return params;
    }
    if (const auto* b = std::get_if<cs::input::MouseButton>(&input)) {
        const auto& info = *std::ranges::find_if(BUTTONS, [&](const ButtonInfo& i) { return i.button == b->button; });
        if (b->pressed) {
            const bool near = last_press_ && last_press_->button == b->button && now - last_press_->at <= MULTI_CLICK_TIME &&
                              std::fabs(b->x - last_press_->x) <= MULTI_CLICK_DISTANCE &&
                              std::fabs(b->y - last_press_->y) <= MULTI_CLICK_DISTANCE;
            click_count_ = near ? click_count_ + 1 : 1;
            last_press_ = Press{b->button, now, b->x, b->y};
            held_ |= info.bit;
        } else {
            held_ &= ~info.bit;
        }
        auto params = mouse(b->pressed ? "mousePressed" : "mouseReleased", b->x, b->y, held_, b->modifiers);
        params.button = std::string(info.cdp);
        params.click_count = std::max<std::int64_t>(click_count_, 1);
        return params;
    }
    if (const auto* s = std::get_if<cs::input::Scroll>(&input)) {
        auto params = mouse("mouseWheel", s->x, s->y, held_, s->modifiers);
        params.delta_x = double(s->dx);
        params.delta_y = double(s->dy);
        return params;
    }
    if (const auto* k = std::get_if<cs::input::Key>(&input)) return key_event(k->key, k->code, k->pressed, k->modifiers);
    if (const auto* t = std::get_if<cs::input::Text>(&input)) {
        if (t->text.empty()) return std::nullopt;
        return TextParams{t->text};
    }
    const auto& r = std::get<cs::input::Resize>(input);
    return ResizeCommand{std::clamp<std::uint32_t>(r.width, 1, MAX_EDGE), std::clamp<std::uint32_t>(r.height, 1, MAX_EDGE)};
}

KeyParams key_event(std::string_view key, std::string_view code, bool pressed, cs::Modifiers modifiers) {
    const KeyDefinition* definition = key_definition(key);
    if (definition && !code.empty() && definition->code != code) definition = nullptr;
    if (!definition && !code.empty()) {
        const auto layout = us_keyboard_layout();
        const auto it = std::ranges::find_if(layout, [&](const KeyDefinition& d) { return d.code == code; });
        if (it != layout.end()) definition = &*it;
    }
    const char* control_text = definition && definition->text && all_control(definition->text) ? definition->text : nullptr;
    KeyParams params;
    params.type = !pressed ? "keyUp" : control_text ? "keyDown" : "rawKeyDown";
    params.key = std::string(key);
    params.code = std::string(code.empty() ? (definition ? definition->code : std::string_view()) : code);
    if (definition) {
        params.windows_virtual_key_code = definition->key_code;
        params.native_virtual_key_code = definition->key_code;
    }
    params.modifiers = modifier_bits(modifiers);
    if (pressed && control_text) {
        params.text = control_text;
        params.unmodified_text = control_text;
    }
    const bool sided = definition && (definition->key == "Shift" || definition->key == "Control" ||
                                      definition->key == "Alt" || definition->key == "Meta");
    if (code.starts_with("Numpad")) {
        params.location = 3;
    } else if (sided && code.ends_with("Right")) {
        params.location = 2;
    } else if (sided && code.ends_with("Left")) {
        params.location = 1;
    }
    return params;
}

} // namespace loa::surfaces::browser
