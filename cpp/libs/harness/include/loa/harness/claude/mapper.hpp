#pragma once

#include <expected>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_map>
#include <variant>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/harness/kit/ledger.hpp"
#include "loa/harness/kit/rpc.hpp"

namespace loa::harness::claude {

inline constexpr std::string_view UUID_ATTR = "x-claude-uuid";
using Json = kit::Json;

struct Write { Json value; friend bool operator==(const Write&, const Write&) = default; };
struct Mcp {
    std::string request_id;
    std::string server;
    Json message;
    friend bool operator==(const Mcp&, const Mcp&) = default;
};
using Output = std::variant<Write, Mcp>;

[[nodiscard]] Json control_response(std::string_view request_id, std::expected<Json, std::string> outcome);

class Mapper {
  public:
    [[nodiscard]] static loa::contracts::Result<Mapper> create(std::shared_ptr<loa::core::PatchSink> sink,
                                                                const loa::contracts::SessionConfig& config);
    [[nodiscard]] static loa::contracts::Result<Mapper> attach(std::shared_ptr<loa::core::PatchSink> sink,
                                                                const loa::core::Dom& dom);
    [[nodiscard]] loa::contracts::SessionState state() const { return ledger_.state(); }
    [[nodiscard]] const std::optional<std::string>& native_id() const { return native_id_; }
    [[nodiscard]] bool turn_open() const { return ledger_.turn() != nullptr; }
    [[nodiscard]] loa::contracts::Result<loa::core::NodeId> open_turn(std::string_view text, std::string_view by,
                                                                      std::string_view uuid);
    [[nodiscard]] loa::contracts::Result<std::optional<Output>> handle(Json raw);
    [[nodiscard]] loa::contracts::Result<> interrupt();
    [[nodiscard]] loa::contracts::Result<> exited();
    [[nodiscard]] loa::contracts::Result<std::optional<Json>> answer(loa::core::NodeId decision,
                                                                     const loa::contracts::Answer& answer);
    [[nodiscard]] loa::contracts::Result<> record_model(std::string_view model);
    [[nodiscard]] loa::contracts::Result<> record_mode(loa::core::SessionMode mode);

  private:
    using NodeId = loa::core::NodeId;
    using Key = std::string;
    struct StreamBlock { std::string kind; NodeId node; NodeId target; bool streamed = false; };
    struct Streaming {
        std::string message_id;
        std::optional<std::string> model;
        std::vector<std::optional<StreamBlock>> blocks;
        std::size_t finalized = 0;
    };
    struct Call { NodeId node; std::optional<NodeId> job; };
    struct Pending {
        std::string request_id;
        loa::core::DecisionKind kind;
        std::string tool;
        Json input;
        std::optional<Json> suggestions;
        std::vector<std::pair<NodeId, std::optional<loa::contracts::Answer>>> decisions;
    };

    Mapper(std::shared_ptr<loa::core::PatchSink> sink, NodeId root, std::optional<std::string> native_id,
           std::optional<std::string> model, std::optional<loa::core::SessionMode> mode);
    [[nodiscard]] loa::core::DomWriter w() const;
    [[nodiscard]] std::optional<NodeId> turn() const;
    [[nodiscard]] loa::contracts::Result<NodeId> begin_turn(std::string_view id);
    [[nodiscard]] loa::contracts::Result<std::optional<NodeId>> current_turn();
    [[nodiscard]] loa::contracts::Result<> end_turn(loa::core::TurnStatus status);
    [[nodiscard]] loa::contracts::Result<> on_system(const Json& raw);
    [[nodiscard]] loa::contracts::Result<> on_task_started(std::string_view task, const Json& raw);
    [[nodiscard]] loa::contracts::Result<> end_task(const std::string* task, loa::core::JobStatus status,
                                                    const std::string* summary);
    [[nodiscard]] loa::contracts::Result<> on_stream(const Json& event);
    [[nodiscard]] loa::contracts::Result<StreamBlock> open_message(NodeId turn, std::string_view kind,
                                                                    const std::optional<std::string>& model,
                                                                    std::string_view text);
    [[nodiscard]] loa::contracts::Result<> finish_message(NodeId node);
    [[nodiscard]] loa::contracts::Result<StreamBlock> open_call(NodeId turn, std::string_view id,
                                                                 std::string_view tool, std::string_view input);
    [[nodiscard]] loa::contracts::Result<> complete_call(NodeId call, const Json& block, const std::string* uuid);
    [[nodiscard]] loa::contracts::Result<> on_assistant(const Json& raw);
    [[nodiscard]] std::optional<StreamBlock> next_streamed(std::string_view id, std::string_view kind);
    [[nodiscard]] loa::contracts::Result<> on_user(const Json& raw);
    [[nodiscard]] loa::contracts::Result<> on_tool_result(const Json& block, const std::string* uuid);
    [[nodiscard]] loa::contracts::Result<> on_result(const Json& raw);
    [[nodiscard]] loa::contracts::Result<std::optional<Output>> on_control(const Json& raw);
    [[nodiscard]] loa::contracts::Result<std::optional<Output>> on_can_use_tool(std::string_view request_id,
                                                                                const Json& request);
    [[nodiscard]] loa::contracts::Result<> on_cancel(std::string_view request_id);
    [[nodiscard]] loa::contracts::Result<> resume_turn();
    [[nodiscard]] loa::contracts::Result<> ext(std::string_view kind, const Json& raw);
    [[nodiscard]] static Json permission_response(const Pending& pending);

    std::shared_ptr<loa::core::PatchSink> sink_;
    kit::TurnLedger<Key> ledger_;
    std::optional<std::string> native_id_;
    std::optional<std::string> model_;
    std::optional<loa::core::SessionMode> mode_;
    std::optional<Streaming> stream_;
    std::unordered_map<std::string, Call> calls_;
    std::unordered_map<std::string, NodeId> jobs_;
    std::vector<Pending> pending_;
    std::vector<NodeId> uncompacted_;
    std::optional<NodeId> open_compaction_;
    unsigned draining_ = 0;
};

} // namespace loa::harness::claude
