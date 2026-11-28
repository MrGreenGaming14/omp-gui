// Ports of the unit tests in `vnc/{proto,keysym,input}.rs` and the codec test
// in `vnc/tests.rs`.

#include <doctest/doctest.h>

#include <asio.hpp>

#include <cmath>
#include <limits>
#include <string>
#include <tuple>

#include "loa/surfaces/vnc/decode.hpp"
#include "loa/surfaces/vnc/input.hpp"
#include "loa/surfaces/vnc/keysym.hpp"
#include "loa/surfaces/vnc/proto.hpp"
#include "loa/surfaces/vnc/testkit.hpp"

using namespace loa::surfaces;
using namespace loa::surfaces::vnc;
namespace cs = loa::contracts::surface;
namespace rfb = loa::surfaces::testkit::rfb;
using Bytes = std::vector<std::uint8_t>;

namespace {

Bytes concat(std::initializer_list<Bytes> parts) {
    Bytes out;
    for (const auto& p : parts) out.insert(out.end(), p.begin(), p.end());
    return out;
}

Bytes be64(std::uint64_t v) {
    Bytes out;
    for (int s = 56; s >= 0; s -= 8) out.push_back(std::uint8_t(v >> unsigned(s)));
    return out;
}

std::uint8_t mirror(std::uint8_t b) {
    std::uint8_t r = 0;
    for (int i = 0; i < 8; ++i) r = std::uint8_t(r | (((b >> i) & 1U) << (7 - i)));
    return r;
}

Bytes text(std::string_view s) { return {s.begin(), s.end()}; }

} // namespace

