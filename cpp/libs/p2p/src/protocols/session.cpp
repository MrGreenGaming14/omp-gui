#include "loa/p2p/protocols/session.hpp"

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <deque>
#include <format>
#include <future>
#include <mutex>
#include <thread>

#include "loa/bus/writer.hpp"
#include "loa/p2p/auth.hpp"
#include "loa/p2p/protocols/projection.hpp"
#include "loa/p2p/sessions.hpp"
#include "loa/runtime/time.hpp"

namespace loa::p2p {

namespace json = runtime::json;
using runtime::time::now_ms;
using trust::Action;

// ---- the serde form of drafts ----

namespace {

// Rust `ElementKind` variant names, in enumerator order.
constexpr std::array<std::string_view, 14> KIND_NAMES = {"Session", "Turn",   "Message", "Call",    "Input",
                                                          "Result",  "Usage",  "Decision", "Job",    "Todo",
                                                          "Surface", "Compaction", "Digest", "Ext"};

json::Value tagged(std::string_view tag, json::Value inner) { return json::Object{{std::string(tag), std::move(inner)}}; }

json::Value value_to_json(const core::Value& value) {
    return std::visit(
        [](const auto& v) -> json::Value {
            using V = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<V, bool>) return tagged("Bool", v);
            else if constexpr (std::is_same_v<V, std::int64_t>) return tagged("Int", v);
            else if constexpr (std::is_same_v<V, double>) return tagged("Float", v);
            else if constexpr (std::is_same_v<V, std::string>) return tagged("Str", v);
            else if constexpr (std::is_same_v<V, core::Value::Bytes>) {
                json::Array bytes;
                for (auto b : v) bytes.emplace_back(std::int64_t{b});
                return tagged("Bytes", std::move(bytes));
            } else {
                json::Array items;
                for (const auto& item : v) items.push_back(value_to_json(item));
                return tagged("List", std::move(items));
            }
        },
        value.variant());
}

Result<> bad(std::string what) { return fail(NodeError::Kind::decode, std::move(what)); }

// The one `{"Tag": inner}` member of an externally tagged enum.
const std::pair<std::string, json::Value>* tag_of(const json::Value& value) {
    const auto* object = value.as_object();
    return object && object->size() == 1 ? &object->front() : nullptr;
}

Result<core::Value> value_from_json(const json::Value& value) {
    const auto* tag = tag_of(value);
    if (!tag) return std::unexpected(bad("expected a tagged value").error());
    const auto& [name, inner] = *tag;
    if (name == "Bool" && inner.as_bool()) return core::Value(*inner.as_bool());
    if (name == "Int" && inner.as_i64()) return core::Value(*inner.as_i64());
    if (name == "Float" && inner.as_f64()) return core::Value(*inner.as_f64());
    if (name == "Str" && inner.as_string()) return core::Value(*inner.as_string());
    if (name == "Bytes" && inner.as_array()) {
        core::Value::Bytes bytes;
        for (const auto& b : *inner.as_array()) {
            auto n = b.as_u64();
            if (!n || *n > 255) return std::unexpected(bad("invalid byte").error());
            bytes.push_back(static_cast<std::uint8_t>(*n));
        }
        return core::Value(std::move(bytes));
    }
    if (name == "List" && inner.as_array()) {
        core::Value::List items;
        for (const auto& item : *inner.as_array()) {
            auto v = value_from_json(item);
            if (!v) return v;
            items.push_back(std::move(*v));
        }
        return core::Value(std::move(items));
    }
    return std::unexpected(bad("unknown variant `" + name + "`").error());
}

Result<core::NodeId> node_field(const json::Value& object, std::string_view field) {
    const auto* v = object.find(field);
    auto n = v ? v->as_u64() : std::nullopt;
    if (!n) return std::unexpected(bad(std::format("missing or invalid field `{}`", field)).error());
    return core::NodeId{*n};
}

Result<std::string> string_field(const json::Value& object, std::string_view field) {
    const auto* v = object.find(field);
    if (!v || !v->as_string()) return std::unexpected(bad(std::format("missing or invalid field `{}`", field)).error());
    return *v->as_string();
}

} // namespace

