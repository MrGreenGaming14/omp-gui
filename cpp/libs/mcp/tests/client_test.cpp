// The client against fake MCP servers: `loa_mcp_fake_server` as a stdio child, and the same
// handler behind an in-process streamable HTTP endpoint on a loopback port (ports of `tests/mcp.rs`).

#include <doctest/doctest.h>

#include <asio.hpp>
#include <httplib.h>

#include <filesystem>
#include <fstream>
#include <future>
#include <thread>

#include "fake.hpp"
#include "loa/mcp/host_tools.hpp"
#include "loa/mcp/registry.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::mcp;
namespace json = loa::runtime::json;
using namespace std::chrono_literals;

namespace {

// Upper bound for anything a test waits on; never a synchronization sleep.
constexpr auto LIMIT = 20s;

Options options() { return Options{Backoff{10ms, 200ms}, LIMIT}; }

Manager manager() { return *Manager::make(options()); }

StdioSpec fake_stdio() { return StdioSpec{LOA_MCP_FAKE_SERVER, {}, {{"LOA_MCP_FAKE_SERVER", "1"}}, std::nullopt}; }

ServerSpec fake_spec(std::string name) { return *ServerSpec::make(std::move(name), fake_stdio()); }

bool running(const Status& status) { return status == Status::running(); }

template <typename F>
Status wait_status(const Manager& manager, std::string_view name, F wanted) {
    auto watch = manager.watch(name);
    REQUIRE(watch);
    auto seen = watch->wait_for(wanted, LIMIT);
    REQUIRE_MESSAGE(seen, "status didn't arrive in time");
    return *seen;
}

template <typename F>
Event next_event(EventReceiver& events, F wanted) {
    const auto deadline = std::chrono::steady_clock::now() + LIMIT;
    while (std::chrono::steady_clock::now() < deadline) {
        auto event = events.recv(std::chrono::duration_cast<std::chrono::milliseconds>(deadline - std::chrono::steady_clock::now()));
        if (event && wanted(*event)) return *event;
    }
    FAIL("event didn't arrive in time");
    return {};
}

std::string call_text(const Manager& manager, std::string_view server, std::string_view tool, json::Object arguments = {}) {
    auto result = manager.call_tool(server, tool, std::move(arguments), AllowAll());
    REQUIRE_MESSAGE(result, result.error().to_string());
    CHECK(result->is_error != true);
    REQUIRE(result->text());
    return *result->text();
}

std::vector<std::string> names(const std::vector<model::Tool>& tools) {
    std::vector<std::string> out;
    for (const auto& tool : tools) out.push_back(tool.name);
    return out;
}

bool alive(int pid) { return std::filesystem::exists("/proc/" + std::to_string(pid)); }

// Serves the fake on `127.0.0.1:<free port>/mcp`, refusing requests without `X-Api-Key: <key>`.
class HttpFake {
  public:
    explicit HttpFake(std::string key) : key_(std::move(key)) {
        server_.Post("/mcp", [this](const httplib::Request& request, httplib::Response& response) {
            if (!authorized(request, response)) return;
            auto message = json::parse(request.body);
            if (!message) {
                response.status = 400;
                return;
            }
            std::vector<json::Value> notes;
            auto reply = fake_.handle(*message, [&](const json::Value& note) { notes.push_back(note); });
            if (!reply) {
                response.status = 202;
                return;
            }
            const auto* method = message->find("method");
            if (method && method->as_string() && *method->as_string() == "initialize") response.set_header("Mcp-Session-Id", "s-1");
            if (notes.empty()) {
                response.set_content(json::dump(*reply), "application/json");
                return;
            }
            std::string body;
            for (const auto& note : notes) body += "data: " + json::dump(note) + "\n\n";
            body += "id: 1\ndata: " + json::dump(*reply) + "\n\n";
            response.set_content(body, "text/event-stream");
        });
        server_.Get("/mcp", [this](const httplib::Request& request, httplib::Response& response) {
            if (authorized(request, response)) response.status = 405;
        });
        server_.Delete("/mcp", [this](const httplib::Request& request, httplib::Response& response) {
            if (authorized(request, response)) response.status = 200;
        });
        port_ = server_.bind_to_any_port("127.0.0.1");
        thread_ = std::thread([this] { server_.listen_after_bind(); });
        server_.wait_until_ready();
    }
    ~HttpFake() {
        server_.stop();
        thread_.join();
    }
    [[nodiscard]] std::string url() const { return "http://127.0.0.1:" + std::to_string(port_) + "/mcp"; }

