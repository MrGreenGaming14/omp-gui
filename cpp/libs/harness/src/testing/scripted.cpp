#include "loa/harness/testing/scripted.hpp"

#include <asio.hpp>

#include <algorithm>
#include <fstream>
#include <iterator>

namespace loa::harness::testing {

using namespace loa::core;
using loa::contracts::AdapterError;
using loa::contracts::Awaitable;
using loa::contracts::Result;
using loa::contracts::SessionPtr;

namespace {

struct SubagentState {
    Dom dom;
    bool addressable = false;
    std::optional<std::string> transcript_error;
    std::optional<std::string> message_error;
};

using Pairs = std::vector<std::pair<std::string_view, Value>>;

Attrs attrs(Pairs pairs) { return into_attrs(std::move(pairs)); }

AdapterError commit_error(CommitError e) { return AdapterError::Commit{std::move(e)}; }

std::string_view trim(std::string_view s) {
    while (!s.empty() && std::isspace(static_cast<unsigned char>(s.front()))) s.remove_prefix(1);
    while (!s.empty() && std::isspace(static_cast<unsigned char>(s.back()))) s.remove_suffix(1);
    return s;
}

// `split_once(' ')`.
std::pair<std::string_view, std::optional<std::string_view>> split_once(std::string_view s) {
    const auto at = s.find(' ');
    if (at == std::string_view::npos) return {s, std::nullopt};
    return {s.substr(0, at), s.substr(at + 1)};
}

} // namespace

struct ScriptedAdapter::Shared {
    mutable std::mutex mutex;
    std::map<std::string, SubagentState, std::less<>> subagents;
    Seen seen;
};

class ScriptedSession final : public loa::contracts::HarnessSession {
  public:
    ScriptedSession(ScriptedAdapter adapter, loa::contracts::SessionConfig config, std::shared_ptr<PatchSink> sink, NodeId root)
        : adapter_(std::move(adapter)), config_(std::move(config)), sink_(std::move(sink)), root_(root) {}

    Awaitable<Result<>> send(loa::contracts::Prompt prompt) const override {
        {
            std::scoped_lock lock(adapter_.shared_->mutex);
            adapter_.shared_->seen.prompts.push_back(prompt.text);
        }
        auto turn = sink_->commit(Draft::insert(root_, ElementKind::Turn, attrs({{attr::STATUS, TurnStatus::Running}, {attr::STARTED, now_ms()}})));
        if (!turn) co_return std::unexpected(commit_error(turn.error()));
        const NodeId t = turn->node;
        auto user = sink_->commit(Draft::insert(t, ElementKind::Message, attrs({{attr::ROLE, Role::User}, {attr::BY, prompt.by}}), prompt.text));
        if (!user) co_return std::unexpected(commit_error(user.error()));
        const std::string_view text = prompt.text;
        if (text.starts_with("ask")) co_return ask(t, trim(text.substr(3)));
        if (text.starts_with("work ")) co_return work(t, text.substr(5));
        if (text.starts_with("tool ")) {
            auto [tool, rest] = split_once(text.substr(5));
            const std::string args(rest.value_or("{}"));
            std::string reply;
            if (config_.host_tool_handler) {
                auto called = co_await config_.host_tool_handler->call(config_.session_id, std::string(tool), args);
                reply = called ? std::move(*called) : "failed: " + called.error();
            } else {
                reply = "failed: no host tools";
            }
            co_return end(t, reply);
        }
        co_return end(t, "ok");
    }

    Awaitable<Result<>> interrupt() const override {
        std::optional<std::pair<NodeId, NodeId>> working;
        {
            std::scoped_lock lock(adapter_.shared_->mutex);
            ++adapter_.shared_->seen.interrupts;
        }
        {
            std::scoped_lock lock(mutex_);
            working = std::exchange(working_, std::nullopt);
        }
        if (working) {
            const auto [turn, call] = *working;
            for (auto [node, status] : {std::pair{call, Value(CallStatus::Cancelled)}, std::pair{turn, Value(TurnStatus::Cancelled)}}) {
                if (auto r = sink_->commit(Draft::set(node, std::string(attr::ENDED), now_ms())); !r) co_return std::unexpected(commit_error(r.error()));
                if (auto r = sink_->commit(Draft::set(node, std::string(attr::STATUS), status)); !r) co_return std::unexpected(commit_error(r.error()));
            }
        }
        co_return Result<>{};
    }

