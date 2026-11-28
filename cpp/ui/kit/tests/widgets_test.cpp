// Port of the tests in `loa-ui-kit/src/widgets/virtual_list.rs`, plus the form, picker,
// popover and screen state, and the widget factories through the testkit.

#include <doctest/doctest.h>

#include <numeric>

#include <QLabel>
#include <QPushButton>

#include "fixture.hpp"
#include "loa/ui/kit/testkit.hpp"
#include "loa/ui/kit/widgets.hpp"

using namespace loa::ui::kit;
using namespace loa::ui::kit::widgets;

TEST_CASE("heights: prefix sums stay exact through push, set, truncate and splice") {
    Heights heights;
    std::vector<float> model;
    for (int i = 0; i < 37; ++i) {
        heights.push(10.0F + static_cast<float>(i % 7) * 3.0F);
        model.push_back(10.0F + static_cast<float>(i % 7) * 3.0F);
    }
    heights.set(5, 100);
    model[5] = 100;
    heights.truncate(30);
    model.resize(30);
    const float a[] = {1, 2, 3};
    heights.splice({10, 12}, a);
    model.erase(model.begin() + 10, model.begin() + 12);
    model.insert(model.begin() + 10, std::begin(a), std::end(a));
    const float b[] = {7};
    heights.splice({29, 31}, b);
    model.erase(model.begin() + 29, model.end());
    model.push_back(7);
    for (int i = 0; i < 5; ++i) {
        heights.push(4);
        model.push_back(4);
    }
    REQUIRE(heights.size() == model.size());
    for (std::size_t count = 0; count <= model.size(); ++count) {
        double sum = 0;
        for (std::size_t i = 0; i < count; ++i) sum += model[i];
        INFO("prefix ", count);
        CHECK(heights.prefix(count) == sum);
    }
}

TEST_CASE("heights: find, window and scroll target") {
    Heights heights;
    for (const float h : {10.0F, 20.0F, 30.0F}) heights.push(h);
    const std::pair<double, std::size_t> found[] = {{0.0, 0}, {9.9, 0}, {10.0, 1}, {29.0, 1}, {30.0, 2}, {1e9, 2}};
    for (const auto& [y, i] : found) {
        INFO(y);
        CHECK(heights.find(y) == std::optional(i));
    }
    CHECK(Heights{}.find(0) == std::nullopt);
    CHECK(heights.window(12, 5, 0) == Range{1, 2});
    CHECK(heights.window(0, 5, 40) == Range{0, 3});
    CHECK(scroll_target(50, 100, 40, 60) == std::optional(40.0F));
    CHECK(scroll_target(50, 100, 140, 170) == std::optional(70.0F));
    CHECK(scroll_target(50, 100, 60, 70) == std::nullopt);
}

TEST_CASE("popover: above when it fits, below otherwise, inside the window") {
    const Rect anchor{100, 300, 80, 20};
    CHECK(popover_limits(anchor, 800, 600, 6, true).max_width == 80);
    CHECK(popover_limits(anchor, 800, 600, 6, false).max_height == 294);
    CHECK(popover_place(anchor, 800, 600, 6, true, 80, 100) == Rect{100, 194, 80, 100});
    const Rect top{100, 10, 80, 20};
    CHECK(popover_place(top, 800, 600, 6, true, 80, 100) == Rect{100, 36, 80, 100});
    CHECK(popover_place(Rect{760, 300, 30, 20}, 800, 600, 6, false, 200, 50).x == 600);
}

