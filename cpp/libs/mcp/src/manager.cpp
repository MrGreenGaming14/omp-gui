#include "loa/mcp/manager.hpp"

#include <algorithm>
#include <condition_variable>
#include <deque>
#include <format>
#include <map>
#include <mutex>
#include <thread>

#include "connect.hpp"
#include "loa/runtime/log.hpp"

namespace loa::mcp {

using detail::Connection;

std::chrono::milliseconds Backoff::delay(std::uint32_t attempt) const {
    const auto factor = std::int64_t{1} << std::min<std::uint32_t>(attempt, 16);
    const auto ms = initial.count();
    // Saturating multiply, then the cap.
    if (ms > 0 && factor > INT64_MAX / ms) return max;
    return std::min(std::chrono::milliseconds(ms * factor), max);
}

std::optional<contracts::surface::SurfaceUri> surface_uri(const model::Resource& resource) {
    auto uri = contracts::surface::SurfaceUri::parse(resource.uri);
    if (!uri) return std::nullopt;
    return std::move(*uri);
}

// ---- events and status watches ----

struct EventReceiver::State {
    static constexpr std::size_t CAPACITY = 256;
    std::mutex mutex;
    std::condition_variable cv;
    std::deque<Event> queue;
};

std::optional<Event> EventReceiver::recv(std::chrono::milliseconds timeout) {
    std::unique_lock lock(state_->mutex);
    if (!state_->cv.wait_for(lock, timeout, [&] { return !state_->queue.empty(); })) return std::nullopt;
    Event event = std::move(state_->queue.front());
    state_->queue.pop_front();
    return event;
}

namespace {

class Broadcast {
  public:
    EventReceiver subscribe() {
        auto state = std::make_shared<EventReceiver::State>();
        std::lock_guard lock(mutex_);
        receivers_.push_back(state);
        return EventReceiver(state);
    }

    void send(const Event& event) {
        std::lock_guard lock(mutex_);
        std::erase_if(receivers_, [&](const std::weak_ptr<EventReceiver::State>& weak) {
            auto state = weak.lock();
            if (!state) return true;
            std::lock_guard inner(state->mutex);
            if (state->queue.size() >= EventReceiver::State::CAPACITY) state->queue.pop_front();
            state->queue.push_back(event);
            state->cv.notify_all();
            return false;
        });
    }

  private:
    std::mutex mutex_;
    std::vector<std::weak_ptr<EventReceiver::State>> receivers_;
};

} // namespace

struct StatusWatch::State {
    mutable std::mutex mutex;
    mutable std::condition_variable cv;
    Status status = Status::stopped();
};

Status StatusWatch::current() const {
    std::lock_guard lock(state_->mutex);
    return state_->status;
}

std::optional<Status> StatusWatch::wait_impl(const std::function<bool(const Status&)>& wanted, std::chrono::milliseconds timeout) const {
    std::unique_lock lock(state_->mutex);
    if (!state_->cv.wait_for(lock, timeout, [&] { return wanted(state_->status); })) return std::nullopt;
    return state_->status;
}

// ---- servers ----

namespace {

// State a supervisor publishes and calls read.
struct Shared {
    std::string name;
    std::shared_ptr<StatusWatch::State> status = std::make_shared<StatusWatch::State>();
    std::shared_ptr<Broadcast> events;
    std::mutex mutex;
    std::shared_ptr<Connection> peer;
    std::vector<model::Tool> tools;

    void set_status(Status next) {
        {
            std::lock_guard lock(status->mutex);
            status->status = next;
            status->cv.notify_all();
        }
        events->send(event::Status{name, std::move(next)});
    }

    Result<std::shared_ptr<Connection>> connection() {
        std::lock_guard lock(mutex);
        if (!peer) return std::unexpected(Error::not_running(name));
        return peer;
    }
};

// Keeps one server connected until stopped.
struct Supervisor {
    ServerSpec spec;
    std::shared_ptr<Shared> shared;
    Options options;

