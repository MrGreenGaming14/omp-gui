#pragma once

// Commands the host writes to omp's stdin, and the typed results some of them return. Field
// names follow `rpc.md` (camelCase on the wire). Only the commands the app sends are modelled.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/harness/kit/rpc.hpp"

namespace loa::harness::omp::rpc {

using Json = loa::runtime::json::Value;
using loa::harness::kit::RpcError;
using loa::harness::kit::RpcResult;

// An image attached to a prompt: `{"type":"image","data":…,"mimeType":…}`.
struct ImageContent {
    // Base64 encoded bytes.
    std::string data;
    std::string mime_type;
    friend bool operator==(const ImageContent&, const ImageContent&) = default;
};

// How a prompt sent during a running turn is queued.
enum class StreamingBehavior { Steer, FollowUp };

// Which subagent frames omp forwards.
enum class SubagentLevel { Off, Progress, Events };

// `set_event_filter` projection of `message_update` frames.
enum class MessageUpdates {
    // Every update carries the accumulated message.
    Full,
    // Updates carry only the delta, which keeps token streaming cheap.
    Delta,
};

enum class TodoStatus { Pending, InProgress, Completed, Abandoned };

struct TodoItem {
    std::string content;
    TodoStatus status = TodoStatus::Pending;
    friend bool operator==(const TodoItem&, const TodoItem&) = default;
};

// A tool the host serves; omp calls it back with `host_tool_call`.
struct HostToolDefinition {
    std::string name;
    std::optional<std::string> label;
    std::string description;
    // JSON Schema of the arguments.
    Json parameters;
    std::optional<bool> hidden;
    // `essential` or `discoverable`.
    std::optional<std::string> load_mode;
    friend bool operator==(const HostToolDefinition&, const HostToolDefinition&) = default;
};

// The commands of `rpc.md` the app uses, one struct each. The client adds the `id`.
namespace command {
struct Prompt {
    std::string message;
    std::vector<ImageContent> images;
    std::optional<StreamingBehavior> streaming_behavior;
    friend bool operator==(const Prompt&, const Prompt&) = default;
};
struct Steer { std::string message; friend bool operator==(const Steer&, const Steer&) = default; };
struct Abort { friend bool operator==(const Abort&, const Abort&) = default; };
struct NewSession { friend bool operator==(const NewSession&, const NewSession&) = default; };
// Withdraws queued user input and aborts, atomically.
struct AbortAndRestoreQueue { friend bool operator==(const AbortAndRestoreQueue&, const AbortAndRestoreQueue&) = default; };
struct NegotiateProtocol { std::uint32_t protocol_version = 0; friend bool operator==(const NegotiateProtocol&, const NegotiateProtocol&) = default; };
struct GetState { friend bool operator==(const GetState&, const GetState&) = default; };
struct GetEntries { friend bool operator==(const GetEntries&, const GetEntries&) = default; };
struct GetSubagents { friend bool operator==(const GetSubagents&, const GetSubagents&) = default; };
struct GetSubagentMessages {
    std::string subagent_id;
    std::uint64_t from_byte = 0;
    friend bool operator==(const GetSubagentMessages&, const GetSubagentMessages&) = default;
};
struct SteerSubagent {
    std::string subagent_id;
    std::string message;
    friend bool operator==(const SteerSubagent&, const SteerSubagent&) = default;
};
struct SetAskDialog { bool enabled = false; friend bool operator==(const SetAskDialog&, const SetAskDialog&) = default; };
struct SetHostTools { std::vector<HostToolDefinition> tools; friend bool operator==(const SetHostTools&, const SetHostTools&) = default; };
struct SetSubagentSubscription {
    SubagentLevel level = SubagentLevel::Off;
    friend bool operator==(const SetSubagentSubscription&, const SetSubagentSubscription&) = default;
};
struct SetEventFilter {
    // `nullopt` (written as `null`) forwards every event type.
    std::optional<std::vector<std::string>> events;
    std::optional<MessageUpdates> message_updates;
    friend bool operator==(const SetEventFilter&, const SetEventFilter&) = default;
};
struct SetModel {
    std::string provider;
    std::string model_id;
    friend bool operator==(const SetModel&, const SetModel&) = default;
};
struct GetAvailableModels { friend bool operator==(const GetAvailableModels&, const GetAvailableModels&) = default; };
// `inherit`, `off`, `minimal`, `low`, `medium`, `high`, `xhigh` or `max`.
struct SetThinkingLevel { std::string level; friend bool operator==(const SetThinkingLevel&, const SetThinkingLevel&) = default; };
struct Compact {
    std::optional<std::string> custom_instructions;
    friend bool operator==(const Compact&, const Compact&) = default;
};
struct Fork { std::optional<std::string> entry_id; friend bool operator==(const Fork&, const Fork&) = default; };
struct GetLoginProviders { friend bool operator==(const GetLoginProviders&, const GetLoginProviders&) = default; };
struct Login { std::string provider_id; friend bool operator==(const Login&, const Login&) = default; };
struct GetLogoutAccounts { std::string provider_id; friend bool operator==(const GetLogoutAccounts&, const GetLogoutAccounts&) = default; };
struct Logout {
    std::string provider_id;
    std::int64_t credential_id = 0;
    friend bool operator==(const Logout&, const Logout&) = default;
};
} // namespace command

using Command = std::variant<command::Prompt, command::Steer, command::Abort, command::NewSession,
                             command::AbortAndRestoreQueue, command::NegotiateProtocol, command::GetState,
                             command::GetEntries, command::GetSubagents, command::GetSubagentMessages,
                             command::SteerSubagent, command::SetAskDialog, command::SetHostTools,
                             command::SetSubagentSubscription, command::SetEventFilter, command::SetModel,
                             command::GetAvailableModels, command::SetThinkingLevel, command::Compact, command::Fork,
                             command::GetLoginProviders, command::Login, command::GetLogoutAccounts, command::Logout>;

// The command frame without its id: `{"type":"get_state",…}`, fields in declaration order.
[[nodiscard]] Json to_json(const Command& command);
[[nodiscard]] Json to_json(const HostToolDefinition& tool);
[[nodiscard]] Json to_json(const ImageContent& image);

struct Subagent {
    std::string id;
    std::string status;
    friend bool operator==(const Subagent&, const Subagent&) = default;
};

struct SubagentList {
    std::vector<Subagent> subagents;
    friend bool operator==(const SubagentList&, const SubagentList&) = default;
};

struct SubagentMessages {
    std::uint64_t next_byte = 0;
    bool reset = false;
    std::vector<Json> entries;
    friend bool operator==(const SubagentMessages&, const SubagentMessages&) = default;
};

// The `ready` frame omp writes before anything else.
struct Ready {
    // Version of the ready frame itself; absent on old servers (v1).
    std::optional<std::uint32_t> protocol_version;
    std::optional<std::vector<std::uint32_t>> supported_protocol_versions;
    bool supports_bash_exclude_from_context = false;

