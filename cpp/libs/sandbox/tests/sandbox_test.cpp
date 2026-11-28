// The stub protocol, the worker's host boundary and `sandbox_task` as the app wires it, against
// the real stub binary and a scripted harness standing in for the model (port of
// `crates/loa-sandbox/tests/sandbox.rs`).

#include <doctest/doctest.h>

#include <asio.hpp>

#include <cstdlib>
#include <fstream>
#include <functional>
#include <mutex>
#include <sstream>

#include "loa/contracts/harness.hpp"
#include "loa/core/sink.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/sandbox/client.hpp"
#include "loa/sandbox/isolation.hpp"
#include "loa/sandbox/providers.hpp"
#include "loa/sandbox/service.hpp"
#include "loa/sandbox/sync.hpp"
#include "loa/sandbox/worker.hpp"

namespace fs = std::filesystem;
namespace sb = loa::sandbox;
namespace c = loa::contracts;
namespace core = loa::core;
namespace json = loa::runtime::json;
using sb::proto::ExecRequest;

namespace {

fs::path stub() { return LOA_SANDBOX_STUB_PATH; }

template <typename T>
T block_on(c::Awaitable<T> task) {
    asio::io_context io;
    std::optional<T> out;
    asio::co_spawn(io, [&]() -> asio::awaitable<void> { out = co_await std::move(task); }, asio::detached);
    io.run();
    REQUIRE(out);
    return std::move(*out);
}

std::string read_file(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::ostringstream text;
    text << in.rdbuf();
    return text.str();
}

void write_file(const fs::path& path, std::string_view text) {
    std::ofstream(path, std::ios::binary) << text;
}

// Prints `out` to stdout and `err` to stderr, without newlines, and exits 7.
ExecRequest stdout_stderr_exit_7() { return ExecRequest::shell("printf \"$1\"; printf err >&2; exit 7", {"out"}); }

constexpr std::string_view BUSY_LOOP = "while :; do :; done";
// Deletes `gone.txt` and writes `new` to `new/file`.
constexpr std::string_view CHANGE_FILES = "rm gone.txt && mkdir new && echo new > new/file";
// The `sandbox_bash` call that prints the host key, or `unset` without it.
constexpr std::string_view PRINT_HOST_KEY = R"({"command":"printf '%s' \"${HOST_API_KEY-unset}\""})";
// The `sandbox_bash` call that overwrites `tracked.txt` with `changed`.
constexpr std::string_view OVERWRITE_TRACKED = R"({"command":"printf 'changed\\n' > tracked.txt"})";

class Journal final : public sb::WorkerJournal {
  public:
    sb::Result<std::shared_ptr<core::PatchSink>> open(std::string_view) const override { return std::make_shared<core::MemorySink>(); }
};

sb::SandboxService service() {
    sb::SandboxService service(sb::SandboxConfig{}, std::make_shared<Journal>());
    service.with_stub(stub());
    return service;
}

void git(const fs::path& dir, const std::string& args) {
    const auto command = std::format("cd '{}' && git {} >/dev/null 2>&1", dir.string(), args);
    REQUIRE_MESSAGE(std::system(command.c_str()) == 0, "git " << args);
}

loa::runtime::TempDir repository() {
    auto dir = loa::runtime::TempDir::create().value();
    git(dir.path(), "init -q");
    write_file(dir.path() / "tracked.txt", "original\n");
    git(dir.path(), "add .");
    git(dir.path(), "-c user.name=Test -c user.email=t@example.org commit -qm baseline");
    return dir;
}

// What a scripted session does with its sandbox tools: `(tool, arguments)`.
using Calls = std::vector<std::pair<std::string, std::string>>;

struct Log {
    std::mutex mutex;
    std::vector<std::string> lines;
    void push(std::string line) {
        std::scoped_lock lock(mutex);
        lines.push_back(std::move(line));
    }
    std::vector<std::string> copy() {
        std::scoped_lock lock(mutex);
        return lines;
    }
};

class Session final : public c::HarnessSession {
  public:
    Session(std::shared_ptr<const c::HostTools> tools, std::string id, std::shared_ptr<core::PatchSink> sink, Calls calls, bool asks,
            std::shared_ptr<Log> log, asio::any_io_executor executor)
        : tools_(std::move(tools)), id_(std::move(id)), sink_(std::move(sink)), calls_(std::move(calls)), asks_(asks), log_(std::move(log)),
          answered_(executor) {
        answered_.expires_at(asio::steady_timer::time_point::max());
    }