    void run(std::stop_token stop) {
        std::uint32_t attempt = 0;
        for (;;) {
            shared->set_status(Status::starting());
            auto connected = Connection::open(spec, options.connect_timeout, stop);
            if (stop.stop_requested()) {
                if (connected) (*connected)->close();
                break;
            }
            std::string failure;
            if (connected) {
                const auto started = detail::Clock::now();
                auto message = serve(*connected, stop);
                if (!message) break;
                if (detail::Clock::now() - started >= options.backoff.max) attempt = 0;
                failure = std::move(*message);
            } else {
                failure = connected.error().to_string();
            }
            shared->set_status(Status::error(std::move(failure)));
            const auto delay = options.backoff.delay(attempt);
            if (attempt != UINT32_MAX) ++attempt;
            runtime::log::info("loa_mcp", std::format("{}: restarting in {}ms", spec.name, delay.count()));
            std::mutex m;
            std::unique_lock lock(m);
            std::condition_variable_any().wait_for(lock, stop, delay, [] { return false; });
            if (stop.stop_requested()) break;
        }
        shared->set_status(Status::stopped());
    }

    // Runs one connection until it ends: none when stopped, else why it failed.
    std::optional<std::string> serve(const std::shared_ptr<Connection>& connection, std::stop_token stop) {
        auto listed = connection->list_all_tools(detail::Cancel{stop, std::nullopt});
        if (stop.stop_requested()) {
            connection->close();
            return std::nullopt;
        }
        if (!listed) {
            connection->close();
            return "listing tools: " + listed.error();
        }
        {
            std::lock_guard lock(shared->mutex);
            shared->tools = std::move(*listed);
            shared->peer = connection;
        }
        shared->set_status(Status::running());

        bool stopped = false;
        for (;;) {
            std::string method;
            const auto wake = connection->wait(stop, method);
            if (wake == Connection::Wake::Closed) break;
            if (wake == Connection::Wake::Stopped) {
                connection->close();
                stopped = true;
                break;
            }
            if (method == "notifications/tools/list_changed") {
                auto tools = connection->list_all_tools(detail::Cancel{stop, std::nullopt});
                if (tools) {
                    {
                        std::lock_guard lock(shared->mutex);
                        shared->tools = std::move(*tools);
                    }
                    shared->events->send(event::ToolsChanged{shared->name});
                } else {
                    runtime::log::warn("loa_mcp", spec.name + ": relisting tools: " + tools.error());
                }
            } else if (method == "notifications/resources/list_changed") {
                shared->events->send(event::ResourcesChanged{shared->name});
            }
        }
        {
            std::lock_guard lock(shared->mutex);
            shared->peer.reset();
            shared->tools.clear();
        }
        if (stopped) return std::nullopt;
        auto last = connection->last_line();
        connection->close();
        if (last) return "the server exited: " + *last;
        return std::string("the server closed the connection");
    }
};

struct Run {
    std::jthread thread;
    std::shared_ptr<std::atomic<bool>> finished = std::make_shared<std::atomic<bool>>(false);
};

struct Entry {
    ServerSpec spec;
    std::shared_ptr<Shared> shared;
    // The supervisor keeping the server connected.
    std::optional<Run> run;
};

void finish(std::optional<Run> run) {
    if (!run) return;
    run->thread.request_stop();
    if (run->thread.joinable()) run->thread.join();
}

Error unknown(std::string_view name) { return Error::unknown_server(std::string(name)); }

} // namespace

struct Manager::Inner {
    std::mutex mutex;
    std::map<std::string, Entry, std::less<>> servers;
    std::shared_ptr<Broadcast> events = std::make_shared<Broadcast>();
    Options options;

    Run spawn(const Entry& entry) const {
        Run run;
        Supervisor supervisor{entry.spec, entry.shared, options};
        run.thread = std::jthread([supervisor = std::move(supervisor), finished = run.finished](std::stop_token stop) mutable {
            supervisor.run(stop);
            finished->store(true);
        });
        return run;
    }

    Result<std::shared_ptr<Shared>> shared(std::string_view server) {
        std::lock_guard lock(mutex);
        auto it = servers.find(server);
        if (it == servers.end()) return std::unexpected(unknown(server));
        return it->second.shared;
    }