json::Value draft_to_json(const core::Draft& draft) {
    return std::visit(
        [](const auto& d) -> json::Value {
            using D = std::decay_t<decltype(d)>;
            if constexpr (std::is_same_v<D, core::Draft::Insert>) {
                json::Object attrs;
                for (const auto& [name, value] : d.attrs) attrs.emplace_back(name, value_to_json(value));
                return tagged("Insert", json::Object{
                                            {"parent", d.parent.value},
                                            {"index", d.index ? json::Value(std::uint64_t{*d.index}) : json::Value()},
                                            {"kind", std::string(KIND_NAMES[static_cast<std::size_t>(d.kind)])},
                                            {"attrs", std::move(attrs)},
                                            {"text", d.text},
                                        });
            } else if constexpr (std::is_same_v<D, core::Draft::Set>) {
                return tagged("Set", json::Object{{"node", d.node.value},
                                                  {"name", d.name},
                                                  {"value", d.value ? value_to_json(*d.value) : json::Value()}});
            } else if constexpr (std::is_same_v<D, core::Draft::Append>) {
                return tagged("Append", json::Object{{"node", d.node.value}, {"text", d.text}});
            } else {
                return tagged("Remove", json::Object{{"node", d.node.value}});
            }
        },
        draft.v);
}

Result<core::Draft> draft_from_json(const json::Value& value) {
    const auto* tag = tag_of(value);
    if (!tag || !tag->second.is_object()) return std::unexpected(bad("expected a tagged draft").error());
    const auto& [name, body] = *tag;
    if (name == "Insert") {
        auto parent = node_field(body, "parent");
        if (!parent) return std::unexpected(parent.error());
        core::Draft::Insert insert{*parent, std::nullopt, core::ElementKind::Session, {}, {}};
        if (const auto* index = body.find("index"); index && !index->is_null()) {
            auto n = index->as_u64();
            if (!n || *n > UINT32_MAX) return std::unexpected(bad("invalid field `index`").error());
            insert.index = static_cast<std::uint32_t>(*n);
        }
        auto kind = string_field(body, "kind");
        if (!kind) return std::unexpected(kind.error());
        auto found = std::ranges::find(KIND_NAMES, *kind);
        if (found == KIND_NAMES.end()) return std::unexpected(bad("unknown variant `" + *kind + "`").error());
        insert.kind = static_cast<core::ElementKind>(found - KIND_NAMES.begin());
        const auto* attrs = body.find("attrs");
        if (!attrs || !attrs->as_object()) return std::unexpected(bad("missing or invalid field `attrs`").error());
        for (const auto& [attr, v] : *attrs->as_object()) {
            auto parsed = value_from_json(v);
            if (!parsed) return std::unexpected(parsed.error());
            insert.attrs.insert_or_assign(attr, std::move(*parsed));
        }
        auto text = string_field(body, "text");
        if (!text) return std::unexpected(text.error());
        insert.text = std::move(*text);
        return core::Draft{std::move(insert)};
    }
    auto node = node_field(body, "node");
    if (!node) return std::unexpected(node.error());
    if (name == "Set") {
        auto attr = string_field(body, "name");
        if (!attr) return std::unexpected(attr.error());
        std::optional<core::Value> v;
        if (const auto* raw = body.find("value"); raw && !raw->is_null()) {
            auto parsed = value_from_json(*raw);
            if (!parsed) return std::unexpected(parsed.error());
            v = std::move(*parsed);
        }
        return core::Draft{core::Draft::Set{*node, std::move(*attr), std::move(v)}};
    }
    if (name == "Append") {
        auto text = string_field(body, "text");
        if (!text) return std::unexpected(text.error());
        return core::Draft{core::Draft::Append{*node, std::move(*text)}};
    }
    if (name == "Remove") return core::Draft{core::Draft::Remove{*node}};
    return std::unexpected(bad("unknown variant `" + name + "`").error());
}

std::vector<Action> wire_actions(const std::vector<std::string>& names) {
    std::vector<Action> out;
    for (const auto& name : names)
        if (auto action = trust::parse_action(name)) out.push_back(*action);
    return out;
}