    Awaitable<Result<>> answer(NodeId decision, loa::contracts::Answer answer) const override {
        std::optional<NodeId> turn;
        {
            std::scoped_lock lock(mutex_);
            turn = std::exchange(waiting_, std::nullopt);
        }
        if (!turn) co_return std::unexpected(AdapterError(AdapterError::UnknownDecision{decision}));
        std::vector<Draft> drafts;
        drafts.push_back(Draft::set(decision, std::string(attr::ANSWER), Value(answer.values)));
        drafts.push_back(Draft::set(decision, std::string(attr::BY), answer.by));
        if (answer.reason) drafts.push_back(Draft::set(decision, std::string(attr::REASON), *answer.reason));
        drafts.push_back(Draft::set(decision, std::string(attr::STATUS), DecisionStatus::Answered));
        drafts.push_back(Draft::set(*turn, std::string(attr::STATUS), TurnStatus::Running));
        for (auto& d : drafts)
            if (auto r = sink_->commit(std::move(d)); !r) co_return std::unexpected(commit_error(r.error()));
        co_return end(*turn, "ok");
    }

    Awaitable<Result<>> set_model(loa::contracts::ModelSelection selection) const override {
        if (std::ranges::none_of(adapter_.models_, [&](const auto& m) { return m.id == selection.model; }))
            co_return std::unexpected(AdapterError(AdapterError::Protocol{"unknown model " + selection.model}));
        if (auto r = sink_->commit(Draft::set(root_, std::string(attr::MODEL), selection.model)); !r)
            co_return std::unexpected(commit_error(r.error()));
        std::scoped_lock lock(adapter_.shared_->mutex);
        adapter_.shared_->seen.models.push_back(std::move(selection.model));
        co_return Result<>{};
    }

    Awaitable<Result<>> set_mode(SessionMode mode) const override {
        if (auto r = sink_->commit(Draft::set(root_, std::string(attr::MODE), as_str(mode))); !r)
            co_return std::unexpected(commit_error(r.error()));
        std::scoped_lock lock(adapter_.shared_->mutex);
        adapter_.shared_->seen.modes.push_back(mode);
        co_return Result<>{};
    }

    Awaitable<Result<std::vector<loa::contracts::HarnessModel>>> models() const override {
        if (adapter_.models_.empty())
            co_return std::unexpected(AdapterError(AdapterError::Unsupported{adapter_.harness(), "listing models"}));
        co_return adapter_.models_;
    }

    Awaitable<Result<>> set_effort(std::string effort) const override {
        if (!adapter_.effort_) co_return std::unexpected(AdapterError(AdapterError::Unsupported{adapter_.harness(), "set_effort"}));
        if (auto r = sink_->commit(Draft::set(root_, std::string(attr::EFFORT), effort)); !r)
            co_return std::unexpected(commit_error(r.error()));
        std::scoped_lock lock(adapter_.shared_->mutex);
        adapter_.shared_->seen.efforts.push_back(std::move(effort));
        co_return Result<>{};
    }

    Awaitable<Result<>> compact(std::optional<std::string> instructions) const override {
        std::scoped_lock lock(adapter_.shared_->mutex);
        adapter_.shared_->seen.compacts.push_back(std::move(instructions));
        co_return Result<>{};
    }

    Awaitable<Result<loa::contracts::SubagentTranscript>> subagent_transcript(std::string id) const override {
        if (!adapter_.capabilities_.has(loa::contracts::Feature::SubagentMessages))
            co_return std::unexpected(AdapterError(AdapterError::Unsupported{adapter_.harness(), "reading subagent conversations"}));
        std::scoped_lock lock(adapter_.shared_->mutex);
        adapter_.shared_->seen.subagent_transcripts.push_back(id);
        auto it = adapter_.shared_->subagents.find(id);
        if (it == adapter_.shared_->subagents.end()) co_return loa::contracts::SubagentTranscript{Dom{}, false};
        if (it->second.transcript_error) co_return std::unexpected(AdapterError(AdapterError::Protocol{*it->second.transcript_error}));
        co_return loa::contracts::SubagentTranscript{it->second.dom, it->second.addressable};
    }

    Awaitable<Result<>> message_subagent(std::string id, std::string text) const override {
        if (!adapter_.capabilities_.has(loa::contracts::Feature::SubagentMessages))
            co_return std::unexpected(AdapterError(AdapterError::Unsupported{adapter_.harness(), "messaging subagents"}));
        co_return deliver(std::move(id), std::move(text));
    }

