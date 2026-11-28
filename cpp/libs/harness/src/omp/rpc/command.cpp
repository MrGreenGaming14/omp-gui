#include "loa/harness/omp/rpc/command.hpp"

#include <algorithm>
#include <type_traits>

#include "de.hpp"

namespace loa::harness::omp::rpc {

namespace json = loa::runtime::json;

Json to_json(const ImageContent& image) {
    return json::Object{{"type", "image"}, {"data", image.data}, {"mimeType", image.mime_type}};
}

Json to_json(const HostToolDefinition& tool) {
    json::Object obj{{"name", tool.name}};
    if (tool.label) obj.emplace_back("label", *tool.label);
    obj.emplace_back("description", tool.description);
    obj.emplace_back("parameters", tool.parameters);
    if (tool.hidden) obj.emplace_back("hidden", *tool.hidden);
    if (tool.load_mode) obj.emplace_back("loadMode", *tool.load_mode);
    return obj;
}

Json to_json(const Command& command) {
    return std::visit([](const auto& c) -> Json {
        using C = std::decay_t<decltype(c)>;
        json::Object obj;
        auto type = [&](std::string_view name) { obj.emplace_back("type", name); };
        if constexpr (std::is_same_v<C, command::Prompt>) {
            type("prompt");
            obj.emplace_back("message", c.message);
            if (!c.images.empty()) {
                json::Array images;
                images.reserve(c.images.size());
                for (const auto& image : c.images) images.push_back(to_json(image));
                obj.emplace_back("images", std::move(images));
            }
            if (c.streaming_behavior) obj.emplace_back("streamingBehavior", *c.streaming_behavior == StreamingBehavior::Steer ? "steer" : "followUp");
        } else if constexpr (std::is_same_v<C, command::Steer>) { type("steer"); obj.emplace_back("message", c.message);
        } else if constexpr (std::is_same_v<C, command::Abort>) { type("abort");
        } else if constexpr (std::is_same_v<C, command::NewSession>) { type("new_session");
        } else if constexpr (std::is_same_v<C, command::AbortAndRestoreQueue>) { type("abort_and_restore_queue");
        } else if constexpr (std::is_same_v<C, command::NegotiateProtocol>) { type("negotiate_protocol"); obj.emplace_back("protocolVersion", std::uint64_t(c.protocol_version));
        } else if constexpr (std::is_same_v<C, command::GetState>) { type("get_state");
        } else if constexpr (std::is_same_v<C, command::GetEntries>) { type("get_entries");
        } else if constexpr (std::is_same_v<C, command::GetSubagents>) { type("get_subagents");
        } else if constexpr (std::is_same_v<C, command::GetSubagentMessages>) { type("get_subagent_messages"); obj.emplace_back("subagentId", c.subagent_id); obj.emplace_back("fromByte", c.from_byte);
        } else if constexpr (std::is_same_v<C, command::SteerSubagent>) { type("steer_subagent"); obj.emplace_back("subagentId", c.subagent_id); obj.emplace_back("message", c.message);
        } else if constexpr (std::is_same_v<C, command::SetAskDialog>) { type("set_ask_dialog"); obj.emplace_back("enabled", c.enabled);
        } else if constexpr (std::is_same_v<C, command::SetHostTools>) {
            type("set_host_tools");
            json::Array tools;
            tools.reserve(c.tools.size());
            for (const auto& tool : c.tools) tools.push_back(to_json(tool));
            obj.emplace_back("tools", std::move(tools));
        } else if constexpr (std::is_same_v<C, command::SetSubagentSubscription>) {
            type("set_subagent_subscription");
            obj.emplace_back("level", c.level == SubagentLevel::Off ? "off" : c.level == SubagentLevel::Progress ? "progress" : "events");
        } else if constexpr (std::is_same_v<C, command::SetEventFilter>) {
            type("set_event_filter");
            if (c.events) {
                json::Array events;
                events.reserve(c.events->size());
                for (const auto& event : *c.events) events.emplace_back(event);
                obj.emplace_back("events", std::move(events));
            } else obj.emplace_back("events", Json{});
            if (c.message_updates) obj.emplace_back("messageUpdates", *c.message_updates == MessageUpdates::Full ? "full" : "delta");
        } else if constexpr (std::is_same_v<C, command::SetModel>) { type("set_model"); obj.emplace_back("provider", c.provider); obj.emplace_back("modelId", c.model_id);
        } else if constexpr (std::is_same_v<C, command::GetAvailableModels>) { type("get_available_models");
        } else if constexpr (std::is_same_v<C, command::SetThinkingLevel>) { type("set_thinking_level"); obj.emplace_back("level", c.level);
        } else if constexpr (std::is_same_v<C, command::Compact>) { type("compact"); if (c.custom_instructions) obj.emplace_back("customInstructions", *c.custom_instructions);
        } else if constexpr (std::is_same_v<C, command::Fork>) { type("fork"); if (c.entry_id) obj.emplace_back("entryId", *c.entry_id);
        } else if constexpr (std::is_same_v<C, command::GetLoginProviders>) { type("get_login_providers");
        } else if constexpr (std::is_same_v<C, command::Login>) { type("login"); obj.emplace_back("providerId", c.provider_id);
        } else if constexpr (std::is_same_v<C, command::GetLogoutAccounts>) { type("get_logout_accounts"); obj.emplace_back("providerId", c.provider_id);
        } else if constexpr (std::is_same_v<C, command::Logout>) { type("logout"); obj.emplace_back("providerId", c.provider_id); obj.emplace_back("credentialId", c.credential_id);
        }
        return obj;
    }, command);
}

std::vector<std::uint32_t> Ready::offered_versions() const {
    if (supported_protocol_versions && !supported_protocol_versions->empty()) return *supported_protocol_versions;
    return {protocol_version.value_or(1)};
}

template <> RpcResult<Json> decode_as<Json>(const Json& data) { return data; }

template <> RpcResult<Ready> decode_as<Ready>(const Json& data) {
    Ready out;
    if (!detail::object(data) || !detail::optional_uint32(data, "protocolVersion", out.protocol_version) ||
        !detail::default_boolean(data, "supportsBashExcludeFromContext", out.supports_bash_exclude_from_context))
        return std::unexpected(detail::json_error("ready"));
    if (const auto* versions = data.find("supportedProtocolVersions"); versions && !versions->is_null()) {
        if (!versions->as_array()) return std::unexpected(detail::json_error("ready.supportedProtocolVersions"));
        std::vector<std::uint32_t> parsed;
        for (const auto& value : *versions->as_array()) {
            auto n = value.as_u64();
            if (!n || *n > std::numeric_limits<std::uint32_t>::max()) return std::unexpected(detail::json_error("ready.supportedProtocolVersions"));
            parsed.push_back(static_cast<std::uint32_t>(*n));
        }
        out.supported_protocol_versions = std::move(parsed);
    }
    return out;
}

template <> RpcResult<PromptAck> decode_as<PromptAck>(const Json& data) {
    PromptAck out;
    if (!detail::object(data) || !detail::optional_boolean(data, "agentInvoked", out.agent_invoked))
        return std::unexpected(detail::json_error("prompt response"));
    return out;
}

template <> RpcResult<ModelInfo> decode_as<ModelInfo>(const Json& data) {
    ModelInfo out;
    if (!detail::object(data) || !detail::string(data, "id", out.id) || !detail::default_string(data, "name", out.name) ||
        !detail::string(data, "provider", out.provider) || !detail::default_boolean(data, "reasoning", out.reasoning))
        return std::unexpected(detail::json_error("model"));
    out.extra = detail::extra(data, {"id", "name", "provider", "reasoning"});
    return out;
}

template <> RpcResult<ModelList> decode_as<ModelList>(const Json& data) {
    ModelList out;
    if (!detail::object(data)) return std::unexpected(detail::json_error("model list"));
    if (const auto* models = data.find("models")) {
        if (!models->as_array()) return std::unexpected(detail::json_error("model list"));
        for (const auto& model : *models->as_array()) {
            auto parsed = decode_as<ModelInfo>(model);
            if (!parsed) return std::unexpected(parsed.error());
            out.models.push_back(std::move(*parsed));
        }
    }
    return out;
}

template <> RpcResult<SubagentList> decode_as<SubagentList>(const Json& data) {
    SubagentList out;
    const auto* agents = data.find("subagents");
    if (!agents || !agents->as_array()) return std::unexpected(detail::json_error("subagent list"));
    for (const auto& agent : *agents->as_array()) {
        Subagent sub;
        if (!detail::string(agent, "id", sub.id) || !detail::string(agent, "status", sub.status))
            return std::unexpected(detail::json_error("subagent list"));
        out.subagents.push_back(std::move(sub));
    }
    return out;
}

template <> RpcResult<SubagentMessages> decode_as<SubagentMessages>(const Json& data) {
    SubagentMessages out;
    const auto* entries = data.find("entries");
    if (!detail::uint64(data, "nextByte", out.next_byte) || !detail::boolean(data, "reset", out.reset) || !entries || !entries->as_array())
        return std::unexpected(detail::json_error("subagent messages"));
    out.entries = *entries->as_array();
    return out;
}

template <> RpcResult<CompactionResult> decode_as<CompactionResult>(const Json& data) {
    CompactionResult out;
    if (!detail::object(data) || !detail::string(data, "summary", out.summary) ||
        !detail::optional_int64(data, "tokensBefore", out.tokens_before))
        return std::unexpected(detail::json_error("compaction result"));
    out.extra = detail::extra(data, {"summary", "tokensBefore"});
    return out;
}

template <> RpcResult<SessionState> decode_as<SessionState>(const Json& data) {
    SessionState out;
    if (!detail::object(data) || !detail::optional_string(data, "thinkingLevel", out.thinking_level) ||
        !detail::default_boolean(data, "isStreaming", out.is_streaming) ||
        !detail::optional_string(data, "sessionFile", out.session_file) ||
        !detail::default_string(data, "sessionId", out.session_id))
        return std::unexpected(detail::json_error("session state"));
    if (const auto* model = data.find("model"); model && !model->is_null()) {
        auto parsed = decode_as<ModelInfo>(*model);
        if (!parsed) return std::unexpected(parsed.error());
        out.model = std::move(*parsed);
    }
    out.extra = detail::extra(data, {"model", "thinkingLevel", "isStreaming", "sessionFile", "sessionId"});
    return out;
}

template <> RpcResult<CancellationResult> decode_as<CancellationResult>(const Json& data) {
    CancellationResult out;
    if (!detail::boolean(data, "cancelled", out.cancelled)) return std::unexpected(detail::json_error("cancellation result"));
    return out;
}

} // namespace loa::harness::omp::rpc
