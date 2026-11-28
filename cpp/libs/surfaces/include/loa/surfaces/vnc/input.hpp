#pragma once

// Surface input to RFB PointerEvent and KeyEvent messages. Port of
// `loa-surfaces/src/vnc/input.rs`.

#include <cstdint>
#include <vector>

#include "loa/contracts/surface.hpp"
#include "loa/surfaces/vnc/vnc.hpp"

namespace loa::surfaces::vnc {

inline constexpr std::uint8_t WHEEL_UP = 1U << 3U;
inline constexpr std::uint8_t WHEEL_DOWN = 1U << 4U;
inline constexpr std::uint8_t WHEEL_LEFT = 1U << 5U;
inline constexpr std::uint8_t WHEEL_RIGHT = 1U << 6U;

// What the server has been told so far: pressed buttons, pointer position,
// held modifiers, and scroll distance not yet sent as a wheel click.
class InputState {
  public:
    // Forgets pressed buttons and keys, for a fresh connection.
    void reset();

    // Appends the messages for `input` to `buf`. `width` and `height` are the
    // framebuffer size, `scroll_step` the pixels per wheel click. On error
    // `buf` may hold a partial encoding and must not be sent.
    RfbResult<void> encode(const loa::contracts::surface::Input& input, std::uint32_t width, std::uint32_t height,
                           float scroll_step, std::vector<std::uint8_t>& buf);

  private:
    void pointer(std::vector<std::uint8_t>& buf, float x, float y, std::uint32_t width, std::uint32_t height);
    void wheel(std::vector<std::uint8_t>& buf, std::int32_t clicks, std::uint8_t positive, std::uint8_t negative) const;

    std::uint8_t buttons_ = 0;
    std::uint16_t x_ = 0;
    std::uint16_t y_ = 0;
    loa::contracts::surface::Modifiers held_;
    float scroll_x_ = 0;
    float scroll_y_ = 0;
};

} // namespace loa::surfaces::vnc