    std::string_view harness() const override { return adapter_.harness(); }
    loa::contracts::SessionState state() const override { return loa::contracts::SessionState::Idle; }
    Awaitable<Result<>> wait_idle() const override { co_return Result<>{}; }
    Awaitable<Result<>> shutdown() const override { co_return Result<>{}; }

  private:
    Result<> end(NodeId turn, std::string_view reply) const {
        if (auto r = sink_->commit(Draft::insert(turn, ElementKind::Message, attrs({{attr::ROLE, Role::Assistant}}), std::string(reply))); !r)
            return std::unexpected(commit_error(r.error()));
        if (auto r = sink_->commit(Draft::set(turn, std::string(attr::ENDED), now_ms())); !r) return std::unexpected(commit_error(r.error()));
        if (auto r = sink_->commit(Draft::set(turn, std::string(attr::STATUS), TurnStatus::Done)); !r)
            return std::unexpected(commit_error(r.error()));
        return {};
    }

    Result<> ask(NodeId turn, std::string_view kind) const {
        auto options = [](std::initializer_list<std::string_view> labels) {
            Value::List list;
            for (auto l : labels) list.emplace_back(l);
            return std::pair<std::string_view, Value>{attr::OPTIONS, Value(std::move(list))};
        };
        DecisionKind decision = DecisionKind::Approval;
        std::string_view title = "Run ls", body = "ls";
        Pairs pairs;
        if (kind == "question") {
            decision = DecisionKind::Question, title = "Color", body = "Which color?";
            pairs.push_back(options({"red", "green"}));
        } else if (kind == "multi") {
            decision = DecisionKind::Question, title = "Languages", body = "Which languages?";
            pairs.push_back(options({"rust", "go", "zig"}));
            pairs.emplace_back(attr::MULTI, true);
        } else if (kind == "plan") {
            decision = DecisionKind::Plan, title = "Plan", body = "# Plan\n\n1. Read the code\n2. Fix the bug";
        } else if (kind == "form") {
            decision = DecisionKind::Form, title = "Release", body = "Release settings";
            pairs.emplace_back(attr::FIELDS, FORM);
        } else {
            pairs.push_back(options({answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY}));
        }
        pairs.emplace_back(attr::KIND, decision);
        pairs.emplace_back(attr::STATUS, DecisionStatus::Waiting);
        pairs.emplace_back(attr::TITLE, title);
        if (auto r = sink_->commit(Draft::insert(turn, ElementKind::Decision, attrs(std::move(pairs)), std::string(body))); !r)
            return std::unexpected(commit_error(r.error()));
        if (auto r = sink_->commit(Draft::set(turn, std::string(attr::STATUS), TurnStatus::Waiting)); !r)
            return std::unexpected(commit_error(r.error()));
        std::scoped_lock lock(mutex_);
        waiting_ = turn;
        return {};
    }

    Result<> work(NodeId turn, std::string_view spec) const {
        auto [tool, title] = split_once(spec);
        Pairs pairs{{attr::TOOL, tool}, {attr::CALL_ID, "work"}, {attr::STATUS, CallStatus::Running}, {attr::STARTED, now_ms()}};
        if (title && !title->empty()) pairs.emplace_back(attr::TITLE, *title);
        auto call = sink_->commit(Draft::insert(turn, ElementKind::Call, attrs(std::move(pairs))));
        if (!call) return std::unexpected(commit_error(call.error()));
        std::scoped_lock lock(mutex_);
        working_ = std::pair{turn, call->node};
        return {};
    }

