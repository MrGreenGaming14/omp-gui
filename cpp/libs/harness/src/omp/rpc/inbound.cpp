#include "loa/harness/omp/rpc/inbound.hpp"

#include <type_traits>

namespace loa::harness::omp::rpc {

namespace json = loa::runtime::json;

Json to_json(const UiResponse& response) {
    return std::visit([](const auto& r) -> Json {
        using R = std::decay_t<decltype(r)>;
        json::Object obj{{"type", "extension_ui_response"}, {"id", r.id}};
        if constexpr (std::is_same_v<R, ui_response::Value>) obj.emplace_back("value", r.value);
        else if constexpr (std::is_same_v<R, ui_response::Confirmed>) obj.emplace_back("confirmed", r.confirmed);
        else if constexpr (std::is_same_v<R, ui_response::Cancelled>) {
            obj.emplace_back("cancelled", true);
            if (r.timed_out) obj.emplace_back("timedOut", true);
        } else if constexpr (std::is_same_v<R, ui_response::Answers>) {
            json::Array answers;
            answers.reserve(r.answers.size());
            for (const auto& answer : r.answers) {
                json::Array options;
                options.reserve(answer.selected_options.size());
                for (const auto& option : answer.selected_options) options.emplace_back(option);
                json::Object fields{{"id", answer.id}, {"selectedOptions", std::move(options)}};
                if (answer.custom_input) fields.emplace_back("customInput", *answer.custom_input);
                answers.emplace_back(std::move(fields));
            }
            obj.emplace_back("answers", std::move(answers));
        }
        return obj;
    }, response);
}

Json HostToolOutput::result_json(std::string_view id) const {
    json::Array blocks;
    blocks.reserve(content.size());
    for (const auto& item : content) {
        blocks.emplace_back(std::visit([](const auto& block) -> Json {
            using B = std::decay_t<decltype(block)>;
            if constexpr (std::is_same_v<B, TextContent>)
                return json::Object{{"type", "text"}, {"text", block.text}};
            else return to_json(block);
        }, item));
    }
    json::Object result{{"content", std::move(blocks)}};
    if (details) result.emplace_back("details", *details);
    json::Object frame{{"type", "host_tool_result"}, {"id", id}, {"result", std::move(result)}};
    if (is_error) frame.emplace_back("isError", true);
    return frame;
}

} // namespace loa::harness::omp::rpc