  private:
    bool authorized(const httplib::Request& request, httplib::Response& response) {
        if (request.get_header_value("X-Api-Key") == key_) return true;
        response.status = 401;
        return false;
    }

    std::string key_;
    fake::Fake fake_;
    httplib::Server server_;
    int port_ = 0;
    std::thread thread_;
};

class FailingStore final : public ServerStore {
  public:
    std::expected<std::vector<ServerSpec>, StoreError> load() const override {
        auto kept = fake_spec("kept");
        kept.enabled = false;
        return std::vector{kept};
    }
    std::expected<void, StoreError> save(const std::vector<ServerSpec>&) const override { return std::unexpected(std::string("disk full")); }
};

} // namespace

TEST_CASE("stdio server lists, calls and reads resources") {
    const auto manager = ::manager();
    REQUIRE(manager.add(fake_spec("fake")));
    wait_status(manager, "fake", running);

    CHECK(names(*manager.tools("fake", AllowAll())) == std::vector<std::string>{"echo", "pid", "crash", "grow", "vault_code", "picture"});
    CHECK(call_text(manager, "fake", "echo", json::Object{{"text", "hello"}}) == "hello");

    CHECK(manager.list_resources("fake")->size() == 2);
    auto surfaces = manager.surfaces("fake");
    REQUIRE(surfaces);
    REQUIRE(surfaces->size() == 1);
    CHECK((*surfaces)[0].first.as_str() == "surface://fake/tab1");
    CHECK((*surfaces)[0].second.name == "Tab 1");

    auto contents = manager.read_resource("fake", "file:///notes.txt");
    REQUIRE(contents);
    REQUIRE(contents->size() == 1);
    const auto* text = std::get_if<model::TextResourceContents>(&(*contents)[0]);
    REQUIRE(text);
    CHECK(text->text == "the notes");

    auto events = manager.events();
    call_text(manager, "fake", "grow");
    next_event(events, [](const Event& e) {
        const auto* changed = std::get_if<event::ToolsChanged>(&e);
        return changed && changed->server == "fake";
    });
    CHECK(std::ranges::contains(names(*manager.tools("fake", AllowAll())), std::string("grown")));

    const int pid = std::stoi(call_text(manager, "fake", "pid"));
    manager.shutdown();
    CHECK(manager.status("fake") == Status::stopped());
    CHECK_MESSAGE(!alive(pid), "the child outlived shutdown");
    CHECK(manager.call_tool("fake", "echo", std::nullopt, AllowAll()).error().kind == Error::Kind::NotRunning);
}

TEST_CASE("filter hides and refuses tools, and missing commands stop cleanly") {
    const auto manager = ::manager();
    REQUIRE(manager.add(fake_spec("fake")));
    REQUIRE(manager.add(*ServerSpec::make("ghost", *StdioSpec::parse("/nonexistent/loa-mcp-ghost --x"))));
    wait_status(manager, "fake", running);
    const auto ghost = wait_status(manager, "ghost", [](const Status& s) { return s.kind == Status::Kind::Error; });
    CHECK_MESSAGE(ghost.message.find("/nonexistent/loa-mcp-ghost") != std::string::npos, ghost.message);
    REQUIRE(manager.stop("ghost"));
    CHECK(manager.status("ghost") == Status::stopped());

    // A peer whose grant allows only `echo` on `fake`.
    const FnFilter grant([](std::string_view server, std::string_view tool) { return server == "fake" && tool == "echo"; });
    CHECK(names(*manager.tools("fake", grant)) == std::vector<std::string>{"echo"});

    std::vector<std::pair<std::string, bool>> allowed;
    for (const auto& view : manager.view(grant))
        if (view.spec.name == "fake")
            for (const auto& tool : view.tools) allowed.emplace_back(tool.tool.name, tool.allowed);
    const std::vector<std::pair<std::string, bool>> expect = {{"echo", true},  {"pid", false},        {"crash", false},
                                                              {"grow", false}, {"vault_code", false}, {"picture", false}};
    CHECK(allowed == expect);

    CHECK(manager.call_tool("fake", "crash", std::nullopt, grant).error().kind == Error::Kind::NotAllowed);
    // The refused crash never reached the server.
    CHECK(manager.status("fake") == Status::running());
    manager.shutdown();
}

