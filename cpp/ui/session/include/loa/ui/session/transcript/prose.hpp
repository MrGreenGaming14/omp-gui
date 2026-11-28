#pragma once

// Assistant Markdown, parsed with md4c into a small block tree, incrementally while it
// streams. Port of `crates/loa-ui-session/src/transcript/prose.rs` (parse half; the Qt drawing
// lives in the view). Plain C++.

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

namespace loa::ui::session::transcript {

namespace md {

// A run of inline text with one format.
struct Span {
    std::string text;
    bool strong = false, emphasis = false, code = false, strike = false;
    // The target of a link or image.
    std::optional<std::string> link;
    friend bool operator==(const Span&, const Span&) = default;
};
using Text = std::vector<Span>;

struct Block;

struct Paragraph {
    Text text;
    friend bool operator==(const Paragraph&, const Paragraph&) = default;
};
struct Heading {
    int level = 1;
    Text text;
    friend bool operator==(const Heading&, const Heading&) = default;
};
struct CodeBlock {
    std::optional<std::string> language;
    std::string code;
    friend bool operator==(const CodeBlock&, const CodeBlock&) = default;
};
// One list item; `task` holds the checkbox state of a task item.
struct Bullet {
    std::optional<bool> task;
    std::vector<Block> items;
    friend bool operator==(const Bullet&, const Bullet&);
};
struct List {
    // The first number of an ordered list.
    std::optional<unsigned> start;
    std::vector<Bullet> bullets;
    friend bool operator==(const List&, const List&) = default;
};
struct Quote {
    std::vector<Block> items;
    friend bool operator==(const Quote&, const Quote&);
};
struct Rule {
    friend bool operator==(const Rule&, const Rule&) = default;
};
struct Table {
    // Header rows first.
    std::size_t header_rows = 0;
    std::vector<std::vector<Text>> rows;
    friend bool operator==(const Table&, const Table&) = default;
};

struct Block {
    std::variant<Paragraph, Heading, CodeBlock, List, Quote, Rule, Table> v;
    friend bool operator==(const Block&, const Block&) = default;
};

// Parses CommonMark with the GitHub extensions (tables, strikethrough, task lists, autolinks).
[[nodiscard]] std::vector<Block> parse(std::string_view source);
// The plain text of `text`.
[[nodiscard]] std::string plain(const Text& text);
// A readable dump of `blocks`, for test failure messages.
[[nodiscard]] std::string debug(const std::vector<Block>& blocks);

} // namespace md

// Every newline as a markdown hard break, so replies keep the model's line breaks.
[[nodiscard]] std::string hard_breaks(std::string_view text);
// Undoes the hard-break decoration on fenced code only, keeping blank lines and the author's
// own spaces before a line break.
[[nodiscard]] std::string code_text(std::string_view code);
// The `index`th code block of `blocks` in document order, through lists and quotes.
[[nodiscard]] std::optional<std::string> take_code(const std::vector<md::Block>& blocks, std::size_t index);
// What "Copy code block" copies for the `index`th code block of the message `source`: the code
// as the author wrote it, whatever the display decoration; `shown` when the source has no such block.
[[nodiscard]] std::string copy_code(std::string_view source, std::size_t index, std::string_view shown);

// Parsed markdown of one message, fed only the text that arrives. Blocks that end before a
// blank line followed by a fresh top-level line are final; only the blocks after that point
// are parsed again when text arrives.
class Streamed {
  public:
    explicit Streamed(std::string_view text = {});

    // Catches up with `text` after `chunk` was appended: pushes the chunk when the parse holds
    // exactly the text before it, parses `text` afresh otherwise.
    void append(std::string_view text, std::string_view chunk);

    [[nodiscard]] const std::vector<md::Block>& blocks() const noexcept { return blocks_; }
    // Bytes of message text handed to the parser so far.
    [[nodiscard]] std::size_t fed() const noexcept { return fed_; }
    // Newlines so far, for height estimates.
    [[nodiscard]] std::size_t lines() const noexcept { return lines_; }

  private:
    void push(std::string_view chunk);
    // Moves `cut_` to the last line start no later text can merge into an earlier block.
    void scan();

    std::string source_; // the hard-broken text
    std::vector<md::Block> blocks_;
    std::size_t stable_ = 0; // blocks_[..stable_] end before cut_
    std::size_t cut_ = 0;
    std::size_t scanned_ = 0; // start of the first line not scanned yet
    bool prev_blank_ = false;
    char fence_char_ = 0;
    std::size_t fence_len_ = 0;
    std::size_t fed_ = 0, lines_ = 0;
};

} // namespace loa::ui::session::transcript
