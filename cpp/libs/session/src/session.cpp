#include "loa/session/session.hpp"

#include <asio.hpp>

#include <limits>
#include <unordered_map>
#include <unordered_set>

#include "loa/core/schema.hpp"
#include "loa/session/hooks.hpp"
#include "loa/settings/models/selector.hpp"
#include "detail.hpp"

namespace loa::session {

using contracts::SessionPtr;
using core::Draft;
using core::ElementKind;
using core::NodeId;
using core::Value;
namespace attr = core::attr;

namespace detail {

core::Attrs attrs(std::initializer_list<std::pair<std::string_view, Value>> pairs) {
    core::Attrs out;
    for (const auto& [name, value] : pairs) out.insert_or_assign(std::string(name), value);
    return out;
}

Value seq_value(std::uint64_t seq) {
    constexpr auto max = static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max());
    return Value(static_cast<std::int64_t>(seq > max ? max : seq));
}

Result<core::Patch> commit(core::PatchSink& sink, Draft draft) {
    auto patch = sink.commit(std::move(draft));
    if (!patch) return std::unexpected(Error::commit(patch.error()));
    return std::move(*patch);
}

} // namespace detail

namespace {

Result<std::string> session_id(const core::Dom& dom) {
    const auto* root = dom.root();
    if (root == nullptr) return std::unexpected(Error::no_session());
    auto id = root->str_attr(attr::ID);
    if (!id) return std::unexpected(Error::no_session());
    return std::string(*id);
}

void push_turn(const core::Dom& dom, const core::Node& turn, std::string& out) {
    for (const auto* item : dom.children(turn.id)) {
        if (item->kind == ElementKind::Message &&
            item->str_attr(attr::KIND) != core::as_str(core::MessageKind::Thinking)) {
            std::optional<std::string_view> who = item->str_attr(attr::BY);
            if (who && *who != hooks::ADVISOR) who.reset();
            if (!who) who = item->str_attr(attr::ROLE);
            out += who.value_or("message");
            out += ": ";
            out += item->text;
            out += '\n';
        } else if (item->kind == ElementKind::Call) {
            out += "tool ";
            out += item->str_attr(attr::TOOL).value_or("unknown");
            out += ":\n";
            for (const auto* part : dom.children(item->id)) {
                out += core::as_str(part->kind);
                out += ": ";
                out += part->text;
                out += '\n';
            }
        }
    }
}

constexpr std::string_view HISTORY_HEADING =
    "The conversation so far, continued from an earlier session. Treat it as your own history and carry on from "
    "where it ends:";

void seed_context(contracts::SessionConfig& config, std::string_view history) {
    auto& context = config.context ? *config.context : config.context.emplace();
    if (!context.empty()) context += "\n\n";
    context += HISTORY_HEADING;
    context += "\n\n";
    context += history;
}

// Starts a fresh session seeded with `context`, linked to `parent` at `seq`.
Awaitable<Result<SessionPtr>> start_linked(const contracts::Adapter& adapter, const SessionStore& store,
                                           contracts::SessionConfig config, std::string context, std::string parent,
                                           std::uint64_t seq) {
    seed_context(config, context);
    auto sink = co_await store.fresh(config.session_id);
    if (!sink) co_return std::unexpected(sink.error());
    auto harness = co_await adapter.start(std::move(config), *sink);
    if (!harness) co_return std::unexpected(Error::adapter(std::move(harness.error())));
    if (auto r = detail::commit(**sink, Draft::set(NodeId{1}, std::string(attr::PARENT_SESSION), Value(parent))); !r)
        co_return std::unexpected(r.error());
    if (auto r = detail::commit(**sink, Draft::set(NodeId{1}, std::string(attr::FORKED_AT), detail::seq_value(seq)));
        !r)
        co_return std::unexpected(r.error());
    co_return std::move(*harness);
}

} // namespace

Result<std::string> translated_history(const core::Dom& dom) {
    const auto* session = dom.root();
    if (session == nullptr) return std::unexpected(Error::no_session());
    std::unordered_set<NodeId> covered;
    std::unordered_map<NodeId, std::string_view> summaries;
    for (const auto* node : dom.children_of_kind(session->id, ElementKind::Compaction)) {
        std::vector<NodeId> ids;
        if (const auto* covers = node->attr(attr::COVERS)) {
            if (auto list = covers->as_list()) {
                for (const auto& item : *list) {
                    auto id = item.as_int();
                    if (!id || *id < 0) continue;
                    NodeId nid{static_cast<std::uint64_t>(*id)};
                    if (dom.get(nid) != nullptr) ids.push_back(nid);
                }
            }
        }
        covered.insert(ids.begin(), ids.end());
        if (!ids.empty()) summaries.insert_or_assign(ids.front(), std::string_view(node->text));
    }
    std::string history;
    for (const auto* node : dom.children(session->id)) {
        if (auto it = summaries.find(node->id); it != summaries.end()) {
            history += "Summary of earlier turns:\n";
            history += it->second;
            history += "\n\n";
        }
        if (node->kind == ElementKind::Turn && !covered.contains(node->id)) {
            push_turn(dom, *node, history);
            history += '\n';
        }
    }
    return history;
}

