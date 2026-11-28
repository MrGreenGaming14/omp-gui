// Markdown parsing: chunked appends match a whole parse; code copies keep the author's text.

#include <doctest/doctest.h>

#include <string>

#include "loa/ui/session/transcript/prose.hpp"

using namespace loa::ui::session::transcript;

namespace {

// Feeds `text` to a `Streamed` in chunks of `size` bytes, as a stream of appends would.
Streamed chunked(std::string_view text, std::size_t size) {
    Streamed s;
    std::string so_far;
    for (std::size_t at = 0; at < text.size(); at += size) {
        const auto chunk = text.substr(at, size);
        so_far += chunk;
        s.append(so_far, chunk);
    }
    return s;
}

} // namespace

TEST_CASE("appending in chunks yields the blocks of a whole parse") {
    const std::string_view fixtures[] = {
        "## Part\n\nSome **bold** and `code` - one\n- two\n\n```rust\nfn main() {}\n```\n\n## Part\n\n",
        "Intro paragraph\nwith a second line.\n\n- a\n- b\n\n  nested para\n\n1. first\n2. second\n\n> quoted\n> more\n\nafter\n",
        "```text\n\nblank lines\n\n\ninside a fence\n```\nTail *em* and ~~gone~~ and [link](https://example.com).\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        "- [x] done\n- [ ] todo\n\nText &amp; entities &#x41;.\n",
    };
    for (const auto text : fixtures) {
        const auto whole = md::parse(hard_breaks(text));
        for (const std::size_t size : {1, 2, 3, 7, 16, 1000}) {
            const auto s = chunked(text, size);
            CHECK(s.fed() == text.size());
            CHECK_MESSAGE(md::debug(s.blocks()) == md::debug(whole), "chunks of ", size);
            CHECK(s.blocks() == whole);
        }
    }
}

TEST_CASE("an append that does not continue the parse starts over") {
    Streamed s("# Title\n\nbody");
    s.append("replaced entirely", "entirely");
    CHECK(s.fed() == std::string_view("replaced entirely").size());
    CHECK(s.blocks() == md::parse("replaced entirely"));
}

TEST_CASE("the parse carries spans, lists and code") {
    const auto blocks = md::parse(hard_breaks("# Title\n\nSome **bold** [x](https://e.x)\n\n- one\n- [x] two\n\n```sh\nls\n```\n"));
    REQUIRE(blocks.size() == 4);
    const auto& h = std::get<md::Heading>(blocks[0].v);
    CHECK(h.level == 1);
    CHECK(md::plain(h.text) == "Title");
    const auto& p = std::get<md::Paragraph>(blocks[1].v);
    CHECK(p.text.size() >= 3);
    CHECK(p.text[1].strong);
    CHECK(p.text.back().link == "https://e.x");
    const auto& l = std::get<md::List>(blocks[2].v);
    REQUIRE(l.bullets.size() == 2);
    CHECK_FALSE(l.bullets[0].task);
    CHECK(l.bullets[1].task == true);
    const auto& c = std::get<md::CodeBlock>(blocks[3].v);
    CHECK(c.language == "sh");
    CHECK(code_text(c.code) == "ls\n");
}

TEST_CASE("hard breaks keep the model's line breaks") {
    CHECK(hard_breaks("a\nb\r\nc") == "a  \nb  \nc");
    CHECK(code_text("a  \n\n b  \n \n") == "a\n\n b\n\n");
}

TEST_CASE("copying fenced code keeps indent, blank lines and author spaces") {
    const std::pair<std::string_view, std::string_view> cases[] = {
        {"Example:\n\n```text\n    first  \n\n\tsecond\n```\n", "    first  \n\n\tsecond\n"},
        {" ```text\n a\n\n b\n ```", "a\n\nb\n"},
        {"```sh\nprintf x |", "printf x |"},
    };
    for (const auto& [source, expected] : cases) {
        const Streamed shown(source);
        const auto code = take_code(shown.blocks(), 0);
        REQUIRE(code.has_value());
        CHECK(copy_code(source, 0, *code) == expected);
    }
}
