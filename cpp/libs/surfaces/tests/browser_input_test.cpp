// Port of the unit tests in `loa-surfaces/src/browser/input.rs`.

#include <doctest/doctest.h>

#include <tuple>

#include "loa/surfaces/browser/input.hpp"

using namespace loa::surfaces::browser;
namespace cs = loa::contracts::surface;
using std::chrono::milliseconds;

namespace {

using Want = std::tuple<std::string, std::optional<std::string>, std::optional<std::int64_t>, std::optional<std::int64_t>,
                        std::optional<std::int64_t>>;

cs::Input left(float x, bool pressed) { return cs::input::MouseButton{x, 10.0F, cs::MouseButton::Left, pressed, {}}; }

} // namespace

TEST_CASE("mouse events count clicks, carry held buttons and modifiers") {
    InputMapper mapper;
    const auto t0 = InputMapper::Clock::now();
    auto mouse = [&](const cs::Input& input, milliseconds at) -> Want {
        auto command = mapper.map(input, t0 + at);
        REQUIRE(command);
        const auto* p = std::get_if<MouseParams>(&*command);
        REQUIRE(p);
        return {p->type, p->button, p->buttons, p->click_count, p->modifiers};
    };
    const std::optional<std::string> l = "left";
    const std::optional<std::int64_t> none;
    const std::vector<std::tuple<cs::Input, milliseconds, Want>> steps = {
        {left(10, true), milliseconds(0), {"mousePressed", l, 1, 1, 0}},
        {cs::input::MouseMove{50, 60}, milliseconds(0), {"mouseMoved", l, 1, none, none}},
        {left(10, false), milliseconds(0), {"mouseReleased", l, 0, 1, 0}},
        {cs::input::MouseMove{51, 60}, milliseconds(0), {"mouseMoved", std::string("none"), 0, none, none}},
        {left(11, true), milliseconds(120), {"mousePressed", l, 1, 2, 0}},
        {left(11, false), milliseconds(120), {"mouseReleased", l, 0, 2, 0}},
        {left(11, true), milliseconds(1020), {"mousePressed", l, 1, 1, 0}},
        {left(11, false), milliseconds(1020), {"mouseReleased", l, 0, 1, 0}},
        {left(80, true), milliseconds(1030), {"mousePressed", l, 1, 1, 0}},
    };
    for (std::size_t n = 0; n < steps.size(); ++n) {
        CAPTURE(n);
        CHECK(mouse(std::get<0>(steps[n]), std::get<1>(steps[n])) == std::get<2>(steps[n]));
    }
    cs::Modifiers modifiers;
    modifiers.ctrl = modifiers.shift = true;
    auto wheel = mapper.map(cs::input::Scroll{5, 6, -3, 120, modifiers}, t0);
    REQUIRE(wheel);
    const auto& w = std::get<MouseParams>(*wheel);
    CHECK(std::tuple(w.type, w.delta_x, w.delta_y, w.modifiers) ==
          std::tuple(std::string("mouseWheel"), std::optional(-3.0), std::optional(120.0), std::optional<std::int64_t>(2 | 8)));
    CHECK_FALSE(mapper.map(cs::input::Text{""}, t0));
    auto resize = mapper.map(cs::input::Resize{0, 100'000}, t0);
    REQUIRE(resize);
    CHECK(std::get<ResizeCommand>(*resize) == ResizeCommand{1, MAX_EDGE});
}

TEST_CASE("keys get virtual key codes and only control keys carry text") {
    const cs::Modifiers none;
    cs::Modifiers ctrl;
    ctrl.ctrl = true;
    cs::Modifiers shift;
    shift.shift = true;
    using KWant = std::tuple<std::string, std::optional<std::int64_t>, std::optional<std::string>, std::optional<std::int64_t>>;
    const std::optional<std::string> no_text;
    const std::vector<std::tuple<std::string, std::string, bool, cs::Modifiers, KWant>> cases = {
        {"Backspace", "Backspace", true, none, {"rawKeyDown", 8, no_text, 0}},
        {"Enter", "Enter", true, none, {"keyDown", 13, std::string("\r"), 0}},
        {"Enter", "Enter", false, none, {"keyUp", 13, no_text, 0}},
        // Letters insert no text twice; select all keeps its Ctrl.
        {"a", "KeyA", true, ctrl, {"rawKeyDown", 65, no_text, 2}},
        // A shifted layout key is still found by its physical code.
        {"!", "Digit1", true, shift, {"rawKeyDown", 49, no_text, 8}},
    };
    for (const auto& [key, code, pressed, modifiers, want] : cases) {
        CAPTURE(key);
        const auto e = key_event(key, code, pressed, modifiers);
        CHECK(KWant(e.type, e.windows_virtual_key_code, e.text, e.modifiers) == want);
    }
}

TEST_CASE("the US layout matches chromiumoxide's table") {
    CHECK(us_keyboard_layout().size() == 244);
    const auto* enter = key_definition("Enter");
    REQUIRE(enter);
    CHECK(std::tuple(enter->key_code, enter->code, std::string(enter->text)) == std::tuple(std::int64_t(13), std::string_view("Enter"), std::string("\r")));
    const auto shift_right = key_event("Shift", "ShiftRight", true, {});
    CHECK(shift_right.location == std::optional<std::int64_t>(2));
    CHECK(key_event("1", "Numpad1", true, {}).location == std::optional<std::int64_t>(3));
}