namespace {

std::vector<std::string> action_names(const std::vector<Action>& actions) {
    std::vector<std::string> out;
    for (auto a : actions) out.emplace_back(trust::as_str(a));
    return out;
}

// Named apart from `wire::send`, which ADL would pick for a bare message.
Result<> send_body(SendHalf& tx, wire::Body body) { return wire::send(tx, wire::Frame{std::move(body)}); }

wire::Body refused(std::string error) { return wire::Ack{std::move(error)}; }

wire::Body ack(const Result<>& result) { return refused(result ? std::string() : result.error().to_string()); }

std::string trimmed(std::string_view s) {
    constexpr std::string_view ws = " \t\n\r\f\v";
    const auto first = s.find_first_not_of(ws);
    return first == std::string_view::npos ? std::string() : std::string(s.substr(first, s.find_last_not_of(ws) - first + 1));
}

Result<> prompted(NodeHost& host, const NodeId& peer, wire::Prompt prompt) {
    auto writer = host.local_sessions().writer(prompt.session);
    if (!writer) return fail(NodeError::Kind::refused, "no such session");
    auto& access = *host.grants().access;
    const auto key = trust::to_bus(peer);
    if (!writer->with_dom([&](const core::Dom& dom) { return access.allows_in(key, dom, Action::drive, std::nullopt); }))
        return fail(NodeError::Kind::refused, "the grant does not allow driving this session");
    if (trimmed(prompt.text).empty()) return fail(NodeError::Kind::refused, "empty prompt");
    host.prompt_requested({peer, std::move(prompt.session), std::move(prompt.text)});
    return {};
}

// Checks a peer's answer to a waiting decision and hands it to the app.
Result<> answered(NodeHost& host, const NodeId& peer, const std::string& session, const Projection& projection,
                  const bus::SessionWriter& writer, wire::Answer answer) {
    if (answer.session != session) return fail(NodeError::Kind::refused, "an answer on this stream must name its session");
    auto decision = projection.source_node(core::NodeId{answer.decision});
    if (!decision) return fail(NodeError::Kind::refused, "the decision is outside the grant");
    auto& access = *host.grants().access;
    const auto key = trust::to_bus(peer);
    auto [allowed, waiting] = writer.with_dom([&](const core::Dom& dom) {
        const auto* node = dom.get(*decision);
        const bool waiting = node && node->kind == core::ElementKind::Decision &&
                             node->str_attr(core::attr::STATUS) == core::as_str(core::DecisionStatus::Waiting);
        return std::pair{access.allows_in(key, dom, Action::drive, *decision), waiting};
    });
    if (!allowed) return fail(NodeError::Kind::refused, "the grant does not allow answering this decision");
    if (!waiting) return fail(NodeError::Kind::refused, "the decision is no longer waiting");
    host.answer_requested({peer, session, *decision, std::move(answer.values), std::move(answer.reason)});
    return {};
}

wire::DriveResult driven(bus::SessionWriter& writer, const Projection& projection, const Access& access,
                         const bus::NodeKey& peer, const wire::Drive& drive) {
    auto failed = [](std::string error) { return wire::DriveResult{std::nullopt, std::nullopt, std::move(error)}; };
    auto parsed = json::parse(std::string_view(reinterpret_cast<const char*>(drive.draft.data()), drive.draft.size()));
    if (!parsed) return failed(parsed.error().message());
    auto draft = draft_from_json(*parsed);
    if (!draft) return failed(draft.error().message());
    auto source = projection.source_draft(std::move(*draft));
    if (!source) return failed("the change names a node outside the grant");
    if (!writer.with_dom([&](const core::Dom& dom) { return access.allows(peer, dom, Action::drive, target(*source)); }))
        return failed("no drive grant on " + writer.session().str());
    auto patch = writer.request(peer, drive.base_seq, std::move(*source), now_ms());
    if (patch) return {wire::to_wire(*patch), std::nullopt, {}};
    if (const auto* conflict = std::get_if<bus::request_error::Conflict>(&patch.error()))
        return {std::nullopt, conflict->actual, "the session moved on"};
    return failed(bus::to_string(patch.error()));
}

// Requests and bus events of one subscription, in arrival order.
struct Inbox {
    std::mutex mutex;
    std::condition_variable changed;
    std::deque<wire::Body> requests;
    bool requests_ended = false;
};

// Streams the requester's projection of a session and applies its requests
// in order, until either side ends or its grants do.
Result<> subscription(NodeHost& host, const NodeId& peer, const wire::Subscribe& subscribe, Stream stream) {
    auto& tx = *stream.send;
    auto access = host.grants().access;
    const auto key = trust::to_bus(peer);
    auto writer = host.local_sessions().writer(subscribe.session);
    if (!writer || !writer->is_writer()) return send_body(tx, refused("no such session"));
    const bool readable = writer->with_dom([&](const core::Dom& dom) {
        if (access->allows_in(key, dom, Action::view, std::nullopt)) return true;
        for (const auto& n : dom.iter())
            if (n.kind == core::ElementKind::Turn && access->allows_in(key, dom, Action::view, n.id)) return true;
        return false;
    });
    if (!readable) return send_body(tx, refused("no view grant on " + subscribe.session));

    auto [dom, events] = writer->subscribe(bus::Policy::every_patch(SUBSCRIBER_QUEUE));
    std::vector<Action> actions;
    if (auto facts = trust::SessionFacts::from_dom(dom)) actions = access->session_actions(key, *facts);
    auto seeded = Projection::seeded(key, dom, *access);
    if (!seeded) return std::unexpected(seeded.error());
    auto& [projection, seed] = *seeded;
    wire::Seed message{dom.seq(), {}, action_names(actions)};
    for (const auto& patch : seed) message.patches.push_back(wire::to_wire(patch));
    if (auto r = send_body(tx, std::move(message)); !r) return r;
    dom = core::Dom();

    // Frames are read on their own thread: a blocked read can't be interrupted.
    auto inbox = std::make_shared<Inbox>();
    std::thread([inbox, rx = std::shared_ptr<RecvHalf>(std::move(stream.recv))] {
        while (true) {
            auto frame = wire::recv<wire::Frame>(*rx);
            if (!frame || !*frame || !(*frame)->body) break;
            std::scoped_lock lock(inbox->mutex);
            inbox->requests.push_back(std::move(*(*frame)->body));
            inbox->changed.notify_all();
        }
        std::scoped_lock lock(inbox->mutex);
        inbox->requests_ended = true;
        inbox->changed.notify_all();
    }).detach();

    std::optional<std::string> ended;
    while (true) {
        std::optional<wire::Body> answer;
        if (auto event = events.try_recv()) {
            if (auto* patch = std::get_if<bus::event::Patch>(&*event)) {
                if (access->live_grants(key, now_ms()).empty()) {
                    ended = "the grant ended";
                    break;
                }
                auto update = projection.accept(*patch->patch, *access);
                if (!update) return std::unexpected(update.error());
                answer = wire::WireUpdate{update->source_seq,
                                          update->patch ? std::optional(wire::to_wire(*update->patch)) : std::nullopt};
            } else if (std::holds_alternative<bus::event::Lagged>(*event)) {
                ended = "fell behind; open the session again";
                break;
            } else {
                ended = "every_patch subscribers get single patches";
                break;
            }
        } else if (event.error() == bus::TryRecvError::Closed) {
            ended = "the session closed";
            break;
        } else {
            std::unique_lock lock(inbox->mutex);
            if (inbox->requests.empty() && !inbox->requests_ended) {
                // Bus events have no waker here; look again soon.
                inbox->changed.wait_for(lock, std::chrono::milliseconds(5));
                continue;
            }
            if (inbox->requests.empty()) break;
            auto request = std::move(inbox->requests.front());
            inbox->requests.pop_front();
            lock.unlock();
            if (auto* drive = std::get_if<wire::Drive>(&request))
                answer = driven(*writer, projection, *access, key, *drive);
            else if (auto* prompt = std::get_if<wire::Prompt>(&request))
                answer = prompt->session != subscribe.session
                             ? refused("a prompt on this stream must name its session")
                             : ack(prompted(host, peer, std::move(*prompt)));
            else if (auto* reply = std::get_if<wire::Answer>(&request))
                answer = ack(answered(host, peer, subscribe.session, projection, *writer, std::move(*reply)));
            else answer = refused("not a request on a subscription");
        }
        if (auto r = send_body(tx, std::move(*answer)); !r) return r;
    }
    if (ended) (void)send_body(tx, refused(*ended));
    (void)tx.shutdown();
    return {};
}

// The requester side: a stream with `request` sent and grants presented.
Result<std::pair<Connection, Stream>> request(NodeHost& host, const NodeId& peer, wire::Body body) {
    auto conn = host.dial(peer, wire::SESSION);
    if (!conn) return std::unexpected(conn.error());
    auto stream = (*conn)->open_stream();
    if (!stream) return std::unexpected(stream.error());
    if (auto r = send_body(*stream->send, std::move(body)); !r) return std::unexpected(r.error());
    if (auto r = host.grants().present(peer, *stream); !r) return std::unexpected(r.error());
    return std::pair{std::move(*conn), std::move(*stream)};
}

// One request and its answer on a fresh stream.
Result<std::optional<wire::Body>> once(NodeHost& host, const NodeId& peer, wire::Body body) {
    auto opened = request(host, peer, std::move(body));
    if (!opened) return std::unexpected(opened.error());
    auto& [conn, stream] = *opened;
    auto answer = wire::expect<wire::Frame>(*stream.recv);
    conn->close(0, "done");
    if (!answer) return std::unexpected(answer.error());
    return std::move(answer->body);
}

// Keeps the mirror's own patches, so a fork can copy them.
class Recorder final : public bus::Persist {
  public:
    Recorder(std::shared_ptr<std::mutex> mutex, std::shared_ptr<std::vector<core::Patch>> patches)
        : mutex_(std::move(mutex)), patches_(std::move(patches)) {}
    std::expected<void, bus::PersistError> append(const core::Patch& patch) override {
        std::scoped_lock lock(*mutex_);
        patches_->push_back(patch);
        return {};
    }

