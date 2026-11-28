#pragma once

// Loopback OpenAI compatible endpoint standing in for the model of the omp bridge's turns: a
// user message carrying a call marker is answered with exactly that tool call, anything else
// (the tool result, startup probes) with a short text. Nothing is ever forwarded to a real
// provider.

#include <cstdint>
#include <expected>
#include <memory>
#include <optional>
#include <string>
#include <string_view>

#include "loa/runtime/json.hpp"

namespace loa::mcp::detail {
class HttpServer;
}

namespace loa::mcp::bridge {

inline constexpr std::string_view CALL_PREFIX = "loa-omp-bridge-call ";
inline constexpr std::string_view DONE_TEXT = "Done.";

// Prompt text that makes the provider emit the call `id` of `tool` with `arguments`:
// `CALL_PREFIX` and the `CallMarker` `{id, tool, arguments}` as JSON.
[[nodiscard]] std::string call_prompt(std::string_view id, std::string_view tool, runtime::json::Object arguments);

// The server sent event stream answering one chat completions request body; none when the
// body isn't JSON.
[[nodiscard]] std::optional<std::string> completion(std::string_view body);

// The running endpoint; stops when destroyed.
class ScriptedProvider {
  public:
    // Binds `127.0.0.1` on a free port. Requests must carry `Bearer <token>`.
    [[nodiscard]] static std::expected<std::unique_ptr<ScriptedProvider>, std::string> start(std::string_view token);
    ~ScriptedProvider();
    ScriptedProvider(const ScriptedProvider&) = delete;
    ScriptedProvider& operator=(const ScriptedProvider&) = delete;

    // `http://127.0.0.1:<port>/v1`.
    [[nodiscard]] std::string base_url() const;

  private:
    explicit ScriptedProvider(std::unique_ptr<detail::HttpServer> server);
    std::unique_ptr<detail::HttpServer> server_;
};

} // namespace loa::mcp::bridge