    Result<> deliver(std::string id, std::string text) const {
        {
            std::scoped_lock lock(adapter_.shared_->mutex);
            auto it = adapter_.shared_->subagents.find(id);
            if (it == adapter_.shared_->subagents.end())
                return std::unexpected(AdapterError(AdapterError::NotAddressable{"unknown subagent " + id}));
            SubagentState& entry = it->second;
            if (entry.message_error) return std::unexpected(AdapterError(AdapterError::Protocol{*entry.message_error}));
            if (!entry.addressable)
                return std::unexpected(AdapterError(AdapterError::NotAddressable{"subagent " + id + " is no longer addressable"}));
            const std::int64_t ts = now_ms();
            auto fail = [](const ApplyError& e) { return AdapterError(AdapterError::Commit{commit_error::Apply{e}}); };
            NodeId root;
            if (const Node* r = entry.dom.root()) {
                root = r->id;
            } else {
                auto inserted = entry.dom.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session,
                                                               attrs({{attr::ID, id}, {attr::HARNESS, adapter_.harness()}, {attr::NODE, config_.node}, {attr::CREATED, ts}})),
                                                 ts);
                if (!inserted) return std::unexpected(fail(inserted.error()));
                root = inserted->node;
            }
            auto turn = entry.dom.commit(Draft::insert(root, ElementKind::Turn, attrs({{attr::STATUS, TurnStatus::Done}})), ts);
            if (!turn) return std::unexpected(fail(turn.error()));
            auto msg = entry.dom.commit(Draft::insert(turn->node, ElementKind::Message, attrs({{attr::ROLE, Role::User}, {attr::BY, config_.node}}), text), ts);
            if (!msg) return std::unexpected(fail(msg.error()));
            adapter_.shared_->seen.subagent_messages.emplace_back(std::move(id), std::move(text));
        }
        return {};
    }

    ScriptedAdapter adapter_;
    loa::contracts::SessionConfig config_;
    std::shared_ptr<PatchSink> sink_;
    NodeId root_;
    mutable std::mutex mutex_;
    // The turn waiting on a decision.
    mutable std::optional<NodeId> waiting_;
    // The turn and its call left running by `work`.
    mutable std::optional<std::pair<NodeId, NodeId>> working_;
};

ScriptedAdapter::ScriptedAdapter(std::string id)
    : id_(std::move(id)), capabilities_(loa::contracts::Capabilities::builder().build()), shared_(std::make_shared<Shared>()) {}

ScriptedAdapter ScriptedAdapter::with_capabilities(loa::contracts::Capabilities capabilities) && {
    capabilities_ = std::move(capabilities);
    return std::move(*this);
}

ScriptedAdapter ScriptedAdapter::subagent_messages() && {
    if (!capabilities_.has(loa::contracts::Feature::SubagentMessages)) capabilities_.features.push_back(loa::contracts::Feature::SubagentMessages);
    return std::move(*this);
}

ScriptedAdapter ScriptedAdapter::subagent_transcript(std::string id, Dom dom, bool addressable) && {
    set_subagent_transcript(std::move(id), std::move(dom), addressable);
    return std::move(*this);
}

ScriptedAdapter ScriptedAdapter::with_availability(loa::contracts::Availability availability) && {
    availability_ = std::move(availability);
    return std::move(*this);
}

ScriptedAdapter ScriptedAdapter::no_start() && {
    starts_ = false;
    return std::move(*this);
}

ScriptedAdapter ScriptedAdapter::no_resume() && {
    resumes_ = false;
    return std::move(*this);
}

ScriptedAdapter ScriptedAdapter::with_models(std::span<const ScriptedModel> models) && {
    models_.clear();
    for (const auto& m : models) {
        const auto slash = m.id.find('/');
        models_.push_back({std::string(m.id), std::string(m.id), std::string(m.id.substr(0, slash)),
                           std::vector<std::string>(m.efforts.begin(), m.efforts.end())});
    }
    return std::move(*this);
}

ScriptedAdapter ScriptedAdapter::with_effort() && {
    effort_ = true;
    return std::move(*this);
}

void ScriptedAdapter::set_subagent_transcript(std::string id, Dom dom, bool addressable) const {
    std::scoped_lock lock(shared_->mutex);
    auto [it, inserted] = shared_->subagents.try_emplace(std::move(id));
    if (inserted) it->second.addressable = addressable;
    it->second.dom = std::move(dom);
    it->second.addressable = addressable;
}

void ScriptedAdapter::set_subagent_error(std::string id, std::optional<std::string> transcript_error,
                                         std::optional<std::string> message_error) const {
    std::scoped_lock lock(shared_->mutex);
    auto [it, inserted] = shared_->subagents.try_emplace(std::move(id));
    if (inserted) it->second.addressable = true;
    it->second.transcript_error = std::move(transcript_error);
    it->second.message_error = std::move(message_error);
}

std::optional<Dom> ScriptedAdapter::subagent_dom(std::string_view id) const {
    std::scoped_lock lock(shared_->mutex);
    auto it = shared_->subagents.find(id);
    if (it == shared_->subagents.end()) return std::nullopt;
    return it->second.dom;
}