  private:
    std::shared_ptr<std::mutex> mutex_;
    std::shared_ptr<std::vector<core::Patch>> patches_;
};

} // namespace

// ---- RemoteView ----

struct RemoteView::Shared {
    std::shared_ptr<std::mutex> patches_mutex = std::make_shared<std::mutex>();
    std::shared_ptr<std::vector<core::Patch>> patches = std::make_shared<std::vector<core::Patch>>();
    std::atomic<std::uint64_t> source_seq{0};
    // Serializes requests so answers come back in order.
    std::mutex send_mutex;
    std::unique_ptr<SendHalf> send;
    // Guards `pending` and `ended`.
    std::mutex mutex;
    std::deque<std::shared_ptr<std::promise<Result<wire::Body>>>> pending;
    std::optional<std::string> ended;
};

namespace {

void pump(std::shared_ptr<RemoteView::Shared> shared, std::unique_ptr<RecvHalf> rx, Mirror mirror) {
    std::string reason;
    while (true) {
        auto frame = wire::recv<wire::Frame>(*rx);
        if (!frame) {
            reason = frame.error().to_string();
            break;
        }
        if (!*frame || !(*frame)->body) {
            reason = "the owner closed the session";
            break;
        }
        auto body = std::move(*(*frame)->body);
        if (auto* update = std::get_if<wire::WireUpdate>(&body)) {
            std::vector<wire::WirePatch> wire_patches;
            if (update->patch) wire_patches.push_back(std::move(*update->patch));
            auto decoded = wire::patches(wire_patches);
            Result<> applied = decoded ? Result<>() : Result<>(std::unexpected(decoded.error()));
            if (decoded) {
                std::optional<core::Patch> patch;
                if (!decoded->empty()) patch = std::move(decoded->front());
                applied = mirror.receive(Update{update->source_seq, std::move(patch)});
            }
            if (!applied) {
                reason = applied.error().to_string();
                break;
            }
            shared->source_seq.store(mirror.source_seq(), std::memory_order_release);
            continue;
        }
        auto answer = wire::into_ack(wire::Frame{std::move(body)});
        std::shared_ptr<std::promise<Result<wire::Body>>> waiter;
        {
            std::scoped_lock lock(shared->mutex);
            if (!shared->pending.empty()) {
                waiter = std::move(shared->pending.front());
                shared->pending.pop_front();
            }
        }
        if (waiter) {
            if (answer) waiter->set_value(std::move(*answer->body));
            else waiter->set_value(std::unexpected(answer.error()));
        } else if (!answer) {
            reason = answer.error().to_string();
            break;
        } else {
            reason = "unexpected answer";
            break;
        }
    }
    {
        std::scoped_lock lock(shared->mutex);
        shared->ended = reason;
        for (auto& waiter : shared->pending) waiter->set_value(fail(NodeError::Kind::refused, reason));
        shared->pending.clear();
    }
    mirror.writer()->close();
}

} // namespace