namespace {

struct Option {
    std::string name;
    [[nodiscard]] std::string label() const { return name; }
    [[nodiscard]] std::optional<std::string> detail() const { return name == "b" ? std::optional<std::string>("bee") : std::nullopt; }
};

enum class NameField { Name };

struct NameSpec {
    using Field = NameField;
    using Output = std::string;
    std::string name;
    [[nodiscard]] std::expected<std::string, std::map<NameField, std::string>> validate(std::span<const std::string> existing) const {
        if (name.empty()) return std::unexpected(std::map<NameField, std::string>{{NameField::Name, "required"}});
        if (std::ranges::find(existing, name) != existing.end())
            return std::unexpected(std::map<NameField, std::string>{{NameField::Name, "taken"}});
        return name;
    }
};

struct Counter final : Screen<int, std::string> {
    int value = 0;
    std::optional<std::string> reduce(int m) override {
        value += m;
        if (value > 2) return "big";
        return std::nullopt;
    }
    [[nodiscard]] bool guard(const std::string& e) const override { return value > 0 || e != "reset"; }
};

} // namespace

TEST_CASE("picker: toggles, picks in range and reports the pick") {
    Picker<Option> p({{"a"}, {"b"}}, std::nullopt);
    CHECK(p.update(PickerMsg::toggle()) == nullptr);
    CHECK(p.open);
    const auto* picked = p.update(PickerMsg::pick(1));
    REQUIRE(picked != nullptr);
    CHECK(picked->name == "b");
    CHECK_FALSE(p.open);
    CHECK(p.update(PickerMsg::pick(9))->name == "b");
    CHECK(p.selected == std::optional<std::size_t>(1));
}

TEST_CASE("form state records errors until valid") {
    FormState<NameSpec> f;
    const std::vector<std::string> existing{"taken"};
    CHECK(f.submit(existing) == std::nullopt);
    CHECK(f.error(NameField::Name) == std::optional<std::string_view>("required"));
    f.spec.name = "taken";
    CHECK(f.submit(existing) == std::nullopt);
    CHECK(f.error(NameField::Name) == std::optional<std::string_view>("taken"));
    f.spec.name = "fresh";
    CHECK(f.submit(existing) == std::optional<std::string>("fresh"));
    CHECK(f.errors.empty());
}

TEST_CASE("screens reduce locals and guard emitted events; deliver collects events") {
    Counter c;
    using M = Msg<int, std::string>;
    CHECK(c.update(M::event("reset")) == std::nullopt);
    CHECK(c.update(M::local(1)) == std::nullopt);
    CHECK(c.update(M::event("reset")) == std::optional<std::string>("reset"));
    const auto events = testkit::deliver(c, std::vector<M>{M::local(1), M::local(1), M::event("x")},
                                         [](Counter& s, M m) { return s.update(std::move(m)); });
    CHECK(events == std::vector<std::string>{"big", "x"});
}

TEST_CASE("widget factories: callbacks fire, missing callbacks disable") {
    qt_app();
    QWidget root;
    int clicks = 0;
    auto* ok = text_button("Save", ButtonKind::primary(), [&] { ++clicks; }, &root);
    auto* off = text_button("Disabled", ButtonKind::secondary(), {}, &root);
    (void)ok;
    CHECK_FALSE(off->isEnabled());
    CHECK(testkit::click_label(root, "Save"));
    CHECK_FALSE(testkit::click_label(root, "Disabled"));
    CHECK(clicks == 1);
    auto* row = menu_row(Icon::Folder, "Projects", true, [&] { clicks += 10; }, &root);
    CHECK(row->accessibleName() == "Projects");
    CHECK(testkit::click_label(root, "Projects"));
    CHECK(clicks == 11);
    CHECK(meter_role(0.2F) == theme::ColorRole::Accent);
    CHECK(meter_role(0.5F) == theme::ColorRole::Warn);
    CHECK(meter_role(2.0F) == theme::ColorRole::Danger);

    Picker<Option> p({{"a"}, {"b"}}, 0);
    p.open = true;
    std::vector<PickerMsg> got;
    QWidget host;
    auto* view = p.view("pick", [&](PickerMsg m) { got.push_back(m); }, &host);
    host.show();
    CHECK(testkit::click_label(host, "b · bee"));
    CHECK(testkit::click_label(*view, "a"));
    CHECK(got == std::vector<PickerMsg>{PickerMsg::pick(1), PickerMsg::toggle()});
}
