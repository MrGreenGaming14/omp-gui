#pragma once

// `TurnLedger`: the open turn of a session and what is open inside it (messages and calls
// by harness key, waiting decisions), so every mapper derives its state and closes turns
// the same way.

#include <algorithm>
#include <expected>
#include <optional>
#include <string_view>
#include <unordered_map>
#include <utility>
#include <variant>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/sink.hpp"
#include "loa/core/writer.hpp"

namespace loa::harness::kit {

// The turn being run.
template <typename K>
struct OpenTurn {
    loa::core::NodeId node;
    // Streaming messages and running calls, by the harness's id for them.
    std::unordered_map<K, loa::core::NodeId> open;
    // Decisions waiting for an answer.
    std::vector<loa::core::NodeId> waiting;
};

// What `TurnLedger::end_turn` reports to the harness policy, in order: each child it
// cancels, then the turn itself.
namespace end_event {
// An open message or call, closed as cancelled (or as the turn's error).
template <typename K>
struct Child { const K* key; loa::core::NodeId node; };
// A waiting decision, cancelled.
struct Decision { loa::core::NodeId node; };
// The turn, closed with `status`. Policies hook here to drain after an interrupt (claude),
// adopt self-started turns (codex), start the next queued follow-up (omp) or skip replayed
// history (acp).
struct Turn { loa::core::NodeId node; loa::core::TurnStatus status; };
} // namespace end_event
template <typename K>
using EndEvent = std::variant<end_event::Child<K>, end_event::Decision, end_event::Turn>;

// Per-session turn bookkeeping. `K` is the harness's key for messages and calls.
template <typename K>
class TurnLedger {
  public:
    using NodeId = loa::core::NodeId;
    using CommitResult = std::expected<void, loa::core::CommitError>;

    explicit TurnLedger(NodeId session) : session(session) {}

    // Rebuilds the ledger of a resumed session: the root, and its last turn when that is
    // still open (its children can't be keyed again, so they are cancelled when the turn ends).
    [[nodiscard]] static TurnLedger from_dom(const loa::core::Dom& dom) {
        using namespace loa::core;
        const Node* root = dom.root();
        TurnLedger ledger(root ? root->id : NodeId::DOCUMENT);
        auto turns = dom.children_of_kind(ledger.session, ElementKind::Turn);
        if (turns.empty()) return ledger;
        const Node* last = turns.back();
        auto status = last->str_attr(attr::STATUS);
        auto parsed = status ? parse<TurnStatus>(*status) : std::nullopt;
        if (!parsed || (*parsed != TurnStatus::Running && *parsed != TurnStatus::Waiting)) return ledger;
        OpenTurn<K> turn{last->id, {}, {}};
        for (const Node& n : dom.descendants(last->id)) {
            if (n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == as_str(DecisionStatus::Waiting))
                turn.waiting.push_back(n.id);
        }
        ledger.turn_ = std::move(turn);
        return ledger;
    }

    [[nodiscard]] loa::contracts::SessionState state() const {
        using loa::contracts::SessionState;
        if (closed_) return SessionState::Closed;
        if (!turn_) return SessionState::Idle;
        return turn_->waiting.empty() ? SessionState::Running : SessionState::Waiting;
    }

    [[nodiscard]] const OpenTurn<K>* turn() const { return turn_ ? &*turn_ : nullptr; }

    // Opens a turn with the user's message.
    std::expected<NodeId, loa::core::CommitError> begin(const loa::core::DomWriter& w, const loa::contracts::Prompt& p) {
        return begin_with(w, &p);
    }

    // Opens a turn, with the user's message when there is one (harness-started turns have none).
    std::expected<NodeId, loa::core::CommitError> begin_with(const loa::core::DomWriter& w,
                                                             const loa::contracts::Prompt* prompt) {
        using namespace loa::core;
        auto node = w.insert(session, ElementKind::Turn, Attrs{{std::string(attr::STATUS), TurnStatus::Running}});
        if (!node) return node;
        if (prompt) {
            Attrs by{{std::string(attr::ROLE), Role::User},
                     {std::string(attr::STATUS), MessageStatus::Done},
                     {std::string(attr::BY), prompt->by}};
            if (auto m = w.insert(*node, ElementKind::Message, std::move(by), prompt->text); !m)
                return std::unexpected(std::move(m).error());
        }
        turn_ = OpenTurn<K>{*node, {}, {}};
        return node;
    }

    // Opens `node`, a turn the mapper inserted itself.
    void adopt(NodeId node) { turn_ = OpenTurn<K>{node, {}, {}}; }

    // Tracks an open message or call under the harness's `key`.
    void open(K key, NodeId node) {
        if (turn_) turn_->open.insert_or_assign(std::move(key), node);
    }

    [[nodiscard]] std::optional<NodeId> get(const K& key) const {
        if (!turn_) return std::nullopt;
        auto it = turn_->open.find(key);
        if (it == turn_->open.end()) return std::nullopt;
        return it->second;
    }

    // Stops tracking `key`; the caller closes the node.
    std::optional<NodeId> take(const K& key) {
        if (!turn_) return std::nullopt;
        auto it = turn_->open.find(key);
        if (it == turn_->open.end()) return std::nullopt;
        NodeId node = it->second;
        turn_->open.erase(it);
        return node;
    }

    void wait(NodeId decision) {
        if (turn_) turn_->waiting.push_back(decision);
    }

    void answered(NodeId decision) {
        if (turn_) std::erase(turn_->waiting, decision);
    }

    // Ends the open turn: cancels what is still open inside it (as `error` when the turn
    // failed), closes the turn and reports each step to `hook(const EndEvent<K>&)`.
    template <typename Hook>
    CommitResult end_turn(const loa::core::DomWriter& w, loa::core::TurnStatus status, Hook&& hook) {
        using namespace loa::core;
        if (!turn_) return {};
        OpenTurn<K> turn = std::move(*turn_);
        turn_.reset();
        const bool failed = status == TurnStatus::Error;
        for (const auto& [key, node] : turn.open) {
            // Calls and messages share the `error` and `cancelled` names.
            if (auto r = w.set(node, attr::STATUS, failed ? CallStatus::Error : CallStatus::Cancelled); !r) return r;
            hook(EndEvent<K>{end_event::Child<K>{&key, node}});
        }
        for (NodeId decision : turn.waiting) {
            if (auto r = w.close(decision, DecisionStatus::Cancelled); !r) return r;
            hook(EndEvent<K>{end_event::Decision{decision}});
        }
        if (auto r = w.close(turn.node, status); !r) return r;
        hook(EndEvent<K>{end_event::Turn{turn.node, status}});
        return {};
    }

    CommitResult end_turn(const loa::core::DomWriter& w, loa::core::TurnStatus status) {
        return end_turn(w, status, [](const EndEvent<K>&) {});
    }

    // The harness is gone: nothing more will open.
    void close() { closed_ = true; }

    NodeId session;

  private:
    std::optional<OpenTurn<K>> turn_;
    bool closed_ = false;
};

// Inserts the `<session>` root of a new session from its config.
[[nodiscard]] loa::contracts::Result<loa::core::NodeId> insert_root(const loa::core::DomWriter& w, std::string_view harness,
                                                                    const loa::contracts::SessionConfig& config);

} // namespace loa::harness::kit
