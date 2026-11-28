#pragma once

#include <array>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_map>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/core/writer.hpp"
#include "loa/harness/kit/ledger.hpp"
#include "loa/runtime/json.hpp"

namespace loa::harness::codex {

using Json = loa::runtime::json::Value;

// A server request response, or a host tool invocation delegated to the session.
struct Reply {
    enum class Kind { Result, Error, HostTool } kind = Kind::Result;
    Json id;
    Json result;
    std::int64_t code = 0;
    std::string message;
    std::string tool;
    std::string arguments;
};

class Mapper {
  public:
    static loa::contracts::Result<Mapper> start(const loa::contracts::SessionConfig& config, std::string_view thread_id,
                                                std::optional<std::string_view> model, const loa::core::DomWriter& w);
    static loa::contracts::Result<Mapper> attach(const loa::core::Dom& dom, const loa::core::DomWriter& w);

    [[nodiscard]] loa::contracts::SessionState state() const { return ledger_.state(); }
    [[nodiscard]] std::optional<std::string_view> turn_id() const;
    [[nodiscard]] std::optional<std::string_view> model() const;

    loa::contracts::Result<> begin_turn(const loa::contracts::Prompt& prompt, const loa::core::DomWriter& w);
    loa::contracts::Result<> turn_accepted(std::string_view id, const loa::core::DomWriter& w);
    loa::contracts::Result<> turn_rejected(std::string_view message, const loa::core::DomWriter& w);
    loa::contracts::Result<> process_exited(std::string_view detail, const loa::core::DomWriter& w);
    loa::contracts::Result<> set_model(std::string_view model, const loa::core::DomWriter& w);
    loa::contracts::Result<> set_effort(std::string_view effort, const loa::core::DomWriter& w);
    loa::contracts::Result<> set_mode(loa::core::SessionMode mode, const loa::core::DomWriter& w);
    loa::contracts::Result<std::optional<Reply>> handle(std::optional<Json> id, std::string_view method,
                                                         const Json& params, const loa::core::DomWriter& w);
    loa::contracts::Result<std::optional<Reply>> answer(loa::core::NodeId decision, const loa::contracts::Answer& answer,
                                                         const loa::core::DomWriter& w);

  private:
    enum class Kind { Message, Thinking, Call, User };
    struct Item {
        loa::core::NodeId node;
        Kind kind;
        std::size_t streamed = 0;
        std::optional<loa::core::NodeId> result;
        std::size_t result_streamed = 0;
        std::string diff;
    };
    enum class PendingKind { Command, FileChange, Question, Form, Url, Permissions };
    struct Pending {
        Json rpc_id;
        PendingKind kind;
        bool session_option = false;
        std::string group;
        std::string question;
        Json permissions;
    };
    struct InputGroup {
        Json rpc_id;
        std::size_t waiting;
        loa::runtime::json::Object answers;
    };

    explicit Mapper(loa::core::NodeId session, std::optional<std::string> model);
    loa::contracts::Result<loa::core::NodeId> ensure_turn(std::optional<std::string_view> id, const loa::core::DomWriter& w);
    loa::contracts::Result<> opened(loa::core::NodeId node, std::optional<std::string_view> id, bool prompted,
                                    const loa::core::DomWriter& w);
    loa::contracts::Result<> end_turn(loa::core::TurnStatus status, const loa::core::DomWriter& w);
    loa::contracts::Result<loa::core::NodeId> message_item(std::optional<std::string_view> turn, std::string_view id,
                                                           Kind kind, std::string_view type, const loa::core::DomWriter& w);
    void track(std::string_view id, loa::core::NodeId node, Kind kind);
    loa::contracts::Result<> item(const Json& params, bool started, const loa::core::DomWriter& w);
    loa::contracts::Result<bool> start_call(std::optional<std::string_view> turn, const Json& item,
                                             const loa::core::DomWriter& w);
    loa::contracts::Result<> finish_call(std::string_view id, const Json& item, const loa::core::DomWriter& w);
    loa::contracts::Result<> agent_jobs(loa::core::NodeId call, const Json& item, const loa::core::DomWriter& w);
    loa::contracts::Result<> text_delta(const Json& params, Kind kind, std::string_view type, const loa::core::DomWriter& w);
    loa::contracts::Result<> output_delta(const Json& params, const loa::core::DomWriter& w);
    loa::contracts::Result<> plan_updated(const Json& params, const loa::core::DomWriter& w);
    loa::contracts::Result<> turn_completed(const Json& turn, const loa::core::DomWriter& w);
    loa::contracts::Result<> request_cleared(const Json& id, const loa::core::DomWriter& w);
    loa::contracts::Result<> unwait(loa::core::NodeId decision, const loa::core::DomWriter& w);
    void forget(loa::core::NodeId decision);
    loa::contracts::Result<std::pair<loa::core::NodeId, std::optional<loa::core::NodeId>>>
    decision_target(const Json& params, const loa::core::DomWriter& w);
    loa::contracts::Result<> decide(const Json& id, const Json& params, loa::core::DecisionKind kind,
                                    loa::core::Attrs attrs, std::string text, Pending pending,
                                    const loa::core::DomWriter& w);
    loa::contracts::Result<std::optional<Reply>> approval(Json id, const Json& params, std::string title,
                                                           std::string body, Pending pending, const loa::core::DomWriter& w);

    loa::harness::kit::TurnLedger<std::string> ledger_;
    std::optional<std::string> model_;
    std::vector<loa::core::NodeId> turns_;
    std::size_t covered_ = 0;
    std::optional<std::string> turn_id_;
    bool prompted_ = false;
    std::unordered_map<std::string, Item> items_;
    std::optional<std::array<std::int64_t, 4>> usage_;
    std::vector<loa::core::NodeId> todos_;
    std::unordered_map<std::string, loa::core::NodeId> jobs_;
    std::unordered_map<loa::core::NodeId, Pending> decisions_;
    std::unordered_map<std::string, InputGroup> groups_;
};

} // namespace loa::harness::codex
