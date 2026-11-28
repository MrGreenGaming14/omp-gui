#pragma once

// The subset of the Agent Client Protocol schema (protocol version 1) the client side reads
// (port of `acp/wire.rs`). Fields the adapter doesn't look at stay in the raw JSON that lands
// in `<ext>` elements. Decoders return the serde-style failure text.

#include <cstdint>
#include <expected>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/runtime/json.hpp"

namespace loa::harness::acp::wire {

using Json = loa::runtime::json::Value;
template <typename T>
using Decoded = std::expected<T, std::string>;

// The only ACP major version this adapter speaks.
inline constexpr std::uint16_t PROTOCOL_VERSION = 1;

struct McpCapabilities {
    bool http = false;
};

struct AgentCapabilities {
    bool load_session = false;
    McpCapabilities mcp_capabilities;
};

struct Implementation {
    std::string name;
    std::optional<std::string> version;
};

struct InitializeResponse {
    std::uint16_t protocol_version = 0;
    AgentCapabilities agent_capabilities;
    std::optional<Implementation> agent_info;
};

struct ModelInfo {
    std::string model_id;
    std::string name;
};

struct SessionModelState {
    std::string current_model_id;
    std::vector<ModelInfo> available_models;
};

struct SessionModeInfo {
    std::string id;
};

struct SessionModeState {
    std::string current_mode_id;
    std::vector<SessionModeInfo> available_modes;
};

struct NewSessionResponse {
    std::string session_id;
    std::optional<SessionModeState> modes;
    // Unstable `session/set_model` support: the models the agent offers.
    std::optional<SessionModelState> models;
};

struct LoadSessionResponse {
    std::optional<SessionModeState> modes;
    std::optional<SessionModelState> models;
};

enum class StopReason { EndTurn, MaxTokens, MaxTurnRequests, Refusal, Cancelled };
[[nodiscard]] std::string_view as_str(StopReason reason) noexcept;

struct PromptResponse {
    StopReason stop_reason = StopReason::EndTurn;
};

enum class ToolCallStatus { Pending, InProgress, Completed, Failed };

// Fields of both `tool_call` and `tool_call_update`. A `tool_call` carries at least
// `title`; an update carries only what changed.
struct ToolCallFields {
    std::string tool_call_id;
    std::optional<std::string> title;
    std::optional<std::string> name;
    std::optional<std::string> kind;
    std::optional<ToolCallStatus> status;
    // Replaces the whole content collection when present.
    std::optional<std::vector<Json>> content;
    std::optional<Json> raw_input;
    std::optional<Json> raw_output;
};

enum class PlanEntryStatus { Pending, InProgress, Completed };

struct PlanEntry {
    std::string content;
    std::optional<std::string> priority;
    PlanEntryStatus status = PlanEntryStatus::Pending;
};

struct Plan {
    std::vector<PlanEntry> entries;
};

struct CurrentModeUpdate {
    std::string current_mode_id;
};

struct SessionInfoUpdate {
    std::optional<std::string> title;
};

enum class PermissionOptionKind { AllowOnce, AllowAlways, RejectOnce, RejectAlways };
[[nodiscard]] std::string_view as_str(PermissionOptionKind kind) noexcept;

struct PermissionOption {
    std::string option_id;
    std::string name;
    PermissionOptionKind kind = PermissionOptionKind::AllowOnce;
    friend bool operator==(const PermissionOption&, const PermissionOption&) = default;
};

struct RequestPermissionRequest {
    ToolCallFields tool_call;
    std::vector<PermissionOption> options;
};

struct ReadTextFileRequest {
    std::string path;
    // 1-based first line.
    std::optional<std::uint32_t> line;
    // Number of lines.
    std::optional<std::uint32_t> limit;
};

struct WriteTextFileRequest {
    std::string path;
    std::string content;
};

struct EnvVariable {
    std::string name;
    std::string value;
};

struct CreateTerminalRequest {
    std::string command;
    std::optional<std::vector<std::string>> args;
    std::optional<std::vector<EnvVariable>> env;
    std::optional<std::string> cwd;
    std::optional<std::uint64_t> output_byte_limit;
};

struct TerminalRef {
    std::string terminal_id;
};

[[nodiscard]] Decoded<InitializeResponse> decode_initialize(const Json& v);
[[nodiscard]] Decoded<NewSessionResponse> decode_new_session(const Json& v);
[[nodiscard]] Decoded<LoadSessionResponse> decode_load_session(const Json& v);
[[nodiscard]] Decoded<PromptResponse> decode_prompt_response(const Json& v);
[[nodiscard]] Decoded<ToolCallFields> decode_tool_call(const Json& v);
[[nodiscard]] Decoded<Plan> decode_plan(const Json& v);
[[nodiscard]] Decoded<CurrentModeUpdate> decode_current_mode(const Json& v);
[[nodiscard]] Decoded<SessionInfoUpdate> decode_session_info(const Json& v);
[[nodiscard]] Decoded<RequestPermissionRequest> decode_permission(const Json& v);
[[nodiscard]] Decoded<ReadTextFileRequest> decode_read_text_file(const Json& v);
[[nodiscard]] Decoded<WriteTextFileRequest> decode_write_text_file(const Json& v);
[[nodiscard]] Decoded<CreateTerminalRequest> decode_create_terminal(const Json& v);
[[nodiscard]] Decoded<TerminalRef> decode_terminal_ref(const Json& v);

} // namespace loa::harness::acp::wire