    // Protocol versions omp accepts. Servers that predate negotiation speak v1 only.
    [[nodiscard]] std::vector<std::uint32_t> offered_versions() const;
    friend bool operator==(const Ready&, const Ready&) = default;
};

// `data` of a `prompt` response.
struct PromptAck {
    // `false`: the prompt finished locally and no `prompt_result` follows.
    std::optional<bool> agent_invoked;
    friend bool operator==(const PromptAck&, const PromptAck&) = default;
};

struct ModelInfo {
    std::string id;
    std::string name;
    std::string provider;
    bool reasoning = false;
    // Everything else omp reports (api, cost, thinking config, …), in wire order.
    loa::runtime::json::Object extra;

    // `provider/id`, the selector form omp uses everywhere.
    [[nodiscard]] std::string selector() const { return provider + "/" + id; }
    friend bool operator==(const ModelInfo&, const ModelInfo&) = default;
};

// `data` of `get_available_models`.
struct ModelList {
    std::vector<ModelInfo> models;
    friend bool operator==(const ModelList&, const ModelList&) = default;
};

struct CompactionResult {
    std::string summary;
    std::optional<std::int64_t> tokens_before;
    loa::runtime::json::Object extra;
    friend bool operator==(const CompactionResult&, const CompactionResult&) = default;
};

// `data` of `get_state`. Fields the client doesn't name stay in `extra`.
struct SessionState {
    std::optional<ModelInfo> model;
    std::optional<std::string> thinking_level;
    bool is_streaming = false;
    std::optional<std::string> session_file;
    std::string session_id;
    loa::runtime::json::Object extra;
    friend bool operator==(const SessionState&, const SessionState&) = default;
};

struct CancellationResult {
    bool cancelled = false;
    friend bool operator==(const CancellationResult&, const CancellationResult&) = default;
};

// Decodes a response `data` (or another frame) as `T`, as serde would; failures are
// `RpcError::Json`. Defined for `Json` and every result type above.
template <typename T>
[[nodiscard]] RpcResult<T> decode_as(const Json& data);

template <> RpcResult<Json> decode_as<Json>(const Json& data);
template <> RpcResult<SubagentList> decode_as<SubagentList>(const Json& data);
template <> RpcResult<SubagentMessages> decode_as<SubagentMessages>(const Json& data);
template <> RpcResult<Ready> decode_as<Ready>(const Json& data);
template <> RpcResult<PromptAck> decode_as<PromptAck>(const Json& data);
template <> RpcResult<ModelInfo> decode_as<ModelInfo>(const Json& data);
template <> RpcResult<ModelList> decode_as<ModelList>(const Json& data);
template <> RpcResult<CompactionResult> decode_as<CompactionResult>(const Json& data);
template <> RpcResult<SessionState> decode_as<SessionState>(const Json& data);
template <> RpcResult<CancellationResult> decode_as<CancellationResult>(const Json& data);

} // namespace loa::harness::omp::rpc