    // Without `Manager::shutdown`, supervisors still close their children.
    ~Inner() {
        std::vector<Run> runs;
        for (auto& [name, entry] : servers)
            if (entry.run) runs.push_back(std::move(*entry.run));
        for (auto& run : runs) run.thread.request_stop();
    }
};

Result<Manager> Manager::make(Options options) {
    if (auto ok = detail::init_http(); !ok) return std::unexpected(Error::http_client(ok.error()));
    auto inner = std::make_shared<Inner>();
    inner->options = options;
    return Manager(std::move(inner));
}

EventReceiver Manager::events() const { return inner_->events->subscribe(); }

Result<> Manager::add(ServerSpec spec) const {
    if (auto ok = spec.validate(); !ok) return ok;
    std::lock_guard lock(inner_->mutex);
    if (inner_->servers.contains(spec.name)) return std::unexpected(Error::name_taken(spec.name));
    auto shared = std::make_shared<Shared>();
    shared->name = spec.name;
    shared->events = inner_->events;
    Entry entry{std::move(spec), std::move(shared), std::nullopt};
    if (entry.spec.enabled) entry.run = inner_->spawn(entry);
    auto name = entry.spec.name;
    inner_->servers.emplace(std::move(name), std::move(entry));
    return {};
}

Result<ServerSpec> Manager::remove(std::string_view name) const {
    Entry entry;
    {
        std::lock_guard lock(inner_->mutex);
        auto it = inner_->servers.find(name);
        if (it == inner_->servers.end()) return std::unexpected(unknown(name));
        entry = std::move(it->second);
        inner_->servers.erase(it);
    }
    finish(std::move(entry.run));
    inner_->events->send(event::Removed{std::string(name)});
    return std::move(entry.spec);
}

Result<> Manager::replace(std::string_view name, ServerSpec spec) const {
    if (auto ok = spec.validate(); !ok) return ok;
    if (spec.name != name) {
        std::lock_guard lock(inner_->mutex);
        if (inner_->servers.contains(spec.name)) return std::unexpected(Error::name_taken(spec.name));
    }
    auto removed = remove(name);
    if (!removed && removed.error().kind != Error::Kind::UnknownServer) return std::unexpected(std::move(removed.error()));
    return add(std::move(spec));
}

Result<> Manager::start(std::string_view name) const {
    std::lock_guard lock(inner_->mutex);
    auto it = inner_->servers.find(name);
    if (it == inner_->servers.end()) return std::unexpected(unknown(name));
    auto& entry = it->second;
    entry.spec.enabled = true;
    if (!entry.run || entry.run->finished->load()) {
        finish(std::move(entry.run));
        entry.run = inner_->spawn(entry);
    }
    return {};
}

Result<> Manager::stop(std::string_view name) const {
    std::optional<Run> run;
    {
        std::lock_guard lock(inner_->mutex);
        auto it = inner_->servers.find(name);
        if (it == inner_->servers.end()) return std::unexpected(unknown(name));
        it->second.spec.enabled = false;
        run = std::exchange(it->second.run, std::nullopt);
    }
    finish(std::move(run));
    return {};
}

Result<> Manager::restart(std::string_view name) const {
    if (auto ok = stop(name); !ok) return ok;
    return start(name);
}

void Manager::shutdown() const {
    std::vector<Run> runs;
    {
        std::lock_guard lock(inner_->mutex);
        for (auto& [name, entry] : inner_->servers)
            if (entry.run) runs.push_back(std::move(*std::exchange(entry.run, std::nullopt)));
    }
    for (auto& run : runs) run.thread.request_stop();
    for (auto& run : runs)
        if (run.thread.joinable()) run.thread.join();
}

std::vector<ServerSpec> Manager::specs() const {
    std::lock_guard lock(inner_->mutex);
    std::vector<ServerSpec> out;
    for (const auto& [name, entry] : inner_->servers) out.push_back(entry.spec);
    return out;
}

std::optional<Status> Manager::status(std::string_view name) const {
    auto shared = inner_->shared(name);
    if (!shared) return std::nullopt;
    return StatusWatch((*shared)->status).current();
}

std::optional<StatusWatch> Manager::watch(std::string_view name) const {
    auto shared = inner_->shared(name);
    if (!shared) return std::nullopt;
    return StatusWatch((*shared)->status);
}

std::vector<ServerView> Manager::view(const ToolFilter& filter) const {
    std::lock_guard lock(inner_->mutex);
    std::vector<ServerView> out;
    for (const auto& [name, entry] : inner_->servers) {
        ServerView view{entry.spec, StatusWatch(entry.shared->status).current(), {}};
        std::lock_guard tools_lock(entry.shared->mutex);
        for (const auto& tool : entry.shared->tools) view.tools.push_back(ToolView{tool, filter.allows(entry.spec.name, tool.name)});
        out.push_back(std::move(view));
    }
    return out;
}

Result<std::vector<model::Tool>> Manager::tools(std::string_view server, const ToolFilter& filter) const {
    auto shared = inner_->shared(server);
    if (!shared) return std::unexpected(std::move(shared.error()));
    std::lock_guard lock((*shared)->mutex);
    std::vector<model::Tool> out;
    for (const auto& tool : (*shared)->tools)
        if (filter.allows(server, tool.name)) out.push_back(tool);
    return out;
}

Result<model::CallToolResult> Manager::call_tool(std::string_view server, std::string_view tool, std::optional<model::JsonObject> arguments,
                                                 const ToolFilter& filter) const {
    if (!filter.allows(server, tool)) return std::unexpected(Error::not_allowed(std::string(server), std::string(tool)));
    auto shared = inner_->shared(server);
    if (!shared) return std::unexpected(std::move(shared.error()));
    auto connection = (*shared)->connection();
    if (!connection) return std::unexpected(std::move(connection.error()));
    auto result = (*connection)->call_tool(tool, std::move(arguments));
    if (!result) return std::unexpected(Error::service(std::string(server), result.error()));
    return std::move(*result);
}

Result<std::vector<model::Resource>> Manager::list_resources(std::string_view server) const {
    auto shared = inner_->shared(server);
    if (!shared) return std::unexpected(std::move(shared.error()));
    auto connection = (*shared)->connection();
    if (!connection) return std::unexpected(std::move(connection.error()));
    if ((*connection)->server_capabilities().find("resources") == nullptr) return std::vector<model::Resource>{};
    auto resources = (*connection)->list_all_resources();
    if (!resources) return std::unexpected(Error::service(std::string(server), resources.error()));
    return std::move(*resources);
}

Result<std::vector<model::ResourceContents>> Manager::read_resource(std::string_view server, std::string_view uri) const {
    auto shared = inner_->shared(server);
    if (!shared) return std::unexpected(std::move(shared.error()));
    auto connection = (*shared)->connection();
    if (!connection) return std::unexpected(std::move(connection.error()));
    auto contents = (*connection)->read_resource(uri);
    if (!contents) return std::unexpected(Error::service(std::string(server), contents.error()));
    return std::move(*contents);
}

Result<std::vector<std::pair<contracts::surface::SurfaceUri, model::Resource>>> Manager::surfaces(std::string_view server) const {
    auto resources = list_resources(server);
    if (!resources) return std::unexpected(std::move(resources.error()));
    std::vector<std::pair<contracts::surface::SurfaceUri, model::Resource>> out;
    for (auto& resource : *resources)
        if (auto uri = surface_uri(resource)) out.emplace_back(std::move(*uri), std::move(resource));
    return out;
}

Result<std::vector<model::Tool>> Manager::probe(const ServerSpec& spec) const {
    if (auto ok = spec.validate(); !ok) return std::unexpected(std::move(ok.error()));
    auto connection = Connection::open(spec, inner_->options.connect_timeout, {});
    if (!connection) return std::unexpected(std::move(connection.error()));
    auto tools = (*connection)->list_all_tools();
    (*connection)->close();
    if (!tools) return std::unexpected(Error::service(spec.name, tools.error()));
    return std::move(*tools);
}

} // namespace loa::mcp