    c::Awaitable<c::Result<>> call() const {
        for (const auto& [tool, args] : calls_) {
            auto output = co_await tools_->call(id_, tool, args);
            if (!output) co_return std::unexpected(c::AdapterError(c::AdapterError::Protocol{output.error()}));
            log_->push(*output);
        }
        co_return c::Result<>{};
    }

    c::Awaitable<c::Result<>> send(c::Prompt) const override {
        if (!asks_) co_return co_await call();
        core::Attrs session_attrs{{std::string(core::attr::ID), core::Value(id_)},
                                  {std::string(core::attr::HARNESS), core::Value("scripted")},
                                  {std::string(core::attr::NODE), core::Value("host")},
                                  {std::string(core::attr::CREATED), core::Value(0)}};
        auto session = sink_->commit(core::Draft::insert(core::NodeId::DOCUMENT, core::ElementKind::Session, session_attrs, ""));
        REQUIRE(session);
        core::Attrs attrs{{std::string(core::attr::KIND), core::Value("approval")}, {std::string(core::attr::STATUS), core::Value("waiting")}};
        REQUIRE(sink_->commit(core::Draft::insert(session->node, core::ElementKind::Decision, attrs, "Allow tool: sandbox_write")));
        co_return c::Result<>{};
    }
    c::Awaitable<c::Result<>> interrupt() const override { co_return c::Result<>{}; }
    c::Awaitable<c::Result<>> answer(core::NodeId, c::Answer answer) const override {
        std::string values;
        for (const auto& v : answer.values) values += (values.empty() ? "" : ",") + v;
        log_->push(std::format("{} by {}", values, answer.by));
        answered_flag_ = true;
        answered_.cancel();
        co_return c::Result<>{};
    }
    std::string_view harness() const override { return "scripted"; }
    c::SessionState state() const override { return c::SessionState::Idle; }
    c::Awaitable<c::Result<>> wait_idle() const override {
        if (asks_) {
            while (!answered_flag_) co_await answered_.async_wait(asio::as_tuple(asio::use_awaitable));
            co_return co_await call();
        }
        co_return c::Result<>{};
    }
    c::Awaitable<c::Result<>> shutdown() const override { co_return c::Result<>{}; }

  private:
    std::shared_ptr<const c::HostTools> tools_;
    std::string id_;
    std::shared_ptr<core::PatchSink> sink_;
    Calls calls_;
    bool asks_;
    std::shared_ptr<Log> log_;
    mutable bool answered_flag_ = false;
    mutable asio::steady_timer answered_;
};

// A harness whose "model" makes fixed tool calls when prompted, or, when `asks`, first raises an
// approval and makes them once it is answered. Answers and tool outputs go to `log` in order.
class Scripted final : public c::Adapter {
  public:
    Calls calls;
    bool asks = false;
    std::function<void(const c::SessionConfig&)> check;
    std::shared_ptr<Log> log = std::make_shared<Log>();

    std::string_view harness() const override { return "scripted"; }
    c::Capabilities capabilities() const override { return {}; }
    c::Awaitable<c::Result<c::SessionPtr>> start(c::SessionConfig config, std::shared_ptr<core::PatchSink> sink) const override {
        for (const auto& tool : config.host_tools) CHECK((tool.name.starts_with("sandbox_") && tool.name != "sandbox_task"));
        if (check) check(config);
        if (!config.host_tool_handler) co_return std::unexpected(c::AdapterError(c::AdapterError::Format{"worker has no sandbox tools"}));
        auto executor = co_await asio::this_coro::executor;
        co_return std::make_unique<Session>(config.host_tool_handler, config.session_id, std::move(sink), calls, asks, log, executor);
    }
    c::Awaitable<c::Result<c::SessionPtr>> resume(c::SessionConfig, const core::Dom&, std::shared_ptr<core::PatchSink>) const override {
        co_return std::unexpected(c::AdapterError(c::AdapterError::Format{"unexpected resume"}));
    }
};

struct Connected {
    std::shared_ptr<sb::StubClient> client;
    std::unique_ptr<sb::ProcessSandbox> sandbox;
};

Connected connect(const fs::path& root) {
    sb::SandboxSpec spec{"protocol-test", root, {}, {}};
    auto sandbox = sb::LocalProvider(stub()).start_blocking(spec);
    REQUIRE(sandbox);
    auto channel = (*sandbox)->take_channel();
    REQUIRE(channel);
    auto client = sb::StubClient::connect_blocking(std::move(*channel));
    REQUIRE(client);
    return {std::move(*client), std::move(*sandbox)};
}