RemoteView::RemoteView(NodeId peer, std::string session, std::vector<Action> actions,
                       std::shared_ptr<bus::SessionWriter> writer, std::shared_ptr<Shared> shared, Connection conn)
    : peer_(peer), session_(std::move(session)), actions_(std::move(actions)), writer_(std::move(writer)),
      shared_(std::move(shared)), conn_(std::move(conn)) {}

RemoteView::~RemoteView() {
    // The pump ends once the closed connection fails its read.
    conn_->close(0, "closed");
}

bool RemoteView::can(Action action) const { return std::ranges::find(actions_, action) != actions_.end(); }

std::uint64_t RemoteView::source_seq() const { return shared_->source_seq.load(std::memory_order_acquire); }

std::optional<std::string> RemoteView::ended() const {
    std::scoped_lock lock(shared_->mutex);
    return shared_->ended;
}

std::vector<core::Patch> RemoteView::patches() const {
    std::scoped_lock lock(*shared_->patches_mutex);
    return *shared_->patches;
}

Result<wire::Body> RemoteView::call(wire::Body body) {
    auto waiter = std::make_shared<std::promise<Result<wire::Body>>>();
    auto answer = waiter->get_future();
    {
        std::scoped_lock send_lock(shared_->send_mutex);
        {
            std::scoped_lock lock(shared_->mutex);
            if (shared_->ended) return fail(NodeError::Kind::refused, *shared_->ended);
            shared_->pending.push_back(waiter);
        }
        if (auto r = send_body(*shared_->send, std::move(body)); !r) return std::unexpected(r.error());
    }
    return answer.get();
}