TEST_CASE("version_banners_and_vnc_responses") {
    const std::vector<std::pair<std::string_view, std::optional<std::uint32_t>>> banners = {
        {"RFB 003.008\n", 8}, {"RFB 003.889\n", 8}, {"RFB 003.007\n", 7},
        {"RFB 003.005\n", 3}, {"RFB 004.000\n", std::nullopt}, {"HTTP/1.1 200", std::nullopt},
    };
    for (const auto& [banner, want] : banners) {
        const auto got = parse_version(text(banner));
        CHECK(got.has_value() == want.has_value());
        if (got && want) CHECK(*got == *want);
    }
    CHECK(parse_version(text("HTTP/1.1 200")).error().message() == "protocol error: bad version banner \"HTTP/1.1 200\"");
    // "\x80" mirrors to the key 0100000000000000, zero up to parity: the known
    // DES answer for a zero key and block, twice (ECB).
    const Bytes zero = be64(0x8CA6'4DE9'C1B1'23A7ULL);
    const Bytes key80{0x80};
    const auto r = vnc_response(key80, {});
    CHECK(Bytes(r.begin(), r.end()) == concat({zero, zero}));
    // FIPS 81 "Now is t" vector: key 0123456789ABCDEF, mirrored back.
    Bytes key;
    for (const auto b : be64(0x0123'4567'89AB'CDEFULL)) key.push_back(mirror(b));
    std::array<std::uint8_t, 16> challenge{};
    const std::string_view now = "Now is t";
    std::copy(now.begin(), now.end(), challenge.begin());
    const auto fips = vnc_response(key, challenge);
    CHECK(Bytes(fips.begin(), fips.begin() + 8) == be64(0x3FA4'0E8A'984D'4815ULL));
}

TEST_CASE("keysym_table") {
    const std::vector<std::tuple<std::string_view, std::string_view, std::optional<std::uint32_t>>> cases = {
        {"a", "KeyA", 0x61}, {"A", "KeyA", 0x41}, {"!", "Digit1", 0x21}, {" ", "Space", 0x20},
        {"~", "Backquote", 0x7e}, {"é", "KeyE", 0xe9}, {"€", "KeyE", 0x0100'20ac}, {"ж", "KeyX", 0x0100'0436},
        {"Shift", "ShiftLeft", 0xffe1}, {"Shift", "ShiftRight", 0xffe2}, {"Control", "ControlRight", 0xffe4},
        {"Alt", "AltLeft", 0xffe9}, {"Meta", "MetaRight", 0xffec}, {"Shift", "", 0xffe1},
        {"CapsLock", "CapsLock", 0xffe5}, {"AltGraph", "AltRight", 0xfe03},
        {"Backspace", "", 0xff08}, {"Tab", "", 0xff09}, {"Enter", "", 0xff0d}, {"Escape", "", 0xff1b},
        {"Delete", "", 0xffff}, {"Insert", "", 0xff63}, {"Home", "", 0xff50}, {"End", "", 0xff57},
        {"PageUp", "", 0xff55}, {"PageDown", "", 0xff56}, {"ArrowLeft", "", 0xff51}, {"ArrowDown", "", 0xff54},
        {"PrintScreen", "", 0xff61}, {"ContextMenu", "", 0xff67},
        {"F1", "F1", 0xffbe}, {"F12", "F12", 0xffc9}, {"F35", "F35", 0xffe0}, {"F36", "F36", std::nullopt},
        {"F0", "F0", std::nullopt}, {"Fn", "Fn", std::nullopt},
        {"7", "Numpad7", 0xffb7}, {"Enter", "NumpadEnter", 0xff8d}, {"+", "NumpadAdd", 0xffab}, {".", "NumpadDecimal", 0xffae},
        // Num Lock off: the numpad key reports a navigation key.
        {"Home", "Numpad7", 0xff50},
        {"Unidentified", "", std::nullopt}, {"Dead", "Quote", std::nullopt}, {"", "", std::nullopt},
    };
    for (const auto& [key, code, sym] : cases) {
        INFO(std::string(key) << " " << std::string(code));
        CHECK(keysym::keysym(key, code) == sym);
    }
    CHECK(keysym::char_keysym(U'\n') == 0xff0dU);
    CHECK(keysym::char_keysym(U'\t') == 0xff09U);
    CHECK(keysym::char_keysym(U'\u0007') == std::nullopt);
}

TEST_CASE("inputs_become_pointer_and_key_events") {
    const auto ptr = [](std::uint8_t mask, std::uint16_t x, std::uint16_t y) { return Msg::pointer(mask, x, y).bytes(); };
    const auto key = [](bool down, std::uint32_t sym) { return Msg::key(down, sym).bytes(); };
    const cs::Modifiers none;
    const cs::Modifiers ctrl_shift{.shift = true, .ctrl = true};
    const auto button = [](cs::MouseButton b, bool pressed, cs::Modifiers m) {
        return cs::Input{cs::input::MouseButton{10.6F, 20.2F, b, pressed, m}};
    };
    const auto scroll = [&](float dx, float dy) { return cs::Input{cs::input::Scroll{5.0F, 6.0F, dx, dy, none}}; };
    const auto key_input = [&](std::string k, std::string code, bool pressed) {
        return cs::Input{cs::input::Key{std::move(k), std::move(code), pressed, ctrl_shift}};
    };
    const auto wheel = [&](std::uint8_t held, std::vector<std::uint8_t> bits) {
        Bytes out = ptr(held, 5, 6);
        for (const auto bit : bits) out = concat({out, ptr(std::uint8_t(held | bit), 5, 6), ptr(held, 5, 6)});
        return out;
    };
    Bytes typed;
    for (const std::uint32_t s : {0x68U, 0xe9U, 0x0100'20acU, keysym::RETURN, keysym::TAB}) {
        typed = concat({typed, key(true, s), key(false, s)});
    }
    using cs::MouseButton;
    // One state through the whole sequence: buttons, wheel remainders and held modifiers carry over.
    const std::vector<std::pair<cs::Input, Bytes>> steps = {
        {button(MouseButton::Left, true, none), ptr(1, 10, 20)},
        {button(MouseButton::Right, true, none), ptr(5, 10, 20)},
        {cs::input::MouseMove{30.0F, 40.0F}, ptr(5, 30, 40)},
        {button(MouseButton::Left, false, none), ptr(4, 10, 20)},
        {button(MouseButton::Back, true, none), ptr(0x84, 10, 20)},
        {button(MouseButton::Back, false, none), ptr(4, 10, 20)},
        {button(MouseButton::Right, false, none), ptr(0, 10, 20)},
        // Coordinates clamp to the framebuffer; NaN is 0.
        {cs::input::MouseMove{-5.0F, 9000.0F}, ptr(0, 0, 599)},
        {cs::input::MouseMove{std::numeric_limits<float>::quiet_NaN(), 599.9F}, ptr(0, 0, 599)},
        // 100 px down is two clicks with 20 px carried over; 20 more completes a third.
        {scroll(0.0F, 100.0F), wheel(0, {WHEEL_DOWN, WHEEL_DOWN})},
        {scroll(0.0F, 20.0F), wheel(0, {WHEEL_DOWN})},
        {scroll(-40.0F, -40.0F), wheel(0, {WHEEL_UP, WHEEL_LEFT})},
        {scroll(40.0F, 0.0F), wheel(0, {WHEEL_RIGHT})},
        // Modifiers not held are pressed around the event and released in reverse.
        {button(MouseButton::Left, true, ctrl_shift),
         concat({key(true, keysym::SHIFT_L), key(true, keysym::CONTROL_L), ptr(1, 10, 20), key(false, keysym::CONTROL_L),
                 key(false, keysym::SHIFT_L)})},
        {cs::input::Scroll{5.0F, 6.0F, 0.0F, 40.0F, none}, wheel(1, {WHEEL_DOWN})},
        // Once Control is held through its own key event, only Shift is wrapped.
        {key_input("Control", "ControlLeft", true), key(true, keysym::CONTROL_L)},
        {key_input("A", "KeyA", true), concat({key(true, keysym::SHIFT_L), key(true, 0x41), key(false, keysym::SHIFT_L)})},
        {key_input("Control", "ControlLeft", false), key(false, keysym::CONTROL_L)},
        {cs::input::Text{"hé€\r\n\t"}, typed},
        {cs::input::Resize{1, 1}, Bytes{}},
    };
    InputState state;
    for (std::size_t n = 0; n < steps.size(); ++n) {
        INFO("step " << n);
        Bytes buf;
        REQUIRE(state.encode(steps[n].first, 800, 600, 40.0F, buf));
        CHECK(buf == steps[n].second);
    }
    const std::vector<cs::Input> bad = {
        cs::input::Key{"Dead", "Quote", true, none},
        cs::input::Text{"\u0007"},
        button(MouseButton::Forward, true, none),
    };
    for (const auto& input : bad) {
        Bytes buf;
        const auto error = state.encode(input, 10, 10, 40.0F, buf);
        REQUIRE_FALSE(error);
        CHECK((error.error().kind() == RfbError::Kind::Key || error.error().kind() == RfbError::Kind::ForwardButton));
    }
    Bytes buf;
    CHECK(state.encode(cs::input::Key{"Dead", "Quote", true, none}, 10, 10, 40.0F, buf).error().message() ==
          "no X11 keysym for key \"Dead\"");
}

namespace {

constexpr Pixel R{255, 0, 0, 255};
constexpr Pixel G{0, 255, 0, 255};
constexpr Pixel B{0, 0, 255, 255};
constexpr Pixel W{255, 255, 255, 255};
constexpr Pixel K{0, 0, 0, 255};

template <typename F>
RgbaImage picture(std::uint32_t width, std::uint32_t height, F px) {
    RgbaImage img{width, height, {}};
    for (std::uint32_t y = 0; y < height; ++y) {
        for (std::uint32_t x = 0; x < width; ++x) {
            const Pixel p = px(x, y);
            img.data.insert(img.data.end(), p.begin(), p.end());
        }
    }
    return img;
}

template <typename T>
T run(asio::awaitable<T> task) {
    asio::io_context io;
    std::optional<T> out;
    asio::co_spawn(io, std::move(task), [&](std::exception_ptr e, T value) {
        if (e) std::rethrow_exception(e);
        out = std::move(value);
    });
    io.run();
    return std::move(*out);
}

} // namespace

TEST_CASE("every_encoding_decodes_what_the_reference_encoder_wrote") {
    const std::array<Pixel, 6> palette{R, G, B, W, K, Pixel{9, 9, 9, 255}};
    const std::vector<RgbaImage> pictures = {
        picture(70, 20, [](auto x, auto y) { return Pixel{std::uint8_t(x * 3), std::uint8_t(y * 12), 7, 255}; }),
        picture(70, 5, [&](auto x, auto y) { return palette[(x + y) % 2]; }),
        picture(18, 17, [&](auto x, auto y) { return palette[(x / 3 + y) % 3]; }),
        picture(9, 3, [&](auto x, auto y) { return palette[(x + y) % 5]; }),
        picture(300, 2, [](auto x, auto) { return x < 290 ? R : G; }),
        picture(5, 4, [](auto, auto) { return B; }),
    };
    Decoders decoders;
    rfb::Deflater z;
    for (std::size_t n = 0; n < pictures.size(); ++n) {
        const RgbaImage& img = pictures[n];
        std::vector<std::pair<std::int32_t, Bytes>> cases = {{0, rfb::raw(img)}, {2, rfb::rre(img)}, {5, rfb::hextile(img)}};
        for (const auto mode : {rfb::Zrle::Raw, rfb::Zrle::Packed, rfb::Zrle::PlainRle, rfb::Zrle::PaletteRle}) {
            cases.emplace_back(16, rfb::zrle(img, mode, z));
        }
        for (const auto& [encoding, payload] : cases) {
            INFO("picture " << n << ", encoding " << encoding);
            Reader wire(payload);
            const auto update = run(decoders.decode(encoding, img.full(), wire));
            REQUIRE(update);
            CHECK(update->kind == Update::Kind::Blit);
            CHECK(wire.remaining() == 0);
            CHECK(decoders.scratch.data == img.data);
        }
    }
    // Cursor rows have independently byte-padded, most-significant-bit-first masks.
    const Bytes cursor_wire = concat({rfb::raw(picture(9, 2, [](auto, auto) { return W; })), Bytes{0x80, 0x80, 0x40, 0}});
    Reader rest(cursor_wire);
    const auto shape = run(decoders.decode(-239, Rect{8, 1, 9, 2}, rest));
    REQUIRE(shape);
    REQUIRE(shape->kind == Update::Kind::Cursor);
    CHECK(rest.remaining() == 0);
    const Cursor& c = shape->cursor;
    CHECK(std::tuple(c.hotspot_x, c.hotspot_y, c.width, c.height) == std::tuple(8U, 1U, 9U, 2U));
    for (std::size_t i = 0; i < c.rgba.size() / 4; ++i) {
        const std::uint8_t alpha = (i == 0 || i == 8 || i == 10) ? 255 : 0;
        CHECK(Bytes(c.rgba.begin() + std::ptrdiff_t(i * 4), c.rgba.begin() + std::ptrdiff_t(i * 4 + 4)) ==
              Bytes{255, 255, 255, alpha});
    }
    // A subrectangle outside its rectangle is a protocol error.
    const auto pr = rfb::pixel(R);
    const auto pg = rfb::pixel(G);
    const Bytes bad = concat({Bytes{0, 0, 0, 1}, Bytes(pr.begin(), pr.end()), Bytes(pg.begin(), pg.end()),
                              Bytes{0, 1, 0, 0, 0, 1, 0, 1}});
    Reader bad_wire(bad);
    const auto rre = run(decoders.decode(2, Rect{0, 0, 1, 1}, bad_wire));
    REQUIRE_FALSE(rre);
    CHECK(rre.error().kind() == RfbError::Kind::Protocol);
}
