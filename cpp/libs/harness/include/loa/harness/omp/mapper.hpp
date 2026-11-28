#pragma once

// Ordered omp RPC frames and sent prompts mapped to session DOM writes.
#include <deque>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_map>
#include <unordered_set>
#include <variant>
#include <vector>

#include "loa/harness/kit/mapper.hpp"
#include "loa/harness/omp/rpc/event.hpp"
#include "loa/harness/omp/rpc/inbound.hpp"

namespace loa::harness::omp {

inline constexpr std::string_view COMMANDS = "available_commands_update";
inline constexpr std::string_view RENAME = "/rename";

struct Sent { std::string id; loa::contracts::Prompt prompt; };
struct Answered { std::string request; loa::contracts::Answer answer; };
struct InputFrame { rpc::Frame frame; };
using Input = std::variant<Sent, Answered, InputFrame>;

// Fixture lines: `{"$":"send","id","text"}`, `{"$":"answer","request","values","reason"?,"by"?}`
// or an omp frame.
[[nodiscard]] Input input_from_json(rpc::Json raw);

struct Reply {
    rpc::UiResponse response;
    std::optional<std::string> steer;
    friend bool operator==(const Reply&, const Reply&) = default;
};

class Mapper final : public kit::EventMapper<Input, Reply> {
  public:
    explicit Mapper(loa::core::NodeId session);
    Mapper& titled(bool value);
    [[nodiscard]] loa::contracts::Result<std::vector<Reply>> apply(Input input, const loa::core::DomWriter& w) override;
    [[nodiscard]] loa::contracts::Result<std::optional<Reply>> answer(loa::core::NodeId decision,
        const loa::contracts::Answer& answer, const loa::core::DomWriter& w) override;
    [[nodiscard]] const kit::TurnLedger<std::string>& ledger() const override { return ledger_; }
    kit::TurnLedger<std::string>& ledger_mut() override { return ledger_; }
    [[nodiscard]] loa::contracts::SessionState state() const;
    [[nodiscard]] loa::core::NodeId session() const { return ledger_.session; }
    [[nodiscard]] bool take_rename();
    void rename_failed();
    void withdraw(std::string_view id);

  private:
    using NodeId = loa::core::NodeId;
    using Writer = loa::core::DomWriter;
    using Result = loa::contracts::Result<>;
    struct Open {
        std::optional<std::string> prompt;
        bool echo = false;
        std::unordered_map<std::string, NodeId> calls, messages, jobs, subagents, decisions, inputs;
        std::unordered_map<NodeId, rpc::UiRequest> requests;
        std::unordered_map<std::uint32_t, std::string> blocks;
        std::unordered_map<std::string, std::string> text, input_text;
        std::unordered_set<NodeId> closed;
        std::optional<std::string> typed;
    };
    enum class Rename { Idle, Due, Sent, Done };
    [[nodiscard]] std::optional<NodeId> turn() const;
    loa::contracts::Result<NodeId> open_turn(const Writer& w);
    loa::contracts::Result<NodeId> ensure_turn(const Writer& w);
    Result finish(const Writer& w, loa::core::TurnStatus status);
    Result event(const Writer& w, const rpc::Event& ev);
    Result assistant(const Writer& w, const rpc::AssistantEvent& ev, const std::optional<std::string>& id);
    Result ui(const Writer& w, const rpc::UiRequest& request);
    Result answered(const Writer& w, NodeId decision, const loa::contracts::Answer& answer);
    [[nodiscard]] loa::contracts::Result<Reply> reply(NodeId decision, const loa::contracts::Answer& answer) const;
    [[nodiscard]] std::optional<rpc::UiResponse> auto_reply(const rpc::UiRequest& request);
    Result prompt_result(const Writer& w, const rpc::event::PromptResult& result);
    Result unknown(const Writer& w, const rpc::Json& raw);
    Result ext(const Writer& w, std::string_view kind, const rpc::Json& raw);
    Result close(const Writer& w, NodeId node, loa::core::Value status);
    Result todos(const Writer& w, const std::vector<rpc::TodoItem>& items);
    kit::TurnLedger<std::string> ledger_;
    Open open_;
    std::deque<std::pair<std::string, loa::contracts::Prompt>> queued_;
    std::vector<NodeId> turns_;
    std::unordered_map<std::string, NodeId> todos_;
    std::optional<std::string> current_message_;
    std::optional<rpc::Json> commands_;
    bool titled_ = false;
    Rename rename_ = Rename::Idle;
};

} // namespace loa::harness::omp
