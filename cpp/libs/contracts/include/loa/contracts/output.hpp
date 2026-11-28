#pragma once

#include <cstddef>
#include <string>
#include <variant>
#include <vector>

namespace loa::contracts {

struct TextBlock {
    std::string text;
    friend bool operator==(const TextBlock&, const TextBlock&) = default;
};

// Image `data` is raw bytes, not base64.
struct ImageBlock {
    std::string mime;
    std::vector<std::byte> data;
    friend bool operator==(const ImageBlock&, const ImageBlock&) = default;
};

// One block of a tool result.
using OutputBlock = std::variant<TextBlock, ImageBlock>;

struct ToolOutput {
    std::vector<OutputBlock> blocks;
    bool is_error = false;

    [[nodiscard]] static ToolOutput text(std::string text);
    [[nodiscard]] static ToolOutput error(std::string text);
    // The text blocks joined by newlines.
    [[nodiscard]] std::string joined_text() const;

    friend bool operator==(const ToolOutput&, const ToolOutput&) = default;
};

} // namespace loa::contracts
