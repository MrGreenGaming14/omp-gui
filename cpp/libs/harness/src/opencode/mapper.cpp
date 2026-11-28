#include "loa/harness/opencode/mapper.hpp"

#include <algorithm>
#include <limits>
#include <utility>

#include "loa/core/schema.hpp"
#include "loa/core/writer.hpp"
#include "loa/harness/opencode/json.hpp"

namespace loa::harness::opencode {
namespace {
namespace attr = loa::core::attr;
using loa::contracts::AdapterError;
using loa::contracts::Answer;
using loa::core::CallStatus;
using loa::core::CommitError;
using loa::core::DecisionKind;
using loa::core::DecisionStatus;
using loa::core::Dom;
using loa::core::DomWriter;
using loa::core::JobKind;
using loa::core::JobStatus;
using loa::core::MessageKind;
using loa::core::MessageStatus;
using loa::core::Node;
using loa::core::NodeId;
using loa::core::Role;
using loa::core::TodoState;
using loa::core::TurnStatus;
using K = loa::core::ElementKind;
using CValue = loa::core::Value;
using loa::core::Attrs;
using json::Value;
using json_util::int_at;
using json_util::is_true;

AdapterError to_adapter(CommitError e) { return AdapterError::Commit{std::move(e)}; }

#define OC_TRY(expr)                                                                                 \
    do {                                                                                             \
        auto oc_result = (expr);                                                                     \
        if (!oc_result) return std::unexpected(to_adapter(std::move(oc_result).error()));            \
    } while (false)
#define OC_TRY_A(expr)                                                                               \
    do {                                                                                             \
        auto oc_result = (expr);                                                                     \
        if (!oc_result) return std::unexpected(std::move(oc_result).error());                        \
    } while (false)

constexpr std::string_view TEXT_FIELDS[] = {"/id", "/sessionID", "/messageID", "/type", "/text"};
constexpr std::string_view EXT_FIELDS[] = {"/id", "/sessionID", "/messageID"};
constexpr std::string_view TOOL_FIELDS[] = {"/id",   "/sessionID", "/messageID",   "/type",        "/callID",
                                            "/tool", "/state/raw", "/state/title", "/state/output", "/state/error"};
constexpr std::string_view USER_FIELDS[] = {"/id", "/sessionID", "/role", "/time/created"};
constexpr std::string_view ASSISTANT_FIELDS[] = {"/id", "/sessionID"};

std::optional<std::string_view> str_at(const Value& v, std::string_view ptr) {
    const auto* s = json_util::str_at(v, ptr);
    return s ? std::optional<std::string_view>(*s) : std::nullopt;
}

std::string str_or(const Value& v, std::string_view ptr) { return std::string(str_at(v, ptr).value_or("")); }

// `provider/model` of the provider and model ids at `provider` and `model` in `v`.
std::optional<std::string> model_at(const Value& v, std::string_view provider, std::string_view model) {
    auto p = str_at(v, provider);
    auto m = str_at(v, model);
    if (!p || !m) return std::nullopt;
    return std::string(*p) + "/" + std::string(*m);
}

// Brings a node's text from `current` to `full`: appends when `full` extends it, else replaces it.
std::expected<void, CommitError> sync_text(const DomWriter& w, NodeId node, std::string& current, std::string_view full) {
    if (full.starts_with(current)) {
        auto tail = full.substr(current.size());
        if (tail.empty()) return {};
        if (auto r = w.append(node, std::string(tail)); !r) return r;
        current.append(tail);
        return {};
    }
    if (auto r = w.set_text(node, std::string(full)); !r) return r;
    current = full;
    return {};
}

std::optional<TodoState> todo_state(std::string_view status) {
    if (status == "completed") return TodoState::Done;
    return loa::core::parse<TodoState>(status);
}

std::expected<NodeId, CommitError> insert_result(const DomWriter& w, NodeId call, std::string_view text, bool is_error) {
    Attrs attrs;
    if (is_error) attrs.emplace(std::string(attr::IS_ERROR), CValue(true));
    return w.insert(call, K::Result, std::move(attrs), std::string(text));
}

} // namespace

// One tool part's state, as the DOM needs it.
struct Mapper::ToolState {
    CallStatus status;
    std::string tool, call_id;
    std::optional<std::string> title;
    std::optional<std::int64_t> started, ended;
    std::string input, oc;
    // Output or error text, and whether it is an error.
    std::optional<std::pair<std::string, bool>> output;

