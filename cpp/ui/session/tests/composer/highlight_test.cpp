// Port of the tests in `composer/highlight.rs` and `composer/shimmer.rs`.

#include <cmath>

#include "support.hpp"

using namespace composer_test;

namespace {

std::vector<std::pair<std::string, std::string>> kinds(std::string_view line, bool first) {
    std::vector<std::pair<std::string, std::string>> out;
    for (const auto& s : spans(line, first, 0)) {
        std::string kind = std::visit(
            []<class H>(const H&) -> std::string {
                if constexpr (std::is_same_v<H, highlight::Command>) return "command";
                else if constexpr (std::is_same_v<H, highlight::Mention>) return "mention";
                else if constexpr (std::is_same_v<H, highlight::Image>) return "image";
                else return "keyword";
            },
            s.highlight);
        out.emplace_back(std::string(line.substr(s.begin, s.end - s.begin)), kind);
    }
    return out;
}

} // namespace

TEST_CASE("a leading slash command, mentions and images are marked") {
    const std::vector<std::pair<std::string, std::string>> expected{
        {"/plan", "command"}, {"@src/a.rs", "mention"}, {"[Image #2]", "image"}, {"@\"a b.png\"", "mention"}};
    CHECK(kinds("/plan fix @src/a.rs, see [Image #2] and @\"a b.png\"", true) == expected);
    CHECK_MESSAGE(kinds("/plan", false).empty(), "only the message's first line starts a command");
    CHECK(kinds("a/b me@x.rs [Image #x]", true).empty());
}

TEST_CASE("keywords count only when they stand alone") {
    const auto alone = kinds("please workflowz, then ultrathink.", true);
    const auto keywords = std::ranges::count_if(alone, [](const auto& k) { return k.second == "keyword"; });
    CHECK(keywords == static_cast<long>(std::string_view("workflowz").size() + std::string_view("ultrathink").size()));
    for (const auto* joined : {"workflowzz", "my_workflowz", "a/workflowz", "workflowz()", "x::workflowz", "workflowz.rs", "workflowz-ish"})
        CHECK_MESSAGE(!has_keyword(joined), joined);
    CHECK(has_keyword("(workflowz)"));
}

TEST_CASE("the gradient spans the hue range and moves with the phase") {
    const auto at = [](std::size_t index, std::uint8_t phase) { return gradient(3, index, 9, phase); };
    CHECK_MESSAGE(at(0, 0) != at(8, 0), "the word is a gradient");
    CHECK_MESSAGE(at(0, 0) != at(0, shimmer::PHASES / 2), "the shimmer moves it");
    CHECK_MESSAGE(at(0, 0) == at(0, shimmer::PHASES), "and loops");
    const auto red = hsl(0.0F, 1.0F, 0.5F);
    CHECK(std::fabs(red.r - 1.0F) < 1e-4F);
    CHECK(std::fabs(red.g) < 1e-4F);
    CHECK(std::fabs(red.b) < 1e-4F);
}

TEST_CASE("the phase steps through one cycle and loops") {
    const auto cycle = shimmer::CYCLE_MS;
    CHECK(shimmer::phase_at(0) == 0);
    CHECK(shimmer::phase_at(cycle / 2) == shimmer::PHASES / 2);
    CHECK(shimmer::phase_at(cycle - 1) == shimmer::PHASES - 1);
    CHECK(shimmer::phase_at(cycle) == 0);
    CHECK(shimmer::progress(shimmer::PHASES / 2) == 0.5F);
    CHECK(shimmer::PHASES == 56);
    CHECK(shimmer::EVERY_STEP.count() == 32);
}

TEST_CASE("the highlighter recolours from the top when the phase moves") {
    Highlighter h(Settings{0});
    (void)h.highlight_line("/plan");
    (void)h.highlight_line("/plan");
    CHECK(h.current_line() == 2);
    h.update(Settings{0});
    CHECK(h.current_line() == 2);
    h.update(Settings{5});
    CHECK(h.current_line() == 0);
    CHECK(h.highlight_line("/plan").size() == 1);
    CHECK_MESSAGE(h.highlight_line("/plan").empty(), "a later line starts no command");
    const auto t = loa::ui::kit::theme::tokens();
    CHECK(format(highlight::Image{6}, *t).color == image_color(1));
    CHECK(format(highlight::Command{}, *t).bold);
}
