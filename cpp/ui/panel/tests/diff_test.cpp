// Port of `crates/loa-ui-panel/src/diff/tests.rs`.

#include <doctest/doctest.h>

#include <format>
#include <tuple>

#include "loa/ui/panel/diff.hpp"

using namespace loa::ui::panel::diff;

namespace {

using Hunks = std::vector<std::tuple<std::uint32_t, std::uint32_t, std::uint32_t, std::uint32_t>>;

Hunks hunks(const std::vector<Row>& rows) {
    Hunks out;
    for (const auto& r : rows)
        if (const auto* h = std::get_if<row::Hunk>(&r)) out.emplace_back(h->hunk.old_start, h->hunk.old_len, h->hunk.new_start, h->hunk.new_len);
    return out;
}
std::vector<std::size_t> folds(const std::vector<Row>& rows) {
    std::vector<std::size_t> out;
    for (const auto& r : rows)
        if (const auto* f = std::get_if<row::Fold>(&r)) out.push_back(f->hidden);
    return out;
}
std::string context(std::size_t from, std::size_t to) {
    std::string s;
    for (auto i = from; i <= to; ++i) s += std::format(" ctx {}\n", i);
    return s;
}

constexpr std::string_view GIT = "commit message\n"
                                 "diff --git a/src/lib.rs b/src/lib.rs\n"
                                 "--- a/src/lib.rs\n"
                                 "+++ b/src/lib.rs\n"
                                 "@@ -10,2 +10,3 @@ fn compute() {\n"
                                 "     let a = 1;\n"
                                 "-    let b = 2;\n"
                                 "+    let b = 3;\n"
                                 "+    let c = 4;\n"
                                 "@@ -40,1 +41,1 @@ impl Thing {\n"
                                 "-    old_tail()\n"
                                 "+    new_tail()\n"
                                 "\\ No newline at end of file\n"
                                 "diff --git a/docs/old.md b/docs/new.md\n"
                                 "rename from docs/old.md\n"
                                 "rename to docs/new.md\n"
                                 "diff --git a/added.txt b/added.txt\n"
                                 "new file mode 100644\n"
                                 "--- /dev/null\n"
                                 "+++ b/added.txt\n"
                                 "@@ -0,0 +1,2 @@\n"
                                 "+first\n"
                                 "+second\n"
                                 "diff --git a/logo.png b/logo.png\n"
                                 "deleted file mode 100644\n"
                                 "Binary files a/logo.png and /dev/null differ\n";

using FileRow = std::tuple<FileStatus, std::string, std::size_t, std::size_t, bool>;

} // namespace

TEST_CASE("max_line_no is the largest numbered line") {
    const auto patch = "--- a\n+++ b\n@@ -1,9 +1,12 @@\n" + context(1, 9) + "+x\n+y\n+z\n";
    const auto files = parse_unified(patch);
    REQUIRE(files);
    CHECK((*files)[0].max_line_no() == 12);
}