    static ToolState of(const Value& part) {
        static const Value null;
        const Value* found = part.find("state");
        const Value& state = found ? *found : null;
        const std::string native(str_at(state, "/status").value_or("pending"));
        ToolState t;
        t.status = native == "pending" ? CallStatus::Pending
                   : native == "running" ? CallStatus::Running
                   : native == "error"   ? CallStatus::Error
                                         : CallStatus::Done;
        // Pending tools stream raw arguments; afterwards `input` holds them, so `x-oc` drops it.
        Value rest = json_util::without(part, TOOL_FIELDS);
        if (native == "pending") {
            t.input = str_or(state, "/raw");
        } else {
            constexpr std::string_view input_ptr[] = {"/state/input"};
            rest = json_util::without(rest, input_ptr);
            const Value* input = state.find("input");
            t.input = input ? json::dump(*input) : std::string("{}");
        }
        if (native == "completed") t.output = std::pair{str_or(state, "/output"), false};
        else if (native == "error") t.output = std::pair{str_or(state, "/error"), true};
        t.tool = str_or(part, "/tool");
        t.call_id = str_or(part, "/callID");
        if (auto title = str_at(state, "/title")) t.title = std::string(*title);
        t.started = int_at(state, "/time/start");
        t.ended = int_at(state, "/time/end");
        t.oc = json_util::rest(rest, {});
        return t;
    }
};

NodeId Mapper::Part::node() const {
    return std::visit([](const auto& k) { return k.node; }, kind);
}

Mapper::Mapper(NodeId root, std::optional<std::string> model) : root_(root), ledger_(root), model_(std::move(model)) {}

loa::contracts::Result<Mapper> Mapper::start(const DomWriter& w, RootInfo info) {
    const Value& native = info.native;
    std::string native_id = str_or(native, "/id");
    const std::int64_t created = int_at(native, "/time/created").value_or(w.now());
    std::optional<std::string> title;
    if (auto t = str_at(native, "/title")) title = std::string(*t);
    std::optional<std::string> cwd = info.cwd;
    if (!cwd) if (auto d = str_at(native, "/directory")) cwd = std::string(*d);
    std::string session_oc = json_util::rest(native, SESSION_FIELDS);
    std::optional<std::string> revert;
    if (const Value* r = native.find("revert"); r && !r->is_null()) revert = json::dump(*r);
    Attrs attrs{
        {std::string(attr::ID), CValue(info.session_id)},
        {std::string(attr::HARNESS), CValue(HARNESS)},
        {std::string(attr::NODE), CValue(info.node)},
        {std::string(attr::CREATED), CValue(created)},
        {std::string(attr::SCHEMA), CValue(static_cast<std::int64_t>(loa::core::SCHEMA_VERSION))},
    };
    const std::pair<std::string_view, std::optional<std::string>> optional[] = {
        {attr::CWD, cwd},
        {attr::PROJECT, info.project},
        {attr::MODEL, info.model},
        {attr::MODEL_ROLE, info.model_role},
        {attr::EFFORT, info.effort},
        {attr::TITLE, title},
        {attr::NATIVE_ID, native_id.empty() ? std::nullopt : std::optional(native_id)},
        {xattr::OC, session_oc.empty() ? std::nullopt : std::optional(session_oc)},
        {xattr::REVERT, revert},
    };
    for (const auto& [k, v] : optional)
        if (v) attrs.insert_or_assign(std::string(k), CValue(*v));
    if (info.mode) attrs.insert_or_assign(std::string(attr::MODE), CValue(*info.mode));
    auto root = w.at(created).insert(NodeId::DOCUMENT, K::Session, std::move(attrs), "");
    if (!root) return std::unexpected(to_adapter(std::move(root).error()));
    Mapper mapper(*root, std::move(info.model));
    mapper.native_id_ = std::move(native_id);
    mapper.session_oc_ = std::move(session_oc);
    mapper.title_ = std::move(title);
    mapper.revert_ = std::move(revert);
    return mapper;
}

Mapper::Result Mapper::set_native_id(const DomWriter& w, std::string_view id) {
    native_id_ = std::string(id);
    OC_TRY(w.set(root_, attr::NATIVE_ID, CValue(id)));
    return {};
}

Mapper::Result Mapper::set_model(const DomWriter& w, std::string_view model) {
    model_ = std::string(model);
    OC_TRY(w.set(root_, attr::MODEL, CValue(model)));
    return {};
}

std::optional<NodeId> Mapper::active() const {
    const auto* turn = ledger_.turn();
    return turn ? std::optional(turn->node) : std::nullopt;
}

loa::contracts::Result<NodeId> Mapper::begin_turn(const DomWriter& w, std::string_view message_id, std::string_view text,
                                                  std::string_view by) {
    if (active()) return std::unexpected(AdapterError::Busy{});
    const auto ts = w.now();
    auto turn = w.insert(root_, K::Turn,
                         Attrs{{std::string(attr::ID), CValue(message_id)},
                               {std::string(attr::STATUS), CValue(TurnStatus::Running)},
                               {std::string(attr::STARTED), CValue(ts)}},
                         "");
    if (!turn) return std::unexpected(to_adapter(std::move(turn).error()));
    Attrs attrs{{std::string(attr::ROLE), CValue(Role::User)},
                {std::string(attr::STATUS), CValue(MessageStatus::Done)},
                {std::string(xattr::MESSAGE), CValue(message_id)}};
    if (!by.empty()) attrs.insert_or_assign(std::string(attr::BY), CValue(by));
    auto message = w.insert(*turn, K::Message, std::move(attrs), std::string(text));
    if (!message) return std::unexpected(to_adapter(std::move(message).error()));
    open_turn(message_id, *turn, ts);
    bind_ = Binding{std::string(message_id), *message, std::string(text)};
    return *turn;
}

void Mapper::open_turn(std::string_view message_id, NodeId turn, std::int64_t ts) {
    turns_.insert_or_assign(std::string(message_id), turn);
    adopt(turn, ts, model_);
}

void Mapper::adopt(NodeId turn, std::int64_t ts, std::optional<std::string> model) {
    turn_seq_.push_back(turn);
    current_ = TurnState{};
    current_.last_ts = ts;
    current_.model = std::move(model);
    ledger_.adopt(turn);
}

Mapper::Result Mapper::fail_turn(const DomWriter& w, NodeId turn, std::string_view message) {
    OC_TRY(w.ext(turn, HARNESS, ext::ERROR, std::string(message)));
    return close_active(w, TurnStatus::Error);
}

Mapper::Result Mapper::cancel_turn(const DomWriter& w) { return close_active(w, TurnStatus::Cancelled); }

Mapper::Result Mapper::finish(const DomWriter& w) {
    return close_active(w.at(std::max(current_.last_ts, w.now())), std::nullopt);
}

Mapper::Result Mapper::apply_event(const DomWriter& w, const Event& event) {
    const auto sid = event.session_id();
    if (!sid) return {};
    if (*sid != native_id_) {
        if (event.known && event.known->kind == Known::Kind::SessionCreated &&
            str_at(event.known->info, "/parentID") == std::optional<std::string_view>(native_id_)) {
            if (auto id = str_at(event.known->info, "/id")) children_.try_emplace(std::string(*id));
            return {};
        }
        if (auto it = children_.find(std::string(*sid)); it != children_.end()) {
            const std::string child = it->first;
            return child_event(w, child, event);
        }
        return {};
    }
    if (!event.known) {
        // `session.next.*` repeat the classic events; the others derive from messages the DOM holds.
        const auto& kind = event.type;
        if (kind.starts_with("session.next.") || kind == "session.diff" || kind == "command.executed" ||
            kind == "session.compacted")
            return {};
        OC_TRY(w.ext(root_, HARNESS, kind, json::dump(event.properties)));
        return {};
    }
    const Known& k = *event.known;
    using KK = Known::Kind;
    switch (k.kind) {
    case KK::SessionCreated: return {};
    case KK::SessionUpdated: return session_updated(w, k.info);
    case KK::SessionStatus: {
        const auto type = str_at(k.status, "/type");
        if (type == "idle") return close_active(w, std::nullopt);
        if (type == "retry") OC_TRY(w.ext(active().value_or(root_), HARNESS, "session.status", json::dump(k.status)));
        return {};
    }
    case KK::SessionIdle: return close_active(w, std::nullopt);
    case KK::SessionError: return session_error(w, k.error);
    case KK::MessageUpdated: return message_updated(w, k.info);
    case KK::MessageRemoved: return message_removed(w, k.at.message);
    case KK::PartUpdated: return part_updated(w, k.part);
    case KK::PartDelta: return part_delta(w, k.at.part, k.field, k.delta);
    case KK::PartRemoved: {
        auto it = parts_.find(k.at.part);
        if (it == parts_.end()) return {};
        const NodeId node = it->second.node();
        parts_.erase(it);
        OC_TRY(w.remove(node));
        return {};
    }
    case KK::TodoUpdated: return sync_todos(w, k.todos);
    case KK::PermissionAsked: return permission_asked(w, *k.permission, std::nullopt);
    case KK::PermissionReplied: return permission_replied(w, k.at.request, k.reply);
    case KK::QuestionAsked: return question_asked(w, *k.question, std::nullopt);
    case KK::QuestionReplied: return question_replied(w, k.at.request, k.answers);
    case KK::QuestionRejected: return request_closed(w, k.at.request);
    }
    return {};
}

Mapper::Result Mapper::session_updated(const DomWriter& w, const Value& info) {
    if (auto title = str_at(info, "/title"); title && (!title_ || *title_ != *title)) {
        OC_TRY(w.set(root_, attr::TITLE, CValue(*title)));
        title_ = std::string(*title);
    }
    std::optional<std::string> revert;
    if (const Value* r = info.find("revert"); r && !r->is_null()) revert = json::dump(*r);
    if (revert != revert_) {
        if (revert) OC_TRY(w.set(root_, xattr::REVERT, CValue(*revert)));
        else OC_TRY(w.clear(root_, xattr::REVERT));
        revert_ = std::move(revert);
    }
    auto oc = json_util::rest(info, SESSION_FIELDS);
    if (oc != session_oc_) {
        OC_TRY(w.set(root_, xattr::OC, CValue(oc)));
        session_oc_ = std::move(oc);
    }
    return {};
}

Mapper::Result Mapper::session_error(const DomWriter& w, const Value& error) {
    if (error.is_null()) return {};
    const bool aborted = str_at(error, "/name") == std::optional<std::string_view>("MessageAbortedError");
    if (active()) (aborted ? current_.aborted : current_.error) = true;
    if (aborted) return {};
    OC_TRY(w.ext(active().value_or(root_), HARNESS, "session.error", json::dump(error)));
    return {};
}

Mapper::Result Mapper::message_updated(const DomWriter& w, const Value& info) {
    auto id = str_at(info, "/id");
    if (!id) return std::unexpected(AdapterError::Protocol{"message without id"});
    const std::string key(*id);
    if (str_at(info, "/role") == std::optional<std::string_view>("user")) return user_message(w, key, info);
    return assistant_message(w, key, info);
}

Mapper::Result Mapper::user_message(const DomWriter& w, const std::string& id, const Value& info) {
    auto oc = json_util::rest(info, USER_FIELDS);
    if (auto it = messages_.find(id); it != messages_.end()) {
        if (it->second.oc != oc) {
            OC_TRY(w.set(it->second.turn, xattr::OC, CValue(oc)));
            it->second.oc = std::move(oc);
        }
        return {};
    }
    const auto created = int_at(info, "/time/created");
    NodeId turn;
    if (auto it = turns_.find(id); it != turns_.end()) {
        turn = it->second;
        // The harness's own time wins over the app's, so exports match the native file.
        if (created) OC_TRY(w.set(turn, attr::STARTED, CValue(*created)));
    } else {
        // A prompt the app didn't send: the TUI, a command, compaction.
        OC_TRY_A(close_active(w.at(current_.last_ts), std::nullopt));
        const auto started = created.value_or(w.now());
        auto inserted = w.at(started).insert(root_, K::Turn,
                                             Attrs{{std::string(attr::ID), CValue(id)},
                                                   {std::string(attr::STATUS), CValue(TurnStatus::Running)},
                                                   {std::string(attr::STARTED), CValue(started)}},
                                             "");
        if (!inserted) return std::unexpected(to_adapter(std::move(inserted).error()));
        turn = *inserted;
        open_turn(id, turn, started);
    }
    if (!oc.empty()) OC_TRY(w.set(turn, xattr::OC, CValue(oc)));
    if (auto model = model_at(info, "/model/providerID", "/model/modelID"); model && active() == turn)
        current_.model = std::move(model);
    messages_.insert_or_assign(id, Msg{turn, true, std::nullopt, std::move(oc), std::nullopt, false, false});
    return {};
}

Mapper::Result Mapper::assistant_message(const DomWriter& w, const std::string& id, const Value& info) {
    auto model = model_at(info, "/providerID", "/modelID");
    if (model) model_ = model;
    auto text = json_util::rest(info, ASSISTANT_FIELDS);
    const bool summary = is_true(info, "/summary");
    NodeId turn;
    if (auto it = messages_.find(id); it != messages_.end()) {
        Msg& msg = it->second;
        if (msg.oc != text && msg.ext) {
            OC_TRY(w.set_text(*msg.ext, text));
            msg.oc = text;
        }
        msg.summary = summary;
        if (model) msg.model = model;
        turn = msg.turn;
    } else {
        std::optional<NodeId> parent;
        if (auto p = str_at(info, "/parentID"))
            if (auto t = turns_.find(std::string(*p)); t != turns_.end()) parent = t->second;
        if (!parent) parent = active();
        if (parent) {
            turn = *parent;
        } else {
            // No prompt in view: the user message was removed or predates a fork.
            auto inserted = w.insert(root_, K::Turn,
                                     Attrs{{std::string(attr::STATUS), CValue(TurnStatus::Running)},
                                           {std::string(attr::STARTED), CValue(w.now())}},
                                     "");
            if (!inserted) return std::unexpected(to_adapter(std::move(inserted).error()));
            turn = *inserted;
            adopt(turn, w.now(), std::nullopt);
        }
        auto ext_node = w.at(int_at(info, "/time/created").value_or(w.now()))
                            .insert(turn, K::Ext,
                                    Attrs{{std::string(attr::HARNESS), CValue(HARNESS)},
                                          {std::string(attr::TYPE), CValue(ext::MESSAGE)},
                                          {std::string(xattr::MESSAGE), CValue(id)}},
                                    text);
        if (!ext_node) return std::unexpected(to_adapter(std::move(ext_node).error()));
        messages_.insert_or_assign(id, Msg{turn, false, *ext_node, text, model, summary, false});
    }
    const auto completed = int_at(info, "/time/completed");
    if (active() == turn) {
        if (auto it = messages_.find(id); it != messages_.end() && it->second.model) current_.model = it->second.model;
        current_.last_ts = std::max(current_.last_ts, completed.value_or(std::numeric_limits<std::int64_t>::min()));
        if (auto name = str_at(info, "/error/name")) {
            if (*name == "MessageAbortedError") current_.aborted = true;
            else current_.error = true;
        }
    }
    if (summary && completed) return record_compaction(w, id, turn);
    return {};
}

Mapper::Result Mapper::record_compaction(const DomWriter& w, const std::string& message_id, NodeId turn) {
    auto it = messages_.find(message_id);
    if (it == messages_.end() || it->second.compacted) return {};
    it->second.compacted = true;
    auto model = it->second.model;
    std::vector<std::pair<NodeId, std::string_view>> texts;
    for (const auto& [_, p] : parts_) {
        if (p.message != message_id) continue;
        if (const auto* t = std::get_if<TextPart>(&p.kind)) texts.emplace_back(t->node, t->text);
    }
    std::ranges::sort(texts, {}, &std::pair<NodeId, std::string_view>::first);
    std::string summary;
    for (std::size_t i = 0; i < texts.size(); ++i) {
        if (i) summary += "\n\n";
        summary += texts[i].second;
    }
    const auto found = std::ranges::find(turn_seq_, turn);
    const std::size_t at = static_cast<std::size_t>(found - turn_seq_.begin());
    CValue::List covers;
    for (std::size_t i = std::min(covered_, at); i < at; ++i) covers.emplace_back(turn_seq_[i]);
    covered_ = at;
    Attrs attrs{{std::string(attr::COVERS), CValue(std::move(covers))}, {std::string(attr::CREATED), CValue(w.now())}};
    if (model) attrs.insert_or_assign(std::string(attr::MODEL), CValue(*model));
    OC_TRY(w.insert(root_, K::Compaction, std::move(attrs), std::move(summary)));
    return {};
}

Mapper::Result Mapper::message_removed(const DomWriter& w, const std::string& message_id) {
    auto it = messages_.find(message_id);
    if (it == messages_.end()) return {};
    const Msg msg = std::move(it->second);
    messages_.erase(it);
    if (msg.user) {
        // Reverting drops the whole exchange: the turn and all it holds.
        OC_TRY(w.remove(msg.turn));
        turns_.erase(message_id);
        std::erase(turn_seq_, msg.turn);
        covered_ = std::min(covered_, turn_seq_.size());
        std::erase_if(messages_, [&](const auto& e) { return e.second.turn == msg.turn; });
        std::erase_if(parts_, [&](const auto& e) { return e.second.turn == msg.turn; });
        std::erase_if(calls_, [&](const auto& e) { return e.second.second == msg.turn; });
        std::erase_if(decisions_, [&](const auto& e) { return e.second.turn == msg.turn; });
        if (active() == msg.turn) ledger_ = kit::TurnLedger<std::string>(root_);
        return {};
    }
    std::vector<NodeId> doomed;
    if (msg.ext) doomed.push_back(*msg.ext);
    for (auto p = parts_.begin(); p != parts_.end();) {
        if (p->second.message == message_id) {
            doomed.push_back(p->second.node());
            p = parts_.erase(p);
        } else {
            ++p;
        }
    }
    std::ranges::sort(doomed);
    for (NodeId node : doomed) OC_TRY(w.remove(node));
    return {};
}

Mapper::Result Mapper::part_delta(const DomWriter& w, const std::string& part, std::string_view field, std::string_view delta) {
    if (field != "text") return {};
    auto it = parts_.find(part);
    if (it == parts_.end()) return {};
    auto* text = std::get_if<TextPart>(&it->second.kind);
    if (!text) return {};
    OC_TRY(w.append(text->node, std::string(delta)));
    text->text.append(delta);
    return {};
}

Mapper::Result Mapper::part_updated(const DomWriter& w, const Value& part) {
    auto id = str_at(part, "/id");
    if (!id) return std::unexpected(AdapterError::Protocol{"part without id"});
    auto message = str_at(part, "/messageID");
    if (!message) return std::unexpected(AdapterError::Protocol{"part " + std::string(*id) + " has no message"});
    const std::string key(*id), owner_id(*message);
    auto it = messages_.find(owner_id);
    if (it == messages_.end())
        return std::unexpected(AdapterError::Protocol{"part " + key + " of unknown message " + owner_id});
    const Owner owner{it->second.turn, it->second.user, it->second.model};
    const auto type = str_at(part, "/type");
    if (type == "text" && !is_true(part, "/synthetic")) return text_part(w, key, owner_id, part, owner, false);
    if (type == "reasoning") return text_part(w, key, owner_id, part, owner, true);
    if (type == "tool") return tool_part(w, key, owner_id, part, owner.turn);
    return ext_part(w, key, owner_id, part, owner.turn);
}

Mapper::Result Mapper::text_part(const DomWriter& w, const std::string& id, const std::string& message, const Value& part,
                                 const Owner& owner, bool thinking) {
    const std::string full = str_or(part, "/text");
    auto oc = json_util::rest(part, TEXT_FIELDS);
    const auto status =
        owner.user || int_at(part, "/time/end") ? MessageStatus::Done : MessageStatus::Streaming;
    if (auto it = parts_.find(id); it != parts_.end()) {
        Part& entry = it->second;
        auto* text = std::get_if<TextPart>(&entry.kind);
        if (!text) return {};
        OC_TRY(sync_text(w, text->node, text->text, full));
        if (entry.oc != oc) {
            OC_TRY(w.set(text->node, xattr::OC, CValue(oc)));
            entry.oc = std::move(oc);
        }
        if (text->status == MessageStatus::Streaming && status == MessageStatus::Done) {
            OC_TRY(w.set(text->node, attr::STATUS, CValue(status)));
            text->status = status;
        }
        return {};
    }
    auto node = new_text(w, message, part, oc, owner, thinking, status);
    if (!node) return std::unexpected(std::move(node).error());
    parts_.insert_or_assign(id, Part{owner.turn, message, std::move(oc), TextPart{*node, full, status}});
    return {};
}

loa::contracts::Result<NodeId> Mapper::new_text(const DomWriter& w, const std::string& message, const Value& part,
                                                const std::string& oc, const Owner& owner, bool thinking,
                                                MessageStatus status) {
    const std::string full = str_or(part, "/text");
    if (owner.user && !thinking && bind_ && bind_->message == message) {
        Binding bound = std::move(*bind_);
        bind_.reset();
        OC_TRY(w.set(bound.node, attr::ID, CValue(str_or(part, "/id"))));
        if (!oc.empty()) OC_TRY(w.set(bound.node, xattr::OC, CValue(oc)));
        OC_TRY(sync_text(w, bound.node, bound.text, full));
        return bound.node;
    }
    Attrs attrs{{std::string(attr::ROLE), CValue(owner.user ? Role::User : Role::Assistant)},
                {std::string(attr::ID), CValue(str_or(part, "/id"))},
                {std::string(attr::STATUS), CValue(status)},
                {std::string(xattr::MESSAGE), CValue(message)}};
    if (thinking) attrs.insert_or_assign(std::string(attr::KIND), CValue(MessageKind::Thinking));
    if (owner.model && !owner.user) attrs.insert_or_assign(std::string(attr::MODEL), CValue(*owner.model));
    if (!oc.empty()) attrs.insert_or_assign(std::string(xattr::OC), CValue(oc));
    auto node = w.at(int_at(part, "/time/start").value_or(w.now())).insert(owner.turn, K::Message, std::move(attrs), full);
    if (!node) return std::unexpected(to_adapter(std::move(node).error()));
    return *node;
}

Mapper::Result Mapper::tool_part(const DomWriter& w, const std::string& id, const std::string& message, const Value& part,
                                 NodeId turn) {
    const ToolState tool = ToolState::of(part);
    if (auto it = parts_.find(id); it != parts_.end()) {
        if (auto* call = std::get_if<Call>(&it->second.kind)) {
            std::string& oc = it->second.oc;
            OC_TRY(sync_text(w, call->input, call->input_text, tool.input));
            if (tool.title && call->title != tool.title) {
                OC_TRY(w.set(call->node, attr::TITLE, CValue(*tool.title)));
                call->title = tool.title;
            }
            if (tool.started && call->started != tool.started) {
                OC_TRY(w.set(call->node, attr::STARTED, CValue(*tool.started)));
                call->started = tool.started;
            }
            if (tool.output) {
                if (call->result) {
                    OC_TRY(sync_text(w, call->result->first, call->result->second, tool.output->first));
                } else {
                    auto r = insert_result(w.at(tool.ended.value_or(w.now())), call->node, tool.output->first, tool.output->second);
                    if (!r) return std::unexpected(to_adapter(std::move(r).error()));
                    call->result = std::pair{*r, tool.output->first};
                }
            }
            if (tool.ended && call->ended != tool.ended) {
                OC_TRY(w.set(call->node, attr::ENDED, CValue(*tool.ended)));
                call->ended = tool.ended;
            }
            // A cancelled call stays cancelled: the interrupt decided it.
            if (tool.status != call->status && call->status != CallStatus::Cancelled) {
                OC_TRY(w.set(call->node, attr::STATUS, CValue(tool.status)));
                call->status = tool.status;
            }
            if (oc != tool.oc) {
                OC_TRY(w.set(call->node, xattr::OC, CValue(tool.oc)));
                oc = tool.oc;
            }
        }
    } else {
        const auto at = tool.started.value_or(w.now());
        Attrs attrs{{std::string(attr::TOOL), CValue(tool.tool)},
                    {std::string(attr::CALL_ID), CValue(tool.call_id)},
                    {std::string(attr::STATUS), CValue(tool.status)},
                    {std::string(attr::STARTED), CValue(at)},
                    {std::string(xattr::PART), CValue(id)},
                    {std::string(xattr::MESSAGE), CValue(message)}};
        if (tool.title) attrs.insert_or_assign(std::string(attr::TITLE), CValue(*tool.title));
        if (tool.ended) attrs.insert_or_assign(std::string(attr::ENDED), CValue(*tool.ended));
        if (!tool.oc.empty()) attrs.insert_or_assign(std::string(xattr::OC), CValue(tool.oc));
        const DomWriter wa = w.at(at);
        auto node = wa.insert(turn, K::Call, std::move(attrs), "");
        if (!node) return std::unexpected(to_adapter(std::move(node).error()));
        auto input = wa.insert(*node, K::Input, {}, tool.input);
        if (!input) return std::unexpected(to_adapter(std::move(input).error()));
        Call call{*node, *input, tool.input, std::nullopt, tool.status, tool.title, at, tool.ended, std::nullopt};
        if (tool.output) {
            auto r = insert_result(wa.at(tool.ended.value_or(at)), *node, tool.output->first, tool.output->second);
            if (!r) return std::unexpected(to_adapter(std::move(r).error()));
            call.result = std::pair{*r, tool.output->first};
        }
        calls_.insert_or_assign(tool.call_id, std::pair{*node, turn});
        parts_.insert_or_assign(id, Part{turn, message, tool.oc, std::move(call)});
    }
    static const Value null;
    const Value* found = part.find("state");
    const Value& state = found ? *found : null;
    if (tool.tool == "task") return task_job(w, id, turn, state);
    if (tool.tool == "todowrite" && tool.status == CallStatus::Done && tool.output && !tool.output->second) {
        if (const Value* todos = json_util::at(state, "/input/todos"))
            if (auto parsed = parse_todos(*todos)) return sync_todos(w.at(tool.ended.value_or(w.now())), *parsed);
    }
    return {};
}

Mapper::Result Mapper::task_job(const DomWriter& w, const std::string& part, NodeId turn, const Value& state) {
    auto it = parts_.find(part);
    if (it == parts_.end()) return {};
    auto* call = std::get_if<Call>(&it->second.kind);
    if (!call) return {};
    NodeId job;
    if (call->job) {
        job = *call->job;
    } else {
        auto child = str_at(state, "/metadata/sessionId");
        if (!child) return {};
        const auto started = call->started.value_or(w.now());
        Attrs attrs{{std::string(attr::KIND), CValue(JobKind::Subagent)},
                    {std::string(attr::STATUS), CValue(JobStatus::Running)},
                    {std::string(attr::CALL), CValue(call->node)},
                    {std::string(attr::STARTED), CValue(started)},
                    {std::string(xattr::NATIVE_SESSION), CValue(*child)}};
        auto title = str_at(state, "/title");
        if (!title) title = str_at(state, "/input/description");
        if (title) attrs.insert_or_assign(std::string(attr::TITLE), CValue(*title));
        if (auto agent = str_at(state, "/input/subagent_type")) attrs.insert_or_assign(std::string(attr::AGENT), CValue(*agent));
        Child& entry = children_[std::string(*child)];
        auto inserted = w.at(started).insert(call->node, K::Job, std::move(attrs), "");
        if (!inserted) return std::unexpected(to_adapter(std::move(inserted).error()));
        job = *inserted;
        entry.job = job;
        entry.call = call->node;
        entry.turn = turn;
        call->job = job;
    }
    JobStatus status;
    switch (call->status) {
    case CallStatus::Done: status = JobStatus::Done; break;
    case CallStatus::Error: status = JobStatus::Error; break;
    case CallStatus::Cancelled: status = JobStatus::Cancelled; break;
    default: return {};
    }
    const auto ended = call->ended;
    for (auto& [_, child] : children_) {
        if (child.job != job || !child.turn) continue;
        child.turn.reset();
        OC_TRY(w.set(job, attr::STATUS, CValue(status)));
        OC_TRY(w.set(job, attr::ENDED, CValue(ended.value_or(w.now()))));
        break;
    }
    return {};
}

Mapper::Result Mapper::ext_part(const DomWriter& w, const std::string& id, const std::string& message, const Value& part,
                                NodeId turn) {
    auto text = json::dump(json_util::without(part, EXT_FIELDS));
    if (str_at(part, "/type") == std::optional<std::string_view>("step-finish") && active() == turn) {
        const auto n = [&](std::string_view ptr) { return int_at(part, ptr).value_or(0); };
        Usage usage{n("/tokens/input"), n("/tokens/output"), n("/tokens/reasoning"), n("/tokens/cache/read"),
                    n("/tokens/cache/write"), 0};
        if (const Value* cost = json_util::at(part, "/cost"))
            if (auto f = cost->as_f64()) usage.cost = *f;
        current_.steps.insert_or_assign(id, usage);
    }
    if (auto it = parts_.find(id); it != parts_.end()) {
        if (auto* e = std::get_if<ExtPart>(&it->second.kind); e && e->text != text) {
            OC_TRY(w.set_text(e->node, text));
            e->text = std::move(text);
        }
        return {};
    }
    auto node = w.insert(turn, K::Ext,
                         Attrs{{std::string(attr::HARNESS), CValue(HARNESS)},
                               {std::string(attr::TYPE), CValue(ext::PART)},
                               {std::string(xattr::PART), CValue(id)},
                               {std::string(xattr::MESSAGE), CValue(message)}},
                         text);
    if (!node) return std::unexpected(to_adapter(std::move(node).error()));
    parts_.insert_or_assign(id, Part{turn, message, std::string(), ExtPart{*node, std::move(text)}});
    return {};
}

Mapper::Result Mapper::close_active(const DomWriter& w, std::optional<TurnStatus> forced) {
    const auto turn = active();
    if (!turn) return {};
    TurnState st = std::exchange(current_, TurnState{});
    const TurnStatus derived = st.error ? TurnStatus::Error : st.aborted ? TurnStatus::Cancelled : TurnStatus::Done;
    const TurnStatus status = forced.value_or(derived);
    std::vector<Part*> parts;
    for (auto& [_, p] : parts_)
        if (p.turn == *turn) parts.push_back(&p);
    std::ranges::sort(parts, {}, [](const Part* p) { return p->node(); });
    for (Part* p : parts) {
        if (auto* text = std::get_if<TextPart>(&p->kind); text && text->status == MessageStatus::Streaming) {
            text->status = status == TurnStatus::Cancelled ? MessageStatus::Cancelled : MessageStatus::Done;
            OC_TRY(w.set(text->node, attr::STATUS, CValue(text->status)));
        } else if (auto* call = std::get_if<Call>(&p->kind);
                   call && (call->status == CallStatus::Pending || call->status == CallStatus::Running)) {
            call->status = CallStatus::Cancelled;
            OC_TRY(w.set(call->node, attr::STATUS, CValue(CallStatus::Cancelled)));
            if (!call->ended) {
                OC_TRY(w.set(call->node, attr::ENDED, CValue(w.now())));
                call->ended = w.now();
            }
            if (call->job) {
                for (auto& [_, child] : children_) {
                    if (child.job != call->job || !child.turn) continue;
                    child.turn.reset();
                    OC_TRY(w.close(*call->job, CValue(JobStatus::Cancelled)));
                    break;
                }
            }
        }
    }
    OC_TRY(ledger_.end_turn(w, status, [&](const kit::EndEvent<std::string>& event) {
        if (const auto* d = std::get_if<kit::end_event::Decision>(&event))
            if (auto it = decisions_.find(d->node); it != decisions_.end()) it->second.waiting = false;
    }));
    if (!st.steps.empty()) {
        Usage u;
        for (const auto& [_, s] : st.steps) {
            u.input += s.input;
            u.output += s.output;
            u.reasoning += s.reasoning;
            u.cache_read += s.cache_read;
            u.cache_write += s.cache_write;
            u.cost += s.cost;
        }
        Attrs attrs{{std::string(attr::INPUT_TOKENS), CValue(u.input)},
                    {std::string(attr::OUTPUT_TOKENS), CValue(u.output)},
                    {std::string(attr::CACHE_READ_TOKENS), CValue(u.cache_read)},
                    {std::string(attr::CACHE_WRITE_TOKENS), CValue(u.cache_write)},
                    {std::string(attr::COST), CValue(u.cost)},
                    {std::string(xattr::REASONING_TOKENS), CValue(u.reasoning)}};
        if (st.model) attrs.insert_or_assign(std::string(attr::MODEL), CValue(*st.model));
        OC_TRY(w.insert(*turn, K::Usage, std::move(attrs), ""));
    }
    if (bind_) {
        if (auto it = turns_.find(bind_->message); it != turns_.end() && it->second == *turn) bind_.reset();
    }
    return {};
}

Mapper::Result Mapper::sync_todos(const DomWriter& w, const std::vector<Todo>& todos) {
    for (std::size_t i = 0; i < todos.size(); ++i) {
        const Todo& todo = todos[i];
        const auto state = todo_state(todo.status);
        if (i >= todos_.size()) {
            Attrs attrs{{std::string(attr::STATE), CValue(state.value_or(TodoState::Pending))}};
            if (!state) attrs.insert_or_assign(std::string(xattr::STATUS), CValue(todo.status));
            if (!todo.priority.empty()) attrs.insert_or_assign(std::string(xattr::PRIORITY), CValue(todo.priority));
            auto node = w.insert(root_, K::Todo, std::move(attrs), todo.content);
            if (!node) return std::unexpected(to_adapter(std::move(node).error()));
            todos_.emplace_back(*node, todo);
            continue;
        }
        auto& [node, current] = todos_[i];
        if (current.content != todo.content) OC_TRY(w.set_text(node, todo.content));
        if (current.status != todo.status) {
            OC_TRY(w.set(node, attr::STATE, CValue(state.value_or(TodoState::Pending))));
            if (state) {
                if (!todo_state(current.status)) OC_TRY(w.clear(node, xattr::STATUS));
            } else {
                OC_TRY(w.set(node, xattr::STATUS, CValue(todo.status)));
            }
        }
        if (current.priority != todo.priority) OC_TRY(w.set(node, xattr::PRIORITY, CValue(todo.priority)));
        current = todo;
    }
    while (todos_.size() > todos.size()) {
        const NodeId node = todos_[todos.size()].first;
        todos_.erase(todos_.begin() + static_cast<std::ptrdiff_t>(todos.size()));
        OC_TRY(w.remove(node));
    }
    return {};
}

Mapper::Place Mapper::request_place(const std::optional<ToolRef>& tool, std::optional<std::string_view> child) const {
    if (child) {
        if (auto it = children_.find(std::string(*child)); it != children_.end() && it->second.call && it->second.turn)
            return {*it->second.call, it->second.call, it->second.turn};
    }
    if (tool) {
        if (auto it = calls_.find(tool->call_id); it != calls_.end())
            return {it->second.first, it->second.first, it->second.second};
    }
    if (auto turn = active()) return {*turn, std::nullopt, turn};
    return {root_, std::nullopt, std::nullopt};
}

Mapper::Result Mapper::waiting(const DomWriter& w, NodeId node, const std::string& request_id, Pending pending) {
    pending.request_id = request_id;
    pending.waiting = true;
    const auto turn = pending.turn;
    decisions_.insert_or_assign(node, std::move(pending));
    if (turn && turn == active()) {
        ledger_.wait(node);
        OC_TRY(w.set(ledger_.turn()->node, attr::STATUS, CValue(TurnStatus::Waiting)));
    }
    return {};
}

Mapper::Result Mapper::permission_asked(const DomWriter& w, const PermissionRequest& request,
                                        std::optional<std::string_view> child) {
    for (const auto& [_, d] : decisions_)
        if (d.request_id == request.id) return {};
    const Place place = request_place(request.tool, child);
    std::string patterns, lines;
    for (std::size_t i = 0; i < request.patterns.size(); ++i) {
        if (i) {
            patterns += ", ";
            lines += "\n";
        }
        patterns += request.patterns[i];
        lines += request.patterns[i];
    }
    const std::string title = request.patterns.empty() ? request.permission : request.permission + ": " + patterns;
    std::string body = lines;
    for (std::string_view p : {"/command", "/diff", "/filepath"}) {
        if (auto s = str_at(request.metadata, p)) {
            body = std::string(*s);
            break;
        }
    }
    json::Array pats(request.patterns.begin(), request.patterns.end());
    json::Array always(request.always.begin(), request.always.end());
    const Value oc(json::Object{{"permission", request.permission},
                                {"patterns", std::move(pats)},
                                {"always", std::move(always)},
                                {"metadata", request.metadata}});
    Attrs attrs{{std::string(attr::KIND), CValue(DecisionKind::Approval)},
                {std::string(attr::STATUS), CValue(DecisionStatus::Waiting)},
                {std::string(attr::REQUEST_ID), CValue(request.id)},
                {std::string(attr::TITLE), CValue(title)},
                {std::string(attr::OPTIONS),
                 CValue(CValue::List{CValue(loa::core::answer::ALLOW_ONCE), CValue(loa::core::answer::ALLOW_SESSION),
                                     CValue(loa::core::answer::DENY)})},
                {std::string(attr::CREATED), CValue(w.now())},
                {std::string(xattr::OC), CValue(json::dump(oc))}};
    if (place.call) attrs.insert_or_assign(std::string(attr::CALL), CValue(*place.call));
    auto node = w.insert(place.parent, K::Decision, std::move(attrs), std::move(body));
    if (!node) return std::unexpected(to_adapter(std::move(node).error()));
    Pending pending;
    pending.turn = place.turn;
    return waiting(w, *node, request.id, std::move(pending));
}

Mapper::Result Mapper::question_asked(const DomWriter& w, const QuestionRequest& request,
                                      std::optional<std::string_view> child) {
    for (const auto& [_, d] : decisions_)
        if (d.request_id == request.id) return {};
    const Place place = request_place(request.tool, child);
    const std::size_t count = request.questions.size();
    for (std::size_t index = 0; index < count; ++index) {
        const QuestionInfo& q = request.questions[index];
        CValue::List options;
        for (const auto& o : q.options) options.emplace_back(o.label);
        Attrs attrs{{std::string(attr::KIND), CValue(DecisionKind::Question)},
                    {std::string(attr::STATUS), CValue(DecisionStatus::Waiting)},
                    {std::string(attr::REQUEST_ID), CValue(request.id)},
                    {std::string(attr::OPTIONS), CValue(std::move(options))},
                    {std::string(attr::MULTI), CValue(q.multiple.value_or(false))},
                    {std::string(attr::CREATED), CValue(w.now())},
                    {std::string(xattr::INDEX), CValue(static_cast<std::int64_t>(index))}};
        if (!q.header.empty()) attrs.insert_or_assign(std::string(attr::TITLE), CValue(q.header));
        if (q.custom) attrs.insert_or_assign(std::string(xattr::CUSTOM), CValue(*q.custom));
        if (place.call) attrs.insert_or_assign(std::string(attr::CALL), CValue(*place.call));
        auto node = w.insert(place.parent, K::Decision, std::move(attrs), q.question);
        if (!node) return std::unexpected(to_adapter(std::move(node).error()));
        Pending pending;
        pending.question = true;
        pending.index = index;
        pending.count = count;
        pending.turn = place.turn;
        OC_TRY_A(waiting(w, *node, request.id, std::move(pending)));
    }
    return {};
}

loa::contracts::Result<std::optional<Reply>> Mapper::plan_reply(NodeId decision, const Answer& answer) const {
    auto it = decisions_.find(decision);
    if (it == decisions_.end() || !it->second.waiting) return std::unexpected(AdapterError::UnknownDecision{decision});
    const Pending& pending = it->second;
    const std::string& id = pending.request_id;
    if (!pending.question) {
        std::string_view reply;
        const std::optional<std::string_view> first =
            answer.values.empty() ? std::nullopt : std::optional<std::string_view>(answer.values.front());
        if (first == loa::core::answer::ALLOW_ONCE) reply = "once";
        else if (first == loa::core::answer::ALLOW_SESSION) reply = "always";
        else if (first == loa::core::answer::DENY) reply = "reject";
        else
            return std::unexpected(AdapterError::Protocol{
                "an approval takes allow_once, allow_session or deny, not " +
                (first ? "Some(\"" + std::string(*first) + "\")" : std::string("None"))});
        Value body(json::Object{{"reply", reply}});
        if (answer.reason && reply == "reject") body.set("message", *answer.reason);
        return Reply{"/permission/" + id + "/reply", std::move(body)};
    }
    if (answer.values.empty()) return Reply{"/question/" + id + "/reject", std::nullopt};
    std::vector<std::vector<std::string>> answers(pending.count);
    for (const auto& [_, d] : decisions_) {
        if (d.request_id != id || !d.question || d.index >= answers.size()) continue;
        if (d.index == pending.index) answers[d.index] = answer.values;
        else if (d.answer) answers[d.index] = *d.answer;
        else return std::optional<Reply>{};
    }
    json::Array rows;
    for (auto& row : answers) rows.emplace_back(json::Array(row.begin(), row.end()));
    return Reply{"/question/" + id + "/reply", Value(json::Object{{"answers", std::move(rows)}})};
}

Mapper::Result Mapper::record_answer(const DomWriter& w, NodeId decision, const Answer& answer) {
    auto it = decisions_.find(decision);
    if (it == decisions_.end() || !it->second.waiting) return std::unexpected(AdapterError::UnknownDecision{decision});
    Pending& pending = it->second;
    const bool rejected = answer.values.empty() && pending.question;
    pending.waiting = false;
    pending.answer = answer.values;
    const auto turn = pending.turn;
    const std::string request_id = pending.request_id;
    ledger_.answered(decision);
    if (!rejected) OC_TRY(w.set(decision, attr::ANSWER, CValue(answer.values)));
    if (!answer.by.empty()) OC_TRY(w.set(decision, attr::BY, CValue(answer.by)));
    if (rejected) {
        // Dismissing one question rejects the whole request.
        OC_TRY(w.close(decision, CValue(DecisionStatus::Cancelled)));
        return request_closed(w, request_id);
    }
    if (answer.reason) OC_TRY(w.set(decision, attr::REASON, CValue(*answer.reason)));
    OC_TRY(w.close(decision, CValue(DecisionStatus::Answered)));
    return resume_turn(w, turn);
}

Mapper::Result Mapper::resume_turn(const DomWriter& w, std::optional<NodeId> turn) const {
    if (!turn || turn != active()) return {};
    for (const auto& [_, d] : decisions_)
        if (d.waiting && d.turn == turn) return {};
    OC_TRY(w.set(*turn, attr::STATUS, CValue(TurnStatus::Running)));
    return {};
}

std::vector<NodeId> Mapper::waiting_on(std::string_view request_id) const {
    std::vector<NodeId> out;
    for (const auto& [node, d] : decisions_)
        if (d.waiting && d.request_id == request_id) out.push_back(node);
    return out;
}

Mapper::Result Mapper::permission_replied(const DomWriter& w, const std::string& request_id, std::string_view reply) {
    const std::string_view value = reply == "once"     ? loa::core::answer::ALLOW_ONCE
                                   : reply == "always" ? loa::core::answer::ALLOW_SESSION
                                                       : loa::core::answer::DENY;
    for (NodeId node : waiting_on(request_id)) {
        Answer a;
        a.values = {std::string(value)};
        OC_TRY_A(record_answer(w, node, a));
    }
    return {};
}

Mapper::Result Mapper::question_replied(const DomWriter& w, const std::string& request_id,
                                        const std::vector<std::vector<std::string>>& answers) {
    for (NodeId node : waiting_on(request_id)) {
        const Pending& d = decisions_.at(node);
        if (!d.question) continue;
        auto values = d.index < answers.size() ? answers[d.index] : std::vector<std::string>{};
        if (values.empty()) continue;
        Answer a;
        a.values = std::move(values);
        OC_TRY_A(record_answer(w, node, a));
    }
    return {};
}

Mapper::Result Mapper::request_closed(const DomWriter& w, const std::string& request_id) {
    std::vector<std::optional<NodeId>> turns;
    for (NodeId node : waiting_on(request_id)) {
        OC_TRY(w.close(node, CValue(DecisionStatus::Cancelled)));
        Pending& pending = decisions_.at(node);
        pending.waiting = false;
        turns.push_back(pending.turn);
        ledger_.answered(node);
    }
    for (auto turn : turns) OC_TRY_A(resume_turn(w, turn));
    return {};
}

Mapper::Result Mapper::child_event(const DomWriter& w, const std::string& child, const Event& event) {
    if (!event.known) return {};
    const Known& k = *event.known;
    switch (k.kind) {
    case Known::Kind::PermissionAsked: return permission_asked(w, *k.permission, child);
    case Known::Kind::QuestionAsked: return question_asked(w, *k.question, child);
    case Known::Kind::PermissionReplied: return permission_replied(w, k.at.request, k.reply);
    case Known::Kind::QuestionReplied: return question_replied(w, k.at.request, k.answers);
    case Known::Kind::QuestionRejected: return request_closed(w, k.at.request);
    default: return {};
    }
}

loa::contracts::Result<Mapper> Mapper::from_dom(const Dom& dom) {
    const Node* root = dom.root();
    if (!root) return std::unexpected(AdapterError::Format{"the session DOM is empty"});
    const auto s = [&](std::string_view a) -> std::optional<std::string> {
        auto v = root->str_attr(a);
        return v ? std::optional(std::string(*v)) : std::nullopt;
    };
    Mapper m(root->id, s(attr::MODEL));
    m.native_id_ = s(attr::NATIVE_ID).value_or("");
    m.session_oc_ = s(xattr::OC).value_or("");
    m.title_ = s(attr::TITLE);
    m.revert_ = s(xattr::REVERT);
    m.ledger_ = kit::TurnLedger<std::string>::from_dom(dom);
    for (const Node* child : dom.children(root->id)) {
        if (child->kind == K::Turn) {
            m.index_turn(dom, *child);
        } else if (child->kind == K::Todo) {
            std::string status;
            if (auto x = child->str_attr(xattr::STATUS)) status = std::string(*x);
            else {
                auto st = child->str_attr(attr::STATE);
                status = st == "done" ? "completed" : std::string(st.value_or("pending"));
            }
            m.todos_.emplace_back(child->id, Todo{child->text, std::move(status),
                                                  std::string(child->str_attr(xattr::PRIORITY).value_or(""))});
        } else if (child->kind == K::Compaction) {
            std::optional<std::int64_t> last;
            if (const CValue* covers = child->attr(attr::COVERS))
                if (auto list = covers->as_list())
                    for (const CValue& v : *list)
                        if (auto i = v.as_int(); i && (!last || *i > *last)) last = i;
            if (last) {
                for (std::size_t at = 0; at < m.turn_seq_.size(); ++at)
                    if (CValue(m.turn_seq_[at]) == CValue(*last)) {
                        m.covered_ = at + 1;
                        break;
                    }
            }
        }
    }
    if (auto turn = m.active())
        if (const Node* node = dom.get(*turn)) m.current_.last_ts = node->int_attr(attr::STARTED).value_or(0);
    return m;
}

void Mapper::index_turn(const Dom& dom, const Node& turn) {
    turn_seq_.push_back(turn.id);
    if (auto id = turn.str_attr(attr::ID)) {
        turns_.insert_or_assign(std::string(*id), turn.id);
        messages_.insert_or_assign(std::string(*id), Msg{turn.id, true, std::nullopt,
                                                         std::string(turn.str_attr(xattr::OC).value_or("")), std::nullopt,
                                                         false, true});
    }
    for (const Node* node : dom.children(turn.id)) {
        std::string message(node->str_attr(xattr::MESSAGE).value_or(""));
        std::string oc(node->str_attr(xattr::OC).value_or(""));
        std::optional<std::string_view> id;
        std::optional<std::variant<TextPart, Call, ExtPart>> kind;
        if (node->kind == K::Ext && node->str_attr(attr::TYPE) == ext::MESSAGE) {
            auto info = json_util::parse_object(node->text);
            const Value parsed = info ? std::move(*info) : Value();
            messages_.insert_or_assign(message, Msg{turn.id, false, node->id, node->text,
                                                    model_at(parsed, "/providerID", "/modelID"),
                                                    is_true(parsed, "/summary"), true});
            continue;
        }
        if (node->kind == K::Ext && node->str_attr(attr::TYPE) == ext::PART) {
            id = node->str_attr(xattr::PART);
            kind = ExtPart{node->id, node->text};
            oc.clear();
        } else if (node->kind == K::Message) {
            auto st = node->str_attr(attr::STATUS);
            auto status = st ? loa::core::parse<MessageStatus>(*st) : std::nullopt;
            id = node->str_attr(attr::ID);
            kind = TextPart{node->id, node->text, status.value_or(MessageStatus::Done)};
        } else if (node->kind == K::Call) {
            auto inputs = dom.children_of_kind(node->id, K::Input);
            if (inputs.empty()) continue;
            auto jobs = dom.children_of_kind(node->id, K::Job);
            const Node* job = jobs.empty() ? nullptr : jobs.front();
            if (auto call_id = node->str_attr(attr::CALL_ID))
                calls_.insert_or_assign(std::string(*call_id), std::pair{node->id, turn.id});
            if (job) {
                if (auto child = job->str_attr(xattr::NATIVE_SESSION)) {
                    const bool running = job->str_attr(attr::STATUS) == "running";
                    Child& entry = children_[std::string(*child)];
                    entry.job = job->id;
                    entry.call = node->id;
                    entry.turn = running ? std::optional(turn.id) : std::nullopt;
                }
            }
            for (const Node* decision : dom.children_of_kind(node->id, K::Decision)) index_decision(*decision, turn.id);
            auto results = dom.children_of_kind(node->id, K::Result);
            auto st = node->str_attr(attr::STATUS);
            auto status = st ? loa::core::parse<CallStatus>(*st) : std::nullopt;
            Call call{node->id,
                      inputs.front()->id,
                      inputs.front()->text,
                      results.empty() ? std::nullopt
                                      : std::optional(std::pair{results.front()->id, results.front()->text}),
                      status.value_or(CallStatus::Done),
                      node->str_attr(attr::TITLE).transform([](std::string_view v) { return std::string(v); }),
                      node->int_attr(attr::STARTED),
                      node->int_attr(attr::ENDED),
                      job ? std::optional(job->id) : std::nullopt};
            id = node->str_attr(xattr::PART);
            kind = std::move(call);
        } else if (node->kind == K::Decision) {
            index_decision(*node, turn.id);
            continue;
        } else {
            continue;
        }
        if (id) parts_.insert_or_assign(std::string(*id), Part{turn.id, std::move(message), std::move(oc), std::move(*kind)});
    }
}

void Mapper::index_decision(const Node& node, NodeId turn) {
    auto request_id = node.str_attr(attr::REQUEST_ID);
    if (!request_id) return;
    Pending pending;
    pending.request_id = std::string(*request_id);
    if (node.str_attr(attr::KIND) == "question") {
        pending.question = true;
        const auto index = node.int_attr(xattr::INDEX);
        pending.index = index && *index >= 0 ? static_cast<std::size_t>(*index) : 0;
    }
    if (const CValue* a = node.attr(attr::ANSWER)) {
        std::vector<std::string> values;
        for (auto item : a->str_items()) values.emplace_back(item);
        pending.answer = std::move(values);
    }
    pending.turn = turn;
    pending.waiting = node.str_attr(attr::STATUS) == "waiting";
    decisions_.insert_or_assign(node.id, std::move(pending));
    std::size_t count = 0;
    for (const auto& [_, d] : decisions_)
        if (d.request_id == *request_id) ++count;
    for (auto& [_, d] : decisions_)
        if (d.request_id == *request_id && d.question) d.count = count;
}

loa::contracts::Result<std::vector<Reply>> Mapper::apply(Event ev, const DomWriter& w) {
    if (auto r = apply_event(w, ev); !r) return std::unexpected(std::move(r).error());
    return std::vector<Reply>{};
}

loa::contracts::Result<std::optional<Reply>> Mapper::answer(NodeId decision, const Answer& answer, const DomWriter& w) {
    auto reply = plan_reply(decision, answer);
    if (!reply) return reply;
    if (auto r = record_answer(w, decision, answer); !r) return std::unexpected(std::move(r).error());
    return reply;
}

} // namespace loa::harness::opencode
