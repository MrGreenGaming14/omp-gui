// Port of the tests in `loa-ui-kit/src/text/{mod,fuzzy,ellipsis}.rs`.

#include <doctest/doctest.h>

#include <cmath>
#include <limits>
#include <string>

#include "fixture.hpp"
#include "loa/ui/kit/text.hpp"

using namespace loa::ui::kit::text;
using loa::ui::kit::theme::Font;

namespace {

std::optional<Match> m(std::string_view q, std::string_view title) {
    Matcher matcher;
    matcher.set_query(q);
    return matcher.matches(title);
}

using Idx = std::vector<std::size_t>;

// Code points of `s` with their UTF-8 length.
template <typename F> float sum_chars(std::string_view s, F&& w) {
    float total = 0;
    for (std::size_t i = 0; i < s.size();) {
        const auto b = static_cast<unsigned char>(s[i]);
        const std::size_t len = b < 0x80 ? 1 : (b >> 5) == 0x6 ? 2 : (b >> 4) == 0xE ? 3 : 4;
        total += w(s.substr(i, len));
        i += len;
    }
    return total;
}

float mono(std::string_view s) {
    return sum_chars(s, [](std::string_view) { return 10.0F; });
}

float weighted(std::string_view s) {
    return sum_chars(s, [](std::string_view c) {
        if (c == "i" || c == "l" || c == "." || c == "/" || c == "\\") return 4.0F;
        if (c == "W" || c == "M") return 14.0F;
        if (c == ELLIPSIS) return 10.0F;
        if (c.size() > 1) return 20.0F;
        return 8.0F;
    });
}

std::string e(std::string_view s, float w, EllipsisMode mode, float (*measure)(std::string_view) = mono) {
    return ellipsize_with(s, w, mode, measure);
}

} // namespace

TEST_CASE("fuzzy: classes, indices and scores") {
    const std::pair<std::string_view, Class> classes[] = {
        {"tog", Class::Prefix}, {"PAN", Class::WordPrefix}, {"tp", Class::WordBoundary}, {"togpan", Class::WordBoundary}, {"gl", Class::Fuzzy}};
    for (const auto& [q, c] : classes) {
        INFO(q);
        REQUIRE(m(q, "Toggle panel"));
        CHECK(m(q, "Toggle panel")->cls == c);
    }
    CHECK(m("sett", "openSettings")->cls == Class::WordPrefix);
    CHECK(!m("xyz", "Toggle panel"));
    CHECK(!m("lenap", "Toggle panel"));
    CHECK(m("tp", "Toggle panel")->indices == Idx{0, 7});
    CHECK(m("on", "Open New")->indices == Idx{0, 5});
    CHECK(m("tog pan", "Toggle panel")->indices == Idx{0, 1, 2, 7, 8, 9});
    CHECK(m("abc", "xabcx")->score > m("abc", "xaxbxcx")->score);
    const auto u = m("üb", "Über Ansicht");
    REQUIRE(u);
    CHECK(u->cls == Class::Prefix);
    CHECK(u->indices == Idx{0, 1});
    CHECK(m("", "anything")->indices.empty());
}

TEST_CASE("fuzzy: highlight runs split on char boundaries") {
    const std::string title = "Über panel";
    const auto hit = m("üpa", title);
    REQUIRE(hit);
    std::vector<std::pair<std::string, bool>> runs;
    for (const auto& r : highlight_runs(title, hit->indices)) runs.emplace_back(title.substr(r.begin, r.end - r.begin), r.matched);
    CHECK(runs == std::vector<std::pair<std::string, bool>>{{"Ü", true}, {"ber ", false}, {"pa", true}, {"nel", false}});
    CHECK(highlight_runs("abc", {}) == std::vector<Run>{{0, 3, false}});
}

