#pragma once

// Non-command frames the host writes: answers to UI requests and results of host tool callbacks.

#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/harness/omp/rpc/command.hpp"

namespace loa::harness::omp::rpc {

// One answer to an `ask` question.
struct AskAnswer {
    std::string id;
    std::vector<std::string> selected_options;
    std::optional<std::string> custom_input;
    friend bool operator==(const AskAnswer&, const AskAnswer&) = default;
};

// An `extension_ui_response`.
namespace ui_response {
// Answers `select`, `input` or `editor`.
struct Value {
    std::string id;
    std::string value;
    friend bool operator==(const Value&, const Value&) = default;
};
// Answers `confirm`.
struct Confirmed {
    std::string id;
    bool confirmed = false;
    friend bool operator==(const Confirmed&, const Confirmed&) = default;
};
// Dismisses any dialog. `timed_out` reports the host's own deadline.
struct Cancelled {
    std::string id;
    bool timed_out = false;
    friend bool operator==(const Cancelled&, const Cancelled&) = default;
};
// Answers `ask`, one entry per question in request order.
struct Answers {
    std::string id;
    std::vector<AskAnswer> answers;
    friend bool operator==(const Answers&, const Answers&) = default;
};
} // namespace ui_response

using UiResponse = std::variant<ui_response::Value, ui_response::Confirmed, ui_response::Cancelled, ui_response::Answers>;

// The `extension_ui_response` frame.
[[nodiscard]] Json to_json(const UiResponse& response);

struct TextContent {
    std::string text;
    friend bool operator==(const TextContent&, const TextContent&) = default;
};

// One block of a host tool result, what the model sees: `{"type":"text","text":…}` or
// `{"type":"image","data":<base64>,"mimeType":…}`.
using ToolContent = std::variant<TextContent, ImageContent>;

[[nodiscard]] inline ToolContent text_content(std::string text) { return TextContent{std::move(text)}; }

// Outcome of a host tool call.
struct HostToolOutput {
    // The result blocks shown to the model.
    std::vector<ToolContent> content;
    // Surfaces the text blocks as a tool error.
    bool is_error = false;
    // Optional structured details kept with the result.
    std::optional<Json> details;

    [[nodiscard]] static HostToolOutput ok(std::string text) { return blocks({text_content(std::move(text))}); }
    [[nodiscard]] static HostToolOutput error(std::string text) {
        return HostToolOutput{{text_content(std::move(text))}, true, std::nullopt};
    }
    // A successful result of text and image blocks.
    [[nodiscard]] static HostToolOutput blocks(std::vector<ToolContent> content) {
        return HostToolOutput{std::move(content), false, std::nullopt};
    }

    // The `host_tool_result` frame answering callback `id`.
    [[nodiscard]] Json result_json(std::string_view id) const;
    friend bool operator==(const HostToolOutput&, const HostToolOutput&) = default;
};

} // namespace loa::harness::omp::rpc