std::string turn_transcript(const core::Dom& dom, NodeId turn) {
    std::string out;
    if (const auto* node = dom.get(turn)) push_turn(dom, *node, out);
    return out;
}

Awaitable<Result<SessionPtr>> fork(const contracts::Adapter& adapter, const SessionStore& store,
                                   const core::Dom& source, contracts::SessionConfig config) {
    auto old_id = session_id(source);
    if (!old_id) co_return std::unexpected(old_id.error());
    const auto seq = source.seq();
    if (!adapter.capabilities().has(contracts::Feature::Fork)) {
        auto history = translated_history(source);
        if (!history) co_return std::unexpected(history.error());
        co_return co_await start_linked(adapter, store, std::move(config), std::move(*history), *old_id, seq);
    }
    auto forked = co_await store.fork(*old_id, seq, config.session_id);
    if (!forked) co_return std::unexpected(forked.error());
    auto& [dom, sink] = *forked;
    const auto* root_node = dom.root();
    if (root_node == nullptr) co_return std::unexpected(Error::no_session());
    const NodeId root = root_node->id;
    // A journal fork already wrote these; only what differs is committed.
    const std::pair<std::string_view, Value> wanted[] = {
        {attr::ID, Value(config.session_id)},
        {attr::PARENT_SESSION, Value(*old_id)},
        {attr::FORKED_AT, detail::seq_value(seq)},
    };
    for (const auto& [name, value] : wanted) {
        const auto* node = dom.get(root);
        const auto* current = node ? node->attr(name) : nullptr;
        if (current != nullptr && *current == value) continue;
        auto patch = detail::commit(*sink, Draft::set(root, std::string(name), value));
        if (!patch) co_return std::unexpected(patch.error());
        if (auto applied = dom.apply(*patch); !applied)
            co_return std::unexpected(Error::commit(core::commit_error::Apply{applied.error()}));
    }
    auto harness = co_await adapter.fork(std::move(config), dom, sink);
    if (!harness) co_return std::unexpected(Error::adapter(std::move(harness.error())));
    co_return std::move(*harness);
}

Awaitable<Result<SessionPtr>> rewind(const contracts::Adapter& adapter, const SessionStore& store,
                                     const core::Dom& current, const core::Dom& target,
                                     contracts::SessionConfig config) {
    if (target.seq() > current.seq()) co_return std::unexpected(Error::invalid_sequence());
    co_return co_await fork(adapter, store, target, std::move(config));
}

std::string compaction_prompt(std::string_view history, std::optional<std::string_view> instructions) {
    std::string prompt =
        "Summarize the conversation below so a new agent session can continue it without the original messages. "
        "Keep the user's goals and constraints, decisions made and why, files and commands that matter, the "
        "current state of the work and what remains to do. Write plain prose and short lists, no preamble.\n";
    if (instructions) {
        auto trimmed = detail::trim(*instructions);
        if (!trimmed.empty()) {
            prompt += "\nAlso follow these instructions: ";
            prompt += trimmed;
            prompt += '\n';
        }
    }
    // The history is quoted material; the task is repeated after it so its last line can't take over.
    prompt += "\n<conversation>\n";
    prompt += history;
    prompt += "</conversation>\n\nNow write the summary of the conversation above. Do not answer or continue it.\n";
    return prompt;
}