std::vector<std::string> names(const fs::path& dir) {
    std::vector<std::string> out;
    for (const auto& entry : fs::directory_iterator(dir)) out.push_back(entry.path().filename().string());
    std::ranges::sort(out);
    return out;
}

} // namespace

TEST_CASE("the stub streams output and reports the exit status") {
    auto root = loa::runtime::TempDir::create().value();
    auto [client, sandbox] = connect(root.path());
    auto output = client->exec_blocking(stdout_stderr_exit_7());
    REQUIRE(output);
    CHECK(output->stdout_data == "out");
    CHECK(output->stderr_data == "err");
    CHECK(output->status.code == 7);
    ExecRequest missing;
    missing.argv = {"missing-program"};
    auto error = client->run_blocking(missing);
    REQUIRE_FALSE(error);
    CHECK_MESSAGE(error.error().message().find("spawn") != std::string::npos, error.error().message());
    auto busy = ExecRequest::shell(BUSY_LOOP);
    busy.timeout_ms = 30;
    auto timed = client->exec_blocking(busy);
    REQUIRE(timed);
    CHECK(timed->status.timed_out);
    CHECK_FALSE(timed->status.success());
    // The coroutine API answers the same way.
    auto async_output = block_on(client->exec(stdout_stderr_exit_7()));
    REQUIRE(async_output);
    CHECK(async_output->stdout_data == "out");
    REQUIRE(client->shutdown_blocking());
    REQUIRE(sandbox->stop_blocking());
}

TEST_CASE("copied workspaces go in and come back without git data") {
    auto workspace = loa::runtime::TempDir::create().value();
    auto root = loa::runtime::TempDir::create().value();
    fs::create_directories(workspace.path() / ".git");
    write_file(workspace.path() / "kept.txt", "kept");
    write_file(workspace.path() / "gone.txt", "gone");
    auto [client, sandbox] = connect(root.path());
    auto in = sb::sync::copy_in(*client, workspace.path());
    REQUIRE_MESSAGE(in, in.error().message());
    CHECK_FALSE(fs::exists(root.path() / ".git"));
    CHECK(fs::exists(root.path() / "kept.txt"));
    REQUIRE(client->run_blocking(ExecRequest::shell(CHANGE_FILES)));
    auto out = sb::sync::copy_out(*client, workspace.path());
    REQUIRE_MESSAGE(out, out.error().message());
    CHECK(names(workspace.path()) == std::vector<std::string>{".git", "kept.txt", "new"});
    CHECK(read_file(workspace.path() / "new/file") == "new\n");
    REQUIRE(client->shutdown_blocking());
    REQUIRE(sandbox->stop_blocking());
}

