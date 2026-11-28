#include "connect.hpp"

#include <format>

namespace loa::mcp::detail {

namespace json = loa::runtime::json;

namespace {

constexpr std::string_view CLOSED = "Transport closed";

// `Duration`'s `Debug` form: `30s`, `10ms`, `1.5s`.
std::string duration_text(std::chrono::milliseconds d) {
    const auto ms = d.count();
    if (ms % 1000 == 0) return std::format("{}s", ms / 1000);
    if (ms < 1000) return std::format("{}ms", ms);
    std::string frac = std::format("{:03}", ms % 1000);
    while (!frac.empty() && frac.back() == '0') frac.pop_back();
    return std::format("{}.{}s", ms / 1000, frac);
}

std::string rpc_error_text(const json::Value& error) {
    std::string code = "?";
    if (const auto* c = error.find("code")) {
        if (auto v = c->as_i64()) code = std::to_string(*v);
    }
    std::string message;
    if (const auto* m = error.find("message"); m && m->as_string()) message = *m->as_string();
    return "Mcp error: " + code + ": " + message;
}

} // namespace

std::optional<std::string> last_nonempty_line(std::string_view text) {
    while (!text.empty()) {
        auto end = text.size();
        auto start = text.rfind('\n', end - 1);
        auto line = start == std::string_view::npos ? text : text.substr(start + 1);
        if (!line.empty() && line.back() == '\r') line.remove_suffix(1);
        if (line.find_first_not_of(" \t\r\f\v") != std::string_view::npos) return std::string(line);
        if (start == std::string_view::npos) break;
        text = text.substr(0, start);
    }
    return std::nullopt;
}

Result<std::shared_ptr<Connection>> Connection::open(const ServerSpec& spec, std::chrono::milliseconds timeout, std::stop_token stop) {
    auto failed = [&](std::string message) { return std::unexpected(Error::connect(spec.name, std::move(message))); };
    std::shared_ptr<Connection> connection(new Connection());
    Connection* raw = connection.get();
    Sink sink{[raw](std::string_view text) { raw->deliver(text); }, [raw] { raw->on_closed(); }};
    if (const auto* stdio = std::get_if<StdioSpec>(&spec.transport)) {
        auto transport = spawn_stdio(*stdio, std::move(sink));
        if (!transport) return failed("can't run `" + stdio->command + "`: " + transport.error());
        connection->transport_ = std::move(*transport);
    } else {
        connection->transport_ = open_http(std::get<HttpSpec>(spec.transport), std::move(sink));
    }

    const Cancel cancel{stop, Clock::now() + timeout};
    json::Object client_info{{"name", "loa"}, {"version", "0.1.0"}};
    json::Object params{{"protocolVersion", std::string(PROTOCOL_VERSION)}, {"capabilities", json::Object{}}, {"clientInfo", std::move(client_info)}};
    auto result = connection->request("initialize", std::move(params), cancel);
    if (!result) {
        const bool timed_out = !stop.stop_requested() && cancel.expired();
        std::string message = timed_out ? "no handshake within " + duration_text(timeout) : result.error();
        if (!timed_out) {
            if (auto last = connection->last_line()) message += ": " + *last;
        }
        connection->close();
        return failed(std::move(message));
    }
    if (const auto* caps = result->find("capabilities")) connection->capabilities_ = *caps;
    std::string version(PROTOCOL_VERSION);
    if (const auto* v = result->find("protocolVersion"); v && v->as_string()) version = *v->as_string();
    connection->transport_->initialized(version);
    connection->notify("notifications/initialized");
    return connection;
}

Connection::~Connection() { close(); }

void Connection::close() {
    if (transport_) transport_->close();
    on_closed();
}

bool Connection::closed() const {
    std::lock_guard lock(mutex_);
    return closed_;
}

std::optional<std::string> Connection::last_line() { return transport_ ? transport_->last_line() : std::nullopt; }

void Connection::on_closed() {
    std::lock_guard lock(mutex_);
    closed_ = true;
    for (auto& [id, slot] : pending_) {
        if (!slot->done) {
            slot->result = std::unexpected(std::string(CLOSED));
            slot->done = true;
        }
    }
    changed_.notify_all();
}

std::expected<json::Value, std::string> Connection::request(std::string_view method, json::Value params, const Cancel& cancel) {
    auto slot = std::make_shared<Pending>();
    std::int64_t id = 0;
    {
        std::lock_guard lock(mutex_);
        if (closed_) return std::unexpected(std::string(CLOSED));
        id = next_id_++;
        pending_.emplace(id, slot);
    }
    json::Object message{{"jsonrpc", "2.0"}, {"id", id}, {"method", std::string(method)}, {"params", std::move(params)}};
    auto sent = transport_->send(json::dump(json::Value(std::move(message))), cancel);
    std::unique_lock lock(mutex_);
    auto finish = [&](std::string error) -> std::expected<json::Value, std::string> {
        pending_.erase(id);
        return std::unexpected(std::move(error));
    };
    if (slot->done) {
        pending_.erase(id);
        return std::move(slot->result);
    }
    if (!sent) return finish(sent.error());
    if (transport_->synchronous()) return finish("the server sent no response");
    auto ready = [&] { return slot->done; };
    if (cancel.deadline) changed_.wait_until(lock, cancel.stop, *cancel.deadline, ready);
    else changed_.wait(lock, cancel.stop, ready);
    if (slot->done) {
        pending_.erase(id);
        return std::move(slot->result);
    }
    return finish(cancel.stop.stop_requested() ? "cancelled" : "timed out");
}

void Connection::notify(std::string_view method, std::optional<json::Value> params) {
    json::Object message{{"jsonrpc", "2.0"}, {"method", std::string(method)}};
    if (params) message.emplace_back("params", std::move(*params));
    (void)transport_->send(json::dump(json::Value(std::move(message))), {});
}

void Connection::deliver(std::string_view text) {
    auto parsed = json::parse(text);
    if (!parsed) return;
    if (const auto* batch = parsed->as_array()) {
        for (const auto& item : *batch) dispatch(item);
    } else {
        dispatch(*parsed);
    }
}

void Connection::dispatch(const json::Value& message) {
    if (!message.is_object()) return;
    const auto* method = message.find("method");
    const auto* id = message.find("id");
    if (method == nullptr) {
        if (id == nullptr) return;
        auto key = id->as_i64();
        if (!key) return;
        std::lock_guard lock(mutex_);
        auto it = pending_.find(*key);
        if (it == pending_.end() || it->second->done) return;
        if (const auto* error = message.find("error")) {
            it->second->result = std::unexpected(rpc_error_text(*error));
        } else if (const auto* result = message.find("result")) {
            it->second->result = *result;
        } else {
            it->second->result = json::Value();
        }
        it->second->done = true;
        changed_.notify_all();
        return;
    }
    const auto* name = method->as_string();
    if (name == nullptr) return;
    if (id != nullptr) {
        // A request from the server: answer pings, refuse the rest.
        json::Object reply{{"jsonrpc", "2.0"}, {"id", *id}};
        if (*name == "ping") reply.emplace_back("result", json::Object{});
        else reply.emplace_back("error", json::Object{{"code", -32601}, {"message", "Method not found"}});
        (void)transport_->send(json::dump(json::Value(std::move(reply))), {});
        return;
    }
    std::lock_guard lock(mutex_);
    notifications_.push_back(*name);
    changed_.notify_all();
}

Connection::Wake Connection::wait(std::stop_token stop, std::string& method) {
    std::unique_lock lock(mutex_);
    changed_.wait(lock, stop, [&] { return closed_ || !notifications_.empty(); });
    if (closed_) return Wake::Closed;
    if (!notifications_.empty()) {
        method = std::move(notifications_.front());
        notifications_.pop_front();
        return Wake::Notification;
    }
    return Wake::Stopped;
}

std::expected<std::vector<model::Tool>, std::string> Connection::list_all_tools(const Cancel& cancel) {
    std::vector<model::Tool> tools;
    std::optional<std::string> cursor;
    do {
        json::Object params;
        if (cursor) params.emplace_back("cursor", *cursor);
        auto result = request("tools/list", std::move(params), cancel);
        if (!result) return std::unexpected(result.error());
        if (const auto* list = result->find("tools"); list && list->as_array()) {
            for (const auto& item : *list->as_array()) {
                auto tool = model::Tool::from_json(item);
                if (!tool) return std::unexpected(tool.error());
                tools.push_back(std::move(*tool));
            }
        }
        cursor.reset();
        if (const auto* next = result->find("nextCursor"); next && next->as_string()) cursor = *next->as_string();
    } while (cursor);
    return tools;
}

std::expected<std::vector<model::Resource>, std::string> Connection::list_all_resources() {
    std::vector<model::Resource> resources;
    std::optional<std::string> cursor;
    do {
        json::Object params;
        if (cursor) params.emplace_back("cursor", *cursor);
        auto result = request("resources/list", std::move(params));
        if (!result) return std::unexpected(result.error());
        if (const auto* list = result->find("resources"); list && list->as_array()) {
            for (const auto& item : *list->as_array()) {
                auto resource = model::Resource::from_json(item);
                if (!resource) return std::unexpected(resource.error());
                resources.push_back(std::move(*resource));
            }
        }
        cursor.reset();
        if (const auto* next = result->find("nextCursor"); next && next->as_string()) cursor = *next->as_string();
    } while (cursor);
    return resources;
}

std::expected<model::CallToolResult, std::string> Connection::call_tool(std::string_view name, std::optional<model::JsonObject> arguments) {
    json::Object params{{"name", std::string(name)}};
    if (arguments) params.emplace_back("arguments", std::move(*arguments));
    auto result = request("tools/call", std::move(params));
    if (!result) return std::unexpected(result.error());
    return model::CallToolResult::from_json(*result);
}

std::expected<std::vector<model::ResourceContents>, std::string> Connection::read_resource(std::string_view uri) {
    auto result = request("resources/read", json::Object{{"uri", std::string(uri)}});
    if (!result) return std::unexpected(result.error());
    std::vector<model::ResourceContents> out;
    if (const auto* contents = result->find("contents"); contents && contents->as_array()) {
        for (const auto& item : *contents->as_array()) {
            auto parsed = model::resource_contents_from_json(item);
            if (!parsed) return std::unexpected(parsed.error());
            out.push_back(std::move(*parsed));
        }
    }
    return out;
}

} // namespace loa::mcp::detail
