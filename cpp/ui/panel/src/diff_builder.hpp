#pragma once

// Collects lines into a `FileDiff`, pairing each change block's deleted and inserted lines for
// intra-line ranges. Shared by the parser and the model.

#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/ui/panel/diff.hpp"

namespace loa::ui::panel::diff {

class Builder {
  public:
    void push(Tag tag, std::optional<std::uint32_t> old_no, std::optional<std::uint32_t> new_no, std::string_view text);
    // Starts a new hunk at the next line, as a `@@` header does.
    void hunk_break(std::string_view section);
    [[nodiscard]] FileDiff finish(std::optional<std::string> old_path, std::optional<std::string> new_path, bool binary);

  private:
    void flush_block();

    FileDiff file_;
    std::vector<std::size_t> dels_, ins_;
};

} // namespace loa::ui::panel::diff
