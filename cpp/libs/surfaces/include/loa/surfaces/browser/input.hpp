#pragma once

// Translation of surface input into CDP `Input.*` commands, and the US
// keyboard layout as data. Port of `loa-surfaces/src/browser/input.rs` and
// chromiumoxide's `keys.rs`.

#include <chrono>
#include <cstdint>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <variant>

#include "loa/contracts/surface.hpp"
#include "loa/runtime/json.hpp"

namespace loa::surfaces::browser {

// One key of the US layout (Puppeteer's `USKeyboardLayout`).
struct KeyDefinition {
    std::string_view key;
    std::int64_t key_code = 0;
    std::string_view code;
    // Text the key types, if any.
    const char* text = nullptr;
};

// All 244 key definitions, in Puppeteer's order.
[[nodiscard]] std::span<const KeyDefinition> us_keyboard_layout() noexcept;
// The first definition whose `key` is `key`, or null.
[[nodiscard]] const KeyDefinition* key_definition(std::string_view key) noexcept;

// Largest surface edge a viewer may ask for.
inline constexpr std::uint32_t MAX_EDGE = 8192;

// `Input.dispatchMouseEvent` parameters.
struct MouseParams {
    std::string type; // mousePressed, mouseReleased, mouseMoved, mouseWheel
    double x = 0, y = 0;
    std::optional<std::int64_t> modifiers;
    std::optional<std::string> button; // none, left, middle, right, back, forward
    std::optional<std::int64_t> buttons;
    std::optional<std::int64_t> click_count;
    std::optional<double> delta_x, delta_y;
    std::string pointer_type = "mouse";

    [[nodiscard]] loa::runtime::json::Value to_json() const;
    friend bool operator==(const MouseParams&, const MouseParams&) = default;
};

// `Input.dispatchKeyEvent` parameters.
struct KeyParams {
    std::string type; // keyDown, keyUp, rawKeyDown
    std::optional<std::int64_t> modifiers;
    std::optional<std::string> text, unmodified_text;
    std::optional<std::string> code, key;
    std::optional<std::int64_t> windows_virtual_key_code, native_virtual_key_code;
    std::optional<std::int64_t> location;

    [[nodiscard]] loa::runtime::json::Value to_json() const;
    friend bool operator==(const KeyParams&, const KeyParams&) = default;
};

// `Input.insertText` parameters.
struct TextParams {
    std::string text;
    friend bool operator==(const TextParams&, const TextParams&) = default;
};

struct ResizeCommand {
    std::uint32_t width = 0, height = 0;
    friend bool operator==(const ResizeCommand&, const ResizeCommand&) = default;
};

// One CDP call for one surface input.
using Command = std::variant<MouseParams, KeyParams, TextParams, ResizeCommand>;

// Pointer state CDP needs on every mouse event but the contract sends once.
class InputMapper {
  public:
    using Clock = std::chrono::steady_clock;

    // The command for `input`, or empty when it has no effect.
    [[nodiscard]] std::optional<Command> map(const loa::contracts::surface::Input& input, Clock::time_point now);

  private:
    struct Press {
        loa::contracts::surface::MouseButton button;
        Clock::time_point at;
        float x, y;
    };
    // CDP `buttons` bit mask of the held buttons.
    std::int64_t held_ = 0;
    std::optional<Press> last_press_;
    std::int64_t click_count_ = 0;
};

// The `Input.dispatchKeyEvent` for a key. Key presses carry no text except
// keys whose default action is a control character (Enter).
[[nodiscard]] KeyParams key_event(std::string_view key, std::string_view code, bool pressed,
                                  loa::contracts::surface::Modifiers modifiers);

} // namespace loa::surfaces::browser