TEST_CASE("formats, truncates and filters") {
    CHECK(format_duration(400) == "0.4s");
    CHECK(format_duration(12'000) == "12s");
    CHECK(format_duration(125'000) == "2m 5s");
    CHECK(format_duration(3'780'000) == "1h 3m");
    CHECK(format_bytes(512) == "512 B");
    CHECK(format_bytes(1536) == "1.5 KB");
    CHECK(format_bytes(3ULL << 20) == "3.0 MB");
    CHECK(format_bytes(5ULL << 40) == "5120.0 GB");
    CHECK(truncate_trailing("abcdef", 4) == "abc…");
    CHECK(truncate_trailing("ab", 4) == "ab");
    CHECK(truncate_leading("src/main.rs", 7) == "…ain.rs");
    CHECK(truncate_leading("ab", 4) == "ab");
    const std::string_view files[] = {"crates/composer/src/model.rs", "src/main.rs"};
    CHECK(filter("main", files, 10) == Idx{1});
    const std::string_view letters[] = {"b", "a", "c"};
    CHECK(filter("", letters, 2) == Idx{0, 1});
}

TEST_CASE("ellipsis: fits without truncation in both modes") {
    const std::string_view s = "crates/loa-ui-kit/src/text/mod.rs";
    CHECK(e(s, mono(s), EllipsisMode::End) == s);
    CHECK(e(s, mono(s), EllipsisMode::Path) == s);
    CHECK(e(s, std::numeric_limits<float>::infinity(), EllipsisMode::End) == s);
    CHECK(e("", 100, EllipsisMode::Path).empty());
}

TEST_CASE("ellipsis: end mode respects the measured budget") {
    CHECK(e("abcdef", 40, EllipsisMode::End) == "abc…");
    const auto out = e("WiiiiiMMM", 40, EllipsisMode::End, weighted);
    CHECK(out.ends_with(ELLIPSIS));
    CHECK(weighted(out) <= 40.0F);
    CHECK(out == "Wiiii…");
}

TEST_CASE("ellipsis: path mode preserves the last component for both separators") {
    const std::string_view unix_path = "/Users/alice/projects/loa-ui-kit";
    CHECK(e(unix_path, 130, EllipsisMode::Path) == "/…/loa-ui-kit");
    CHECK(e(unix_path, 200, EllipsisMode::Path) == "/Users/a…/loa-ui-kit");
    const std::string_view windows = R"(C:\Users\alice\projects\loa-ui-kit)";
    CHECK(e(windows, 130, EllipsisMode::Path) == R"(C…\loa-ui-kit)");
    CHECK(e(windows, 170, EllipsisMode::Path) == R"(C:\Us…\loa-ui-kit)");
    CHECK(e(R"(C:\Users/alice\projects/my-repo)", 110, EllipsisMode::Path) == "C:…/my-repo");
    CHECK(e("/alpha/beta/gamma/", 80, EllipsisMode::Path) == "/…/gamma");
    // When even "…/<tail>" is wider than max_width, falls back to the tail component.
    CHECK(e("/a/b/long_component", 100, EllipsisMode::Path) == "long_comp…");
    CHECK(e("single_segment_name", 80, EllipsisMode::Path) == "single_…");
}

TEST_CASE("ellipsis: tiny and invalid widths never overflow") {
    for (const float w : {0.0F, -5.0F, std::numeric_limits<float>::quiet_NaN(), 4.0F, 9.9F}) {
        CHECK(e("hello/world", w, EllipsisMode::End).empty());
        CHECK(e("hello/world", w, EllipsisMode::Path).empty());
    }
    CHECK(e("hello", 10, EllipsisMode::End) == ELLIPSIS);
    CHECK(e("a/very_long_tail", 10, EllipsisMode::Path) == ELLIPSIS);
}

TEST_CASE("ellipsis: multibyte unicode is safe") {
    const std::string_view title = "café 🚀 日本語プロジェクト";
    for (const float w : {0.0F, 10.0F, 35.0F, 60.0F, 120.0F, 500.0F}) {
        const auto out = e(title, w, EllipsisMode::End, weighted);
        INFO(w, ": ", out);
        CHECK(weighted(out) <= w);
    }
    const std::string_view path = "/home/ünicode/日本語/résumé_📄.rs";
    const auto kept = e(path, 220, EllipsisMode::Path, weighted);
    CHECK(kept.ends_with("/résumé_📄.rs"));
    CHECK(kept.find(ELLIPSIS) != std::string::npos);
    CHECK(weighted(kept) <= 220.0F);
    CHECK(weighted(e(path, 90, EllipsisMode::Path, weighted)) <= 90.0F);
}

TEST_CASE("ellipsis: real font measurement fits and ellipsizes") {
    qt_app();
    const std::string_view s = "A reasonably long session title for narrow windows";
    const float full = measure_text_width(s, 14, Font::Default);
    CHECK(full > 100.0F);
    CHECK(ellipsize_text(s, full + 1, 14, Font::Default, EllipsisMode::End) == s);
    const auto cut = ellipsize_text(s, full * 0.4F, 14, Font::Default, EllipsisMode::End);
    CHECK(cut.ends_with(ELLIPSIS));
    CHECK(cut != s);
    CHECK(measure_text_width(cut, 14, Font::Default) <= full * 0.4F + 0.5F);

    const std::string_view path = "~/.omp/agent/OMGUI-worktrees/issue-331-narrow-overflow";
    const float path_w = measure_text_width(path, 13, Font::Default);
    const float budget = measure_text_width("…/issue-331-narrow-overflow", 13, Font::Default) + 24;
    CHECK(budget < path_w);
    const auto cut_path = ellipsize_text(path, budget, 13, Font::Default, EllipsisMode::Path);
    CHECK(cut_path.ends_with("/issue-331-narrow-overflow"));
    CHECK(cut_path.find(ELLIPSIS) != std::string::npos);
    CHECK(measure_text_width(cut_path, 13, Font::Default) <= budget + 0.5F);
}
