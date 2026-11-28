// Diff ops checked against `similar` 3.2.1 output recorded from Rust (`TextDiff::configure()
// .algorithm(Algorithm::Myers)`), on the loa-ui-panel diff test lines and harder cases;
// gitignore cases from the `ignore` 0.4 / gitignore documentation.

#include <doctest/doctest.h>

#include <algorithm>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <string>

#include "loa/runtime/diff.hpp"
#include "loa/runtime/gitignore.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::runtime;

namespace {

const char* tag_name(diff::Tag t) {
    switch (t) {
    case diff::Tag::equal: return "Equal";
    case diff::Tag::delete_: return "Delete";
    case diff::Tag::insert: return "Insert";
    case diff::Tag::replace: return "Replace";
    }
    return "";
}

// Ops as similar's test program printed them: `Tag o..o n..n; ...`.
std::string show(const diff::TextDiff& d) {
    std::string out;
    for (const auto& op : d.ops) {
        if (!out.empty()) out += "; ";
        out += std::string(tag_name(op.tag)) + " " + std::to_string(op.old_index) + ".." + std::to_string(op.old_end()) + " " +
               std::to_string(op.new_index) + ".." + std::to_string(op.new_end());
    }
    return out;
}

void spit(const std::filesystem::path& p, std::string_view s) { std::ofstream(p, std::ios::binary) << s; }

} // namespace

TEST_CASE("word diffs match similar") {
    struct Case {
        const char *old, *nw, *ops;
        float ratio;
    };
    const Case cases[] = {
        {"    let b = 2;", "    let b = 3;", "Equal 0..7 0..7; Replace 7..8 7..8", 0.875f},
        {"    old_tail()", "    new_tail()", "Equal 0..1 0..1; Replace 1..2 1..2", 0.5f},
        {"x", "y", "Replace 0..1 0..1", 0.0f},
        {"old", "new", "Replace 0..1 0..1", 0.0f},
        {"the quick brown fox", "the slow brown dog jumps", "Equal 0..2 0..2; Replace 2..3 2..3; Equal 3..6 3..6; Replace 6..7 6..9", 0.625f},
        {"a b c d e f", "a c b d f e",
         "Equal 0..2 0..2; Insert 2..2 2..4; Equal 2..4 4..6; Replace 4..5 6..7; Equal 5..6 7..8; Replace 6..7 8..9; Equal 7..9 9..11; Delete 9..11 11..11",
         0.6363636f},
        {"h\xC3\xA9llo w\xC3\xB6rld\xC2\xA0x", "h\xC3\xA9llo world\xC2\xA0x", "Equal 0..2 0..2; Replace 2..3 2..3; Equal 3..5 3..5", 0.8f},
    };
    for (const auto& c : cases) {
        const auto d = diff::diff_words(c.old, c.nw);
        CHECK_MESSAGE(show(d) == c.ops, c.old);
        CHECK(d.ratio() == doctest::Approx(c.ratio));
    }
}

TEST_CASE("line diffs match similar") {
    struct Case {
        const char *old, *nw, *ops;
    };
    const Case cases[] = {
        {"a\nb\nc\n", "a\nB\nc\nd\n", "Equal 0..1 0..1; Replace 1..2 1..2; Equal 2..3 2..3; Insert 3..3 3..4"},
        {"    let a = 1;\n    let b = 2;\n", "    let a = 1;\n    let b = 3;\n    let c = 4;\n", "Equal 0..1 0..1; Replace 1..2 1..3"},
        {"x\r\ny\rz", "x\r\nq\rz\n", "Equal 0..1 0..1; Replace 1..3 1..3"},
        {"a\nb\na\nb\na\n", "b\na\nb\na\nb\n", "Insert 0..0 0..1; Equal 0..4 1..5; Delete 4..5 5..5"},
        {"", "only\n", "Insert 0..0 0..1"},
        {"1\n2\n3\n4\n5\n6\n7\n8\n9\n", "1\n2\n0\n3\n4\n6\n7\n9\n9\n",
         "Equal 0..2 0..2; Insert 2..2 2..3; Equal 2..4 3..5; Delete 4..5 5..5; Equal 5..7 5..7; Delete 7..8 7..7; Equal 8..9 7..8; Insert 9..9 8..9"},
    };
    for (const auto& c : cases) CHECK_MESSAGE(show(diff::diff_lines(c.old, c.nw)) == c.ops, c.old);
}