TEST_CASE("crashed server restarts with its error shown") {
    const auto manager = ::manager();
    REQUIRE(manager.add(fake_spec("fake")));
    wait_status(manager, "fake", running);
    const int first = std::stoi(call_text(manager, "fake", "pid"));

    auto events = manager.events();
    CHECK(!manager.call_tool("fake", "crash", std::nullopt, AllowAll()));
    const auto failed = next_event(events, [](const Event& e) {
        const auto* status = std::get_if<event::Status>(&e);
        return status && status->status.kind == Status::Kind::Error;
    });
    const auto& message = std::get<event::Status>(failed).status.message;
    CHECK_MESSAGE(message.find("crash requested") != std::string::npos, message);
    next_event(events, [](const Event& e) {
        const auto* status = std::get_if<event::Status>(&e);
        return status && status->status == Status::running();
    });

    const int second = std::stoi(call_text(manager, "fake", "pid"));
    CHECK(first != second);
    CHECK(call_text(manager, "fake", "echo", json::Object{{"text", "back"}}) == "back");
    manager.shutdown();
}

TEST_CASE("http server is validated with its headers") {
    const HttpFake fake("secret");
    auto registry = *Registry::open(std::make_shared<MemoryStore>(), manager());

    auto without = *ServerSpec::make("web", *HttpSpec::make(fake.url()));
    CHECK(registry->add(without).error().kind == Error::Kind::Connect);
    CHECK(registry->specs().empty());
    CHECK(!registry->manager().status("web"));

    auto with = *ServerSpec::make("web", *HttpSpec::make(fake.url(), {{"X-Api-Key", "secret"}}));
    auto tools = registry->add(with);
    REQUIRE_MESSAGE(tools, tools.error().to_string());
    CHECK(tools->size() == 6);
    CHECK(registry->specs() == std::vector{with});

    const auto& manager = registry->manager();
    wait_status(manager, "web", running);
    CHECK(call_text(manager, "web", "echo", json::Object{{"text", "over http"}}) == "over http");
    CHECK(manager.read_resource("web", "file:///notes.txt")->size() == 1);
    // A notification streamed ahead of the reply reaches the client.
    auto events = manager.events();
    call_text(manager, "web", "grow");
    next_event(events, [](const Event& e) { return std::holds_alternative<event::ToolsChanged>(e); });
    manager.shutdown();
}

TEST_CASE("registry edits and removes servers") {
    auto store = std::make_shared<MemoryStore>();
    auto registry = *Registry::open(store, manager());
    REQUIRE(registry->add(fake_spec("one")));
    CHECK(registry->add(fake_spec("one")).error().kind == Error::Kind::NameTaken);
    CHECK(registry->edit("nope", fake_spec("x")).error().kind == Error::Kind::UnknownServer);

    auto broken = *ServerSpec::make("two", *StdioSpec::parse("/nonexistent/loa-mcp-ghost"));
    CHECK(registry->edit("one", broken).error().kind == Error::Kind::Connect);
    CHECK(store->servers() == std::vector{fake_spec("one")});

    auto renamed = fake_spec("two");
    std::get<StdioSpec>(renamed.transport).env["EXTRA"] = "1";
    REQUIRE(registry->edit("one", renamed));
    CHECK(store->servers() == std::vector{renamed});
    CHECK(!registry->manager().status("one"));
    wait_status(registry->manager(), "two", running);

    REQUIRE(registry->set_enabled("two", false));
    CHECK(registry->manager().status("two") == Status::stopped());
    CHECK(!store->servers()[0].enabled);

    REQUIRE(registry->remove("two"));
    CHECK(store->servers().empty());
    CHECK(registry->manager().specs().empty());
}

TEST_CASE("registry import starts new servers once") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto stdio = fake_stdio();
    json::Object env;
    for (const auto& [k, v] : stdio.env) env.emplace_back(k, v);
    const json::Value config = json::Object{{"mcpServers", json::Object{{"imported", json::Object{{"command", stdio.command}, {"env", env}}}}}};
    const auto path = dir->path() / ".mcp.json";
    std::ofstream(path) << json::dump(config);
    std::vector<std::unique_ptr<import::ImportSource>> files;
    files.push_back(std::make_unique<import::JsonServers>(import::App::Claude, path));

    auto store = std::make_shared<MemoryStore>();
    auto registry = *Registry::open(store, manager());
    auto report = registry->import_found(import::scan(files).first);
    REQUIRE(report);
    CHECK(std::holds_alternative<import::outcome::Imported>(report->entries[0].outcome));
    CHECK(store->servers().size() == 1);
    wait_status(registry->manager(), "imported", running);

    auto again = registry->import_found(import::scan(files).first);
    REQUIRE(again);
    CHECK(again->entries[0].outcome == import::Outcome(import::outcome::Duplicate{"imported"}));
    CHECK(store->servers().size() == 1);
    registry->manager().shutdown();
}