TEST_CASE("parses unified diffs") {
    using enum FileStatus;
    const auto folded = "--- a\n+++ b\n@@ -1,12 +1,12 @@\n-x\n+y\n" + context(2, 12);
    struct Case {
        std::string input;
        std::vector<FileRow> files;
        Hunks hunks;
        std::vector<std::size_t> folds;
    };
    const std::vector<Case> cases{
        {std::string(GIT),
         {{Modified, "src/lib.rs", 3, 2, false}, {Renamed, "docs/new.md", 0, 0, false}, {Added, "added.txt", 2, 0, false}, {Deleted, "logo.png", 0, 0, true}},
         {{10, 2, 10, 3}, {40, 1, 41, 1}},
         {}},
        {"--- a.txt\t2024-01-01\n+++ b.txt\t2024-01-02\n@@ -1 +1 @@\n-x\n+y\n", {{Renamed, "b.txt", 1, 1, false}}, {{1, 1, 1, 1}}, {}},
        {folded, {{Renamed, "b", 1, 1, false}}, {{1, 4, 1, 4}, {10, 3, 10, 3}}, {5}},
    };
    for (const auto& c : cases) {
        CAPTURE(c.input);
        const auto files = parse_unified(c.input);
        REQUIRE(files);
        std::vector<FileRow> got;
        for (const auto& f : *files) got.emplace_back(f.status(), std::string(f.path()), f.added(), f.removed(), f.binary);
        CHECK(got == c.files);
        const auto rows = (*files)[0].unified_rows();
        CHECK(hunks(rows) == c.hunks);
        CHECK(folds(rows) == c.folds);
    }
    const auto parsed = parse_unified(GIT);
    REQUIRE(parsed);
    const auto& lib = (*parsed)[0];
    std::vector<std::string_view> sections;
    for (const auto& r : lib.unified_rows())
        if (const auto* h = std::get_if<row::Hunk>(&r)) sections.push_back(lib.section(h->hunk));
    CHECK(sections == std::vector<std::string_view>{"fn compute() {", "impl Thing {"});
    auto emph = [&](std::size_t i) {
        std::vector<std::string_view> out;
        for (const auto r : lib.emphasis(i)) out.push_back(lib.line_text(i).substr(r.start, r.size()));
        return out;
    };
    CHECK(lib.lines()[1].old_no == std::optional<std::uint32_t>(11));
    CHECK_FALSE(lib.lines()[1].new_no);
    CHECK(lib.line_text(1) == "    let b = 2;");
    CHECK(emph(1) == std::vector<std::string_view>{"2;"});
    CHECK_FALSE(lib.lines()[3].old_no);
    CHECK(lib.lines()[3].new_no == std::optional<std::uint32_t>(12));
    CHECK(lib.line_text(3) == "    let c = 4;");
    CHECK(emph(3).empty());
    std::vector<row::Pair> split;
    for (const auto& r : lib.split_rows())
        if (const auto* p = std::get_if<row::Pair>(&r)) split.push_back(*p);
    REQUIRE(split.size() >= 3);
    CHECK(split[0] == row::Pair{0, 0});
    CHECK(split[1] == row::Pair{1, 2});
    CHECK(split[2] == row::Pair{std::nullopt, 3});

    using K = ParseError::Kind;
    const std::vector<std::pair<std::string_view, ParseError>> errors{
        {"--- a\n+++ b\n@@ -1,3 +1,3 @@\n a\n-b\n", {K::Truncated, 5, 1, 2}},
        {"@@ -1 +1 @@\n-a\n+b\n", {K::Orphan, 1}},
        {"--- a\n+++ b\n@@ -x +1 @@\n", {K::HunkHeader, 3}},
        {"--- a\n+++ b\n@@ -1,1 +1,2 @@\n a\n a\n", {K::Overflow, 5}},
    };
    for (const auto& [input, error] : errors) {
        CAPTURE(input);
        const auto r = parse_unified(input);
        REQUIRE_FALSE(r);
        CHECK(r.error() == error);
    }
    CHECK(ParseError{K::Truncated, 5, 1, 2}.message() == "line 5: hunk ends with 1 old and 2 new lines missing");
}

TEST_CASE("fifty thousand line diff builds only the rows in view") {
    const auto patch = "--- a\n+++ b\n@@ -1,50000 +1,50000 @@\n" + context(1, 24'999) + "-old\n+new\n" + context(25'001, 50'000);
    State state;
    auto files = parse_unified(patch);
    REQUIRE(files);
    const auto task = state.set_files(std::move(*files));
    CHECK(task == Task::of(loa::ui::shell::op::SnapTo{std::string(SCROLL_ID), 0.0F, 0.0F}));
    CHECK(state.rows()[4] == Row{row::Fold{0, 24'993}});
    (void)state.update(msg::Expand{0});
    (void)state.update(msg::Scrolled{20'000.0F * row_height(), 600.0F});
    const auto range = state.visible_rows();
    CHECK(range.size() <= static_cast<std::size_t>(600.0F / row_height()) + 2 * OVERSCAN + 1);
    CHECK(range.contains(20'000));
    CHECK(state.rows()[20'000] == Row{row::Line{19'999}});
    CHECK(state.selected()->line_text(19'999) == "ctx 20000");
}

TEST_CASE("state: open, modes, collapse and notes") {
    State state;
    auto files = parse_unified("--- a\n+++ a\n@@ -1 +1,2 @@\n-x\n+y\n+z\n--- b\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\n");
    REQUIRE(files);
    (void)state.set_files(std::move(*files));
    CHECK(state.files().size() == 2);
    CHECK(state.update(msg::Open{}).second == Event{loa::view::panel::diff_intent::OpenFile{"a"}});
    (void)state.update(msg::SetMode{Mode::Split});
    CHECK(state.mode() == Mode::Split);
    CHECK(std::holds_alternative<row::Pair>(state.rows()[1]));
    (void)state.update(msg::ToggleCollapsed{});
    CHECK(state.collapsed());
    CHECK_FALSE(state.update(msg::SelectFile{1}).first.empty());
    CHECK_FALSE(state.collapsed());
    CHECK(state.selected()->status() == FileStatus::Deleted);
    CHECK(state.update(msg::Open{}).second == Event{loa::view::panel::diff_intent::OpenFile{"b"}});
    (void)state.set_note("Not a git repository");
    CHECK(state.files().empty());
    CHECK_FALSE(state.selected());
}