TEST_CASE("a large repetitive line diff (preflight and heuristics) matches similar") {
    std::string old, nw;
    for (int i = 0; i < 1500; ++i) old += "line " + std::to_string(i % 37) + "\n";
    for (int i = 0; i < 1400; ++i) nw += "line " + std::to_string((i * 7) % 41) + "\n";
    const auto d = diff::diff_lines(old, nw);
    std::uint64_t h = 1469598103934665603ULL;
    for (const auto& op : d.ops) {
        const auto s = std::string(tag_name(op.tag)) + std::to_string(op.old_index) + std::to_string(op.old_end()) +
                       std::to_string(op.new_index) + std::to_string(op.new_end());
        for (const unsigned char b : s) h = (h ^ b) * 1099511628211ULL;
    }
    CHECK(d.ops.size() == 610);
    CHECK(d.ratio() == doctest::Approx(0.21034482f));
    CHECK(h == 12448693472909079110ULL);
}

TEST_CASE("gitignore: patterns, negation, ** and directory-only rules") {
    const auto gi = gitignore::Gitignore::parse("/r", "# comment\n*.log\n!keep.log\nbuild/\n/top.txt\na/**/b\nfoo/**\n\\#hash\ntrail\\ \n");
    using gitignore::Match;
    CHECK(gi.matched("x.log", false) == Match::ignore);
    CHECK(gi.matched("/r/deep/x.log", false) == Match::ignore);
    CHECK(gi.matched("deep/keep.log", false) == Match::whitelist);
    CHECK(gi.matched("build", true) == Match::ignore);
    CHECK(gi.matched("build", false) == Match::none);
    CHECK(gi.matched("sub/build", true) == Match::ignore);
    CHECK(gi.matched("top.txt", false) == Match::ignore);
    CHECK(gi.matched("sub/top.txt", false) == Match::none);
    CHECK(gi.matched("a/b", false) == Match::ignore);
    CHECK(gi.matched("a/x/y/b", false) == Match::ignore);
    CHECK(gi.matched("foo/x/y", false) == Match::ignore);
    CHECK(gi.matched("foo", true) == Match::none);
    CHECK(gi.matched("#hash", false) == Match::ignore);
    CHECK(gi.matched("trail ", false) == Match::ignore);
}

TEST_CASE("gitignore: nested files, .ignore precedence and the repository boundary") {
    auto tmp = TempDir::create().value();
    const auto root = tmp.path();
    namespace stdfs = std::filesystem;
    stdfs::create_directories(root / "repo/.git/info");
    stdfs::create_directories(root / "repo/sub/build");
    spit(root / ".gitignore", "*.md\n");                    // outside the repository: not applied
    spit(root / "repo/.gitignore", "*.log\nbuild/\n");
    spit(root / "repo/sub/.gitignore", "!keep.log\n");
    spit(root / "repo/sub/.ignore", "keep.log\n");          // .ignore beats .gitignore
    spit(root / "repo/.git/info/exclude", "secret\n");
    for (const auto* f : {"a.log", "keep.log", "other.log", "x.md", "secret", "plain", ".hidden"}) spit(root / "repo/sub" / f, "x");
    spit(root / "repo/sub/.ignore", "keep.log\n!other.log\n");

    auto listed = gitignore::list_dir(root / "repo/sub").value();
    std::vector<std::string> names;
    for (const auto& e : listed) names.push_back(e.name + (e.is_dir ? "/" : ""));
    std::ranges::sort(names);
    CHECK(names == std::vector<std::string>{".gitignore", ".hidden", ".ignore", "other.log", "plain", "x.md"});
    CHECK(gitignore::list_dir(root / "missing").error().code() == loa::ErrorCode::not_found);
}