Result<DriveOutcome> RemoteView::drive(core::Draft draft) { return drive_at(source_seq(), std::move(draft)); }

Result<DriveOutcome> RemoteView::drive_at(std::uint64_t base_seq, core::Draft draft) {
    const auto text = json::dump(draft_to_json(draft));
    auto answer = call(wire::Drive{session_, base_seq, Bytes(text.begin(), text.end()), now_ms()});
    if (!answer) return std::unexpected(answer.error());
    auto* result = std::get_if<wire::DriveResult>(&*answer);
    if (!result) return fail(NodeError::Kind::protocol, "expected a drive result");
    if (result->conflict_current) return drive_outcome::Conflict{*result->conflict_current};
    if (result->patch) {
        auto patch = wire::patches(std::span(&*result->patch, 1));
        if (!patch) return std::unexpected(patch.error());
        return drive_outcome::Applied{patch->front().seq};
    }
    return fail(NodeError::Kind::refused, result->error);
}

Result<> RemoteView::prompt(std::string_view text) {
    auto answer = call(wire::Prompt{session_, std::string(text)});
    if (!answer) return std::unexpected(answer.error());
    return {};
}

Result<> RemoteView::answer(core::NodeId decision, std::vector<std::string> values, std::optional<std::string> reason) {
    auto answer = call(wire::Answer{session_, decision.value, std::move(values), std::move(reason)});
    if (!answer) return std::unexpected(answer.error());
    return {};
}