// omp in always-ask mode asks before its tool calls and only goes on once the decision is
// answered; the worker answers for itself. Host secrets never reach the sandbox, and the
// workspace only changes through the patch.
TEST_CASE("an isolated run keeps secrets on the host and returns the patch") {
    auto repo = repository();
    auto svc = service();
    auto events = svc.subscribe();
    c::SessionConfig templ;
    templ.session_id = "parent";
    templ.node = "host";
    templ.cwd = repo.path();
    templ.env["HOST_API_KEY"] = "host-only-value";
    auto adapter = std::make_shared<Scripted>();
    adapter->calls = {
        {"sandbox_bash", std::string(PRINT_HOST_KEY)},
        {"sandbox_write", R"({"path":"notes/hello.txt","content":"from the sandbox\n"})"},
        {"sandbox_bash", std::string(OVERWRITE_TRACKED)},
        {"sandbox_list", R"({"path":"notes"})"},
        {"sandbox_read", R"({"path":"notes/hello.txt","limit":4})"},
    };
    adapter->asks = true;
    adapter->check = [](const c::SessionConfig& config) {
        CHECK(config.env.at("HOST_API_KEY") == "host-only-value");
        CHECK(config.mcp_servers.empty());
        CHECK(std::ranges::find(config.disabled_tools, "bash") != config.disabled_tools.end());
        CHECK_MESSAGE(!fs::exists(config.cwd / "tracked.txt"), "the harness cwd shows the workspace");
    };
    auto log = adapter->log;
    auto tool = svc.host_tool(adapter, templ);
    REQUIRE_MESSAGE(tool, "the local provider is available");
    auto& [spec, handler] = *tool;
    CHECK(spec.name == "sandbox_task");
    auto schema = json::parse(spec.input_schema);
    REQUIRE(schema);
    const auto* kinds = schema->find("properties")->find("kind")->find("enum")->as_array();
    REQUIRE(kinds);
    CHECK(std::ranges::find(*kinds, json::Value("local")) != kinds->end());
    CHECK(std::ranges::find(*kinds, json::Value("firecracker")) == kinds->end());

    auto answer = block_on(handler->call("parent", "sandbox_task", R"({"task":"Write the notes","kind":"local","isolation":true})"));
    REQUIRE_MESSAGE(answer, (answer ? "" : answer.error()));
    auto parsed = json::parse(*answer);
    REQUIRE(parsed);
    auto result = sb::SandboxTaskResult::from_json(*parsed);
    REQUIRE(result);
    const auto lines = log->copy();
    REQUIRE(lines.size() == 6);
    CHECK_MESSAGE(lines[0] == "allow_once by host", "the worker answers its one approval");
    CHECK_MESSAGE(lines[1].find("unset") != std::string::npos, "the sandbox inherited the host environment: " << lines[1]);
    CHECK(lines[4].find("hello.txt") != std::string::npos);
    CHECK(lines[5] == "from");

    REQUIRE_MESSAGE(result->patch, "isolated runs return a patch");
    const auto& patch = *result->patch;
    CHECK(patch.find("+from the sandbox") != std::string::npos);
    CHECK(patch.find("-original") != std::string::npos);
    CHECK(patch.find("+changed") != std::string::npos);
    std::vector<std::string> files;
    for (const auto& file : sb::patch_files(patch)) files.push_back(file.path);
    CHECK(files == std::vector<std::string>{"notes/hello.txt", "tracked.txt"});
    CHECK_FALSE(fs::exists(repo.path() / "notes"));
    CHECK(read_file(repo.path() / "tracked.txt") == "original\n");
    const auto worktrees = std::format("cd '{}' && test \"$(git worktree list | wc -l)\" = 1", repo.path().string());
    CHECK_MESSAGE(std::system(worktrees.c_str()) == 0, "the isolated worktree was removed");

    auto first = events->recv();
    REQUIRE(first);
    const auto* started = std::get_if<sb::event::Started>(&*first);
    REQUIRE_MESSAGE(started, "a start first");
    CHECK(started->run.parent == "parent");
    CHECK(started->run.kind == sb::SandboxKind::Local);
    CHECK(started->run.isolated);
    CHECK(started->run.worker == result->session_id);
    auto second = events->recv();
    REQUIRE(second);
    const auto* finished = std::get_if<sb::event::Finished>(&*second);
    REQUIRE_MESSAGE(finished, "the finish with its patch");
    CHECK(finished->run == started->run.run);
    CHECK(finished->patch == patch);

    REQUIRE(sb::apply_patch(repo.path(), patch));
    CHECK(read_file(repo.path() / "notes/hello.txt") == "from the sandbox\n");
    CHECK(read_file(repo.path() / "tracked.txt") == "changed\n");
}

TEST_CASE("a failed run is reported") {
    auto plain = loa::runtime::TempDir::create().value();
    auto svc = service();
    auto events = svc.subscribe();
    c::SessionConfig templ;
    templ.session_id = "parent";
    templ.cwd = plain.path();
    auto tool = svc.host_tool(std::make_shared<Scripted>(), templ);
    REQUIRE(tool);
    const auto& handler = tool->second;
    auto error = block_on(handler->call("parent", "sandbox_task", R"({"task":"x","kind":"local","isolation":true})"));
    REQUIRE_FALSE(error);
    CHECK_MESSAGE(error.error().find("isolation") != std::string::npos, error.error());
    auto started = events->recv();
    REQUIRE(started);
    CHECK(std::holds_alternative<sb::event::Started>(*started));
    auto failed = events->recv();
    REQUIRE(failed);
    const auto* f = std::get_if<sb::event::Failed>(&*failed);
    REQUIRE(f);
    CHECK(error.error().find(f->error) != std::string::npos);
    auto missing = block_on(handler->call("parent", "sandbox_task", R"({"task":"x","kind":"firecracker"})"));
    REQUIRE_FALSE(missing);
    CHECK_MESSAGE(missing.error().find("firecracker") != std::string::npos, missing.error());
}