TEST_CASE("registry rolls back a failed save") {
    auto registry = *Registry::open(std::make_shared<FailingStore>(), manager());
    CHECK(registry->remove("kept").error().kind == Error::Kind::Store);
    CHECK(registry->set_enabled("nope", true).error().kind == Error::Kind::UnknownServer);
    CHECK(registry->specs().size() == 1);
    registry->manager().shutdown();
}

TEST_CASE("registry save and rollback on a memory store") {
    auto spec_a = fake_spec("a");
    spec_a.enabled = false;
    auto spec_b = fake_spec("b");
    spec_b.enabled = false;
    auto store = std::make_shared<MemoryStore>(std::vector{spec_a});
    auto registry = *Registry::open(store, manager());
    auto committed = registry->commit<int>([&](std::vector<ServerSpec>& specs) -> Result<int> {
        specs.push_back(spec_b);
        return 7;
    });
    CHECK(*committed == 7);
    CHECK(store->servers().size() == 2);
    auto refused = registry->commit<void>([&](std::vector<ServerSpec>& specs) -> Result<> {
        specs.clear();
        return std::unexpected(Error::name_taken("a"));
    });
    CHECK(refused.error().kind == Error::Kind::NameTaken);
    CHECK(registry->specs().size() == 2);
    CHECK(store->servers().size() == 2);
    registry->manager().shutdown();
}

TEST_CASE("host tools route calls through the filter") {
    using namespace loa::mcp::host_tools;
    CHECK(host_tool_name("github.com", "list issues") == "github_com__list_issues");
    CHECK(host_tool_name("fs-2", "read_file") == "fs-2__read_file");

    const auto manager = ::manager();
    REQUIRE(manager.add(fake_spec("fake")));
    auto off = fake_spec("off");
    off.enabled = false;
    REQUIRE(manager.add(off));
    wait_status(manager, "fake", running);
    auto filter = std::make_shared<FnFilter>([](std::string_view server, std::string_view tool) { return server == "fake" && tool != "crash"; });
    const McpHostTools tools(manager, filter);
    std::vector<std::string> listed;
    for (const auto& spec : tools.specs()) listed.push_back(spec.name);
    CHECK(listed == std::vector<std::string>{"fake__echo", "fake__pid", "fake__grow", "fake__vault_code", "fake__picture"});
    const auto& vault = tools.specs()[3];
    CHECK(vault.description == "[fake] Returns the current vault code. Takes no arguments.");
    CHECK(*json::parse(vault.input_schema)->find("type") == json::Value("object"));

    asio::io_context io;
    auto run = [&](auto awaitable) {
        std::optional<typename decltype(awaitable)::value_type> out;
        asio::co_spawn(io, [&]() -> asio::awaitable<void> { out = co_await std::move(awaitable); }, asio::detached);
        io.restart();
        io.run();
        return std::move(*out);
    };
    CHECK(*run(tools.call("s", "fake__echo", R"({"text":"hi"})")) == "hi");
    CHECK(*run(tools.call("s", "fake__vault_code", "{}")) == fake::VAULT_CODE);
    auto blocks = run(tools.call_content("s", "fake__picture", "{}"));
    REQUIRE(blocks);
    REQUIRE(blocks->size() == 2);
    CHECK((*blocks)[0] == loa::contracts::OutputBlock(loa::contracts::TextBlock{"a picture"}));
    const auto* image = std::get_if<loa::contracts::ImageBlock>(&(*blocks)[1]);
    REQUIRE(image);
    CHECK(image->mime == "image/png");
    CHECK(image->data.size() == fake::PNG.size());
    CHECK(std::memcmp(image->data.data(), fake::PNG.data(), fake::PNG.size()) == 0);
    CHECK_MESSAGE(run(tools.call("s", "fake__crash", "{}")).error().find("unknown MCP tool") != std::string::npos, "filtered out");
    CHECK(!run(tools.call("s", "fake__echo", "[1]")));
    manager.shutdown();
}