Result<std::unique_ptr<RemoteView>> open_remote_view(NodeHost& host, const NodeId& peer, std::string_view session) {
    auto opened = request(host, peer, wire::Subscribe{std::string(session), 0});
    if (!opened) return std::unexpected(opened.error());
    auto& [conn, stream] = *opened;
    auto first = wire::expect<wire::Frame>(*stream.recv);
    if (!first) return std::unexpected(first.error());
    auto* seed = first->body ? std::get_if<wire::Seed>(&*first->body) : nullptr;
    if (!seed) return fail(NodeError::Kind::protocol, "expected the session seed");
    auto shared = std::make_shared<RemoteView::Shared>();
    bus::WriterConfig config{bus::SessionId(session), trust::to_bus(peer), trust::to_bus(host.key()), core::Dom(),
                             std::make_unique<Recorder>(shared->patches_mutex, shared->patches),
                             std::make_shared<Refuse>()};
    auto writer = std::make_shared<bus::SessionWriter>(std::move(config));
    auto patches = wire::patches(seed->patches);
    if (!patches) return std::unexpected(patches.error());
    auto mirror = Mirror::seeded(writer, seed->source_seq, std::move(*patches));
    if (!mirror) return std::unexpected(mirror.error());
    shared->source_seq.store(seed->source_seq);
    shared->send = std::move(stream.send);
    auto actions = wire_actions(seed->actions);
    std::thread(pump, shared, std::move(stream.recv), std::move(*mirror)).detach();
    return std::unique_ptr<RemoteView>(
        new RemoteView(peer, std::string(session), std::move(actions), std::move(writer), std::move(shared), std::move(conn)));
}

// ---- protocol ----

namespace session {

std::vector<wire::SessionEntry> visible(NodeHost& host, const NodeId& peer) {
    auto& access = *host.grants().access;
    const auto key = trust::to_bus(peer);
    std::vector<wire::SessionEntry> out;
    for (auto& s : host.local_sessions().sessions()) {
        auto actions = access.session_actions(key, s.facts);
        const bool partial = actions.empty() && std::ranges::any_of(s.turns, [&](const LocalTurn& t) {
                                 return access.turn_visible(key, s.facts, t.id);
                             });
        if (actions.empty() && !partial) continue;
        const bool view = std::ranges::find(actions, Action::view) != actions.end();
        out.push_back(wire::SessionEntry{.id = s.facts.id,
                                         .title = view ? s.label() : std::string(),
                                         .harness = s.harness,
                                         .created_ms = s.facts.created_ms,
                                         .actions = action_names(actions),
                                         .partial = partial,
                                         .last_seq = s.last_seq});
    }
    return out;
}

Result<> serve(NodeHost& host, const Connection& conn, Stream stream) {
    const auto peer = conn->remote();
    auto first = wire::expect<wire::Frame>(*stream.recv);
    if (!first) return std::unexpected(first.error());
    if (auto r = host.grants().challenge(peer, stream); !r) return r;
    auto& tx = *stream.send;
    if (!first->body) return send_body(tx, refused("not a request"));
    auto& body = *first->body;
    if (std::holds_alternative<wire::ListSessions>(body)) return send_body(tx, wire::SessionList{visible(host, peer)});
    if (auto* prompt = std::get_if<wire::Prompt>(&body)) return send_body(tx, ack(prompted(host, peer, std::move(*prompt))));
    if (auto* subscribe = std::get_if<wire::Subscribe>(&body)) {
        auto request = std::move(*subscribe);
        return subscription(host, peer, request, std::move(stream));
    }
    return send_body(tx, refused("not a request"));
}

Result<std::vector<RemoteSession>> list(NodeHost& host, const NodeId& peer) {
    auto answer = once(host, peer, wire::ListSessions{});
    if (!answer) return std::unexpected(answer.error());
    auto* sessions = *answer ? std::get_if<wire::SessionList>(&**answer) : nullptr;
    if (!sessions) return fail(NodeError::Kind::protocol, "expected the session list");
    std::vector<RemoteSession> out;
    for (auto& entry : sessions->sessions)
        out.push_back(RemoteSession{.peer = peer,
                                    .id = std::move(entry.id),
                                    .title = std::move(entry.title),
                                    .harness = std::move(entry.harness),
                                    .created_ms = entry.created_ms,
                                    .actions = wire_actions(entry.actions),
                                    .partial = entry.partial,
                                    .last_seq = entry.last_seq});
    return out;
}

Result<> prompt(NodeHost& host, const NodeId& peer, std::string_view session, std::string_view text) {
    auto answer = once(host, peer, wire::Prompt{std::string(session), std::string(text)});
    if (!answer) return std::unexpected(answer.error());
    return {};
}

} // namespace session
} // namespace loa::p2p