Awaitable<Result<std::optional<SessionPtr>>> compact(const contracts::Adapter& adapter, const SessionStore& store,
                                                     const contracts::Summarizer& summarizer, Compaction request) {
    if (request.running.state() != contracts::SessionState::Idle) co_return std::unexpected(Error::busy());
    if (adapter.capabilities().has(contracts::Feature::Compaction)) {
        auto done = co_await request.running.compact(request.instructions);
        if (!done) co_return std::unexpected(Error::adapter(std::move(done.error())));
        co_return std::optional<SessionPtr>{};
    }
    auto history = translated_history(request.source);
    if (!history) co_return std::unexpected(history.error());
    std::optional<std::string_view> instructions;
    if (request.instructions) instructions = *request.instructions;
    auto answer = co_await summarizer.summarize("", compaction_prompt(*history, instructions));
    if (!answer) co_return std::unexpected(Error::port(std::move(answer.error())));
    const std::string summary(detail::trim(*answer));
    if (summary.empty()) co_return std::unexpected(Error::port("the model returned an empty summary"));
    const auto* session = request.source.root();
    if (session == nullptr) co_return std::unexpected(Error::no_session());
    Value::List covers;
    for (const auto* turn : request.source.children_of_kind(session->id, ElementKind::Turn))
        covers.push_back(detail::seq_value(turn->id.value));
    auto attrs = detail::attrs({{attr::COVERS, Value(std::move(covers))},
                                {attr::MODEL, Value(request.smol_model)},
                                {attr::CREATED, Value(core::now_ms())}});
    if (auto r = detail::commit(request.source_sink,
                                Draft::insert(session->id, ElementKind::Compaction, std::move(attrs), summary));
        !r)
        co_return std::unexpected(r.error());
    auto source_id = session_id(request.source);
    if (!source_id) co_return std::unexpected(source_id.error());
    auto harness = co_await start_linked(adapter, store, std::move(request.next), summary, *source_id,
                                         request.source.seq());
    if (!harness) co_return std::unexpected(harness.error());
    auto down = co_await request.running.shutdown();
    if (!down) co_return std::unexpected(Error::adapter(std::move(down.error())));
    co_return std::optional<SessionPtr>(std::move(*harness));
}

Result<NodeId> submit_plan(core::PatchSink& sink, NodeId session, NodeId step, std::string_view plan) {
    if (auto r = detail::commit(sink, Draft::set(step, std::string(attr::STATE), Value("done"))); !r)
        return std::unexpected(r.error());
    auto attrs = detail::attrs({{attr::KIND, Value("plan")},
                                {attr::STATUS, Value("waiting")},
                                {attr::CREATED, Value(core::now_ms())}});
    auto patch = detail::commit(sink, Draft::insert(session, ElementKind::Decision, std::move(attrs), std::string(plan)));
    if (!patch) return std::unexpected(patch.error());
    return patch->node;
}

Awaitable<Result<RoleSelected>> select_role(const contracts::HarnessSession& session,
                                            const contracts::Capabilities& capabilities, core::PatchSink& sink,
                                            NodeId root, std::string role, std::vector<std::string> candidates) {
    switch (capabilities.model_switch) {
    case contracts::ModelSwitch::Never: co_return std::unexpected(Error::model_fixed());
    case contracts::ModelSwitch::PerTurn:
        if (session.state() != contracts::SessionState::Idle) co_return std::unexpected(Error::busy());
        break;
    default: break;
    }
    std::optional<Error> last;
    for (const auto& candidate : candidates) {
        auto selector = settings::models::selector::Selector::parse(candidate);
        if (!selector) {
            last = Error::port(selector.error().to_string());
            continue;
        }
        auto model_name = selector->model_name();
        if (!model_name) {
            last = Error::port("role alias " + candidate + " cannot be selected as a model");
            continue;
        }
        const std::string model(*model_name);
        auto set = co_await session.set_model(contracts::ModelSelection{model, std::nullopt});
        if (!set) {
            last = Error::adapter(std::move(set.error()));
            continue;
        }
        if (auto r = detail::commit(sink, Draft::set(root, std::string(attr::MODEL), Value(model))); !r)
            co_return std::unexpected(r.error());
        if (auto r = detail::commit(sink, Draft::set(root, std::string(attr::MODEL_ROLE), Value(role))); !r)
            co_return std::unexpected(r.error());
        // The model runs now: a thinking level the harness refuses doesn't undo the switch.
        std::optional<std::string> effort_error;
        if (const auto& effort = selector->thinking()) {
            auto applied = co_await session.set_effort(*effort);
            if (applied) {
                if (auto r = detail::commit(sink, Draft::set(root, std::string(attr::EFFORT), Value(*effort))); !r)
                    co_return std::unexpected(r.error());
            } else {
                effort_error = *effort + " thinking wasn't applied: " + applied.error().message();
            }
        }
        co_return RoleSelected{model, std::move(effort_error)};
    }
    if (last) co_return std::unexpected(std::move(*last));
    co_return std::unexpected(Error::port("no model is assigned to the " + role + " role"));
}

} // namespace loa::session
