#pragma once

// Text selection across the selectable blocks of one message, in document order. Port of
// `crates/loa-ui-session/src/transcript/selection.rs` and `selection/group.rs` (gesture and
// copy logic; the view hit-tests and paints). Plain C++.

#include <cstddef>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace loa::ui::session::transcript {

// A caret: byte `offset` into the text of block `block`.
struct Endpoint {
    std::size_t block = 0;
    std::size_t offset = 0;
    friend auto operator<=>(const Endpoint&, const Endpoint&) = default;
};

// One message's selectable blocks and the gesture over them.
class Selection {
  public:
    // Registers a block of `text` (its links are `links`, byte ranges with a target) and
    // returns its index.
    struct Link {
        std::size_t begin = 0, end = 0;
        std::string url;
    };
    std::size_t add(std::string text, std::vector<Link> links = {});
    [[nodiscard]] std::size_t size() const noexcept { return blocks_.size(); }
    [[nodiscard]] const std::string& text(std::size_t block) const { return blocks_[block].text; }

    // A left press at `at` (screen point `x`, `y`): starts a gesture, dropping the old selection.
    void press(Endpoint at, float x, float y);
    // The pointer moved to `at` with the button held; selects once it moved 3 px or reached
    // another block. `at` is none while the pointer is outside every block.
    void drag(std::optional<Endpoint> at, float x, float y);
    // The button was released; returns the link to open when a press on a link did not drag.
    // A press that did not drag clears the selection.
    std::optional<std::string> release();
    // Drops the selection (focus lost).
    void clear();

    [[nodiscard]] bool selecting() const noexcept { return press_.has_value(); }
    [[nodiscard]] bool empty() const noexcept { return !anchor_ || !end_ || *anchor_ == *end_; }
    // The selected byte range of `block`, none when it has no selected text.
    [[nodiscard]] std::optional<std::pair<std::size_t, std::size_t>> range(std::size_t block) const;
    // The selected text of every block, joined by a blank line; empty without a selection.
    [[nodiscard]] std::string copy() const;

  private:
    struct Block {
        std::string text;
        std::vector<Link> links;
    };
    struct Press {
        float x = 0, y = 0;
        std::optional<std::string> link;
        bool dragged = false;
        std::size_t block = 0;
    };
    std::vector<Block> blocks_;
    std::optional<Endpoint> anchor_, end_;
    std::optional<Press> press_;
};

} // namespace loa::ui::session::transcript