Seen ScriptedAdapter::seen() const {
    std::scoped_lock lock(shared_->mutex);
    return shared_->seen;
}

SessionPtr ScriptedAdapter::session(loa::contracts::SessionConfig config, std::shared_ptr<PatchSink> sink, NodeId root) const {
    {
        // Without the handler: what it routes to may hold the session open.
        loa::contracts::SessionConfig recorded = config;
        recorded.host_tool_handler = nullptr;
        std::scoped_lock lock(shared_->mutex);
        shared_->seen.configs.push_back(std::move(recorded));
    }
    return std::make_unique<ScriptedSession>(*this, std::move(config), std::move(sink), root);
}

Awaitable<loa::contracts::Availability> ScriptedAdapter::availability() const {
    co_return availability_.value_or(loa::contracts::Availability::ready(std::nullopt, "1.0"));
}

Awaitable<Result<SessionPtr>> ScriptedAdapter::start(loa::contracts::SessionConfig config, std::shared_ptr<PatchSink> sink) const {
    if (!starts_) co_return std::unexpected(AdapterError(AdapterError::NotInstalled{id_}));
    Pairs pairs{{attr::ID, config.session_id},
                {attr::NODE, config.node},
                {attr::HARNESS, id_},
                {attr::CREATED, now_ms()},
                {attr::CWD, config.cwd.string()},
                {attr::NATIVE_ID, "native-" + config.session_id}};
    // As the real adapters do through `kit::ledger::insert_root`.
    if (config.project) pairs.emplace_back(attr::PROJECT, *config.project);
    auto root = sink->commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs(std::move(pairs))));
    if (!root) co_return std::unexpected(commit_error(root.error()));
    co_return session(std::move(config), std::move(sink), root->node);
}

Awaitable<Result<SessionPtr>> ScriptedAdapter::resume(loa::contracts::SessionConfig config, const Dom& dom, std::shared_ptr<PatchSink> sink) const {
    if (!resumes_) co_return std::unexpected(AdapterError(AdapterError::Unsupported{id_, "resume"}));
    {
        std::scoped_lock lock(shared_->mutex);
        ++shared_->seen.resumes;
    }
    const Node* root = dom.root();
    if (root == nullptr) co_return std::unexpected(AdapterError(AdapterError::Format{"empty DOM"}));
    co_return session(std::move(config), std::move(sink), root->id);
}

Result<> ScriptedAdapter::import(const std::filesystem::path& native, PatchSink& sink) const {
    std::ifstream in(native, std::ios::binary);
    if (!in) return std::unexpected(AdapterError(AdapterError::Format{"No such file or directory (os error 2)"}));
    const std::string text{std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
    auto root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session,
                                          attrs({{attr::ID, "scripted-import"}, {attr::NODE, "testhost"}, {attr::HARNESS, id_}, {attr::CREATED, now_ms()}, {attr::TITLE, "Imported"}})));
    if (!root) return std::unexpected(commit_error(root.error()));
    std::string_view rest = text;
    while (!rest.empty()) {
        const auto nl = rest.find('\n');
        std::string_view line = rest.substr(0, nl);
        if (line.ends_with('\r')) line.remove_suffix(1);
        rest = nl == std::string_view::npos ? std::string_view{} : rest.substr(nl + 1);
        auto turn = sink.commit(Draft::insert(root->node, ElementKind::Turn, attrs({{attr::STATUS, TurnStatus::Done}})));
        if (!turn) return std::unexpected(commit_error(turn.error()));
        if (auto m = sink.commit(Draft::insert(turn->node, ElementKind::Message, attrs({{attr::ROLE, Role::User}}), std::string(line))); !m)
            return std::unexpected(commit_error(m.error()));
    }
    return {};
}

Result<> ScriptedAdapter::export_(const Dom& dom, const std::filesystem::path& out) const {
    std::string text;
    bool first = true;
    for (const Node& node : dom.iter()) {
        if (node.kind != ElementKind::Message) continue;
        if (!first) text += '\n';
        text += node.text;
        first = false;
    }
    std::ofstream file(out, std::ios::binary | std::ios::trunc);
    if (!file || !(file << text) || !file.flush()) return std::unexpected(AdapterError(AdapterError::Format{"cannot write " + out.string()}));
    return {};
}

} // namespace loa::harness::testing
