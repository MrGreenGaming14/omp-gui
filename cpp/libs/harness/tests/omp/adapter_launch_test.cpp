#include <doctest/doctest.h>

#include <algorithm>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

#include "loa/harness/omp/launch.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::harness::omp;
namespace fs = std::filesystem;

namespace {

std::string contents(const fs::path& path) {
    std::ifstream stream(path, std::ios::binary);
    REQUIRE(stream.good());
    return {std::istreambuf_iterator<char>(stream), std::istreambuf_iterator<char>()};
}

std::string value_after(const rpc::SpawnOptions& opts, std::string_view flag) {
    const auto it = std::find(opts.args.begin(), opts.args.end(), flag);
    REQUIRE(it != opts.args.end());
    REQUIRE(std::next(it) != opts.args.end());
    return *std::next(it);
}

bool contains(const rpc::SpawnOptions& opts, std::string_view flag) {
    return std::find(opts.args.begin(), opts.args.end(), flag) != opts.args.end();
}

} // namespace

TEST_CASE("omp launch: default memory overlay has exact YAML bytes and project digest plus instructions") {
    auto dir = loa::runtime::TempDir::create().value();
    OmpAdapterConfig adapter("owner", dir.path() / "state");
    loa::contracts::SessionConfig session;
    session.session_id = "role/test";
    session.context = "Project context";
    session.instructions = "Role instructions";
    const auto opts = spawn_options(adapter, session, Target{});
    REQUIRE(opts);
    CHECK(opts->mode == rpc::Mode::RpcUi);
    CHECK(opts->ready_timeout == std::chrono::seconds(60));
    CHECK(contents(value_after(*opts, "--config")) == "memory:\n  backend: \"off\"\n");
    CHECK(contents(value_after(*opts, "--append-system-prompt")) == "Project context\n\nRole instructions");
    CHECK(fs::path(value_after(*opts, "--append-system-prompt")).filename() == "context-role_test.md");
    const std::vector<std::string> expected_args{"--config", (adapter.state_dir / "memory-off.yml").string(),
                                                  "--append-system-prompt", (adapter.state_dir / "context-role_test.md").string()};
    CHECK(opts->args == expected_args);
}
TEST_CASE("omp launch: omp memory omits context but still delivers instructions") {
    auto dir = loa::runtime::TempDir::create().value();
    OmpAdapterConfig adapter("owner", dir.path());
    adapter.memory = MemoryMode::OmpMemory;
    loa::contracts::SessionConfig session;
    session.context = "Digest";
    session.instructions = "Role: DEEP";
    auto opts = spawn_options(adapter, session, Target{});
    REQUIRE(opts);
    CHECK_FALSE(contains(*opts, "--config"));
    CHECK(contents(value_after(*opts, "--append-system-prompt")) == "Role: DEEP");
    session.instructions = " \t\n ";
    opts = spawn_options(adapter, session, Target{});
    REQUIRE(opts);
    CHECK_FALSE(contains(*opts, "--append-system-prompt"));
    CHECK_FALSE(fs::exists(dir.path() / "memory-off.yml"));
}

TEST_CASE("omp launch: session overrides, target, environment and tool filtering") {
    auto dir = loa::runtime::TempDir::create().value();
    OmpAdapterConfig adapter("owner", dir.path());
    adapter.executable = "default-omp";
    adapter.approval_mode = "write";
    adapter.session_dir = dir.path() / "sessions";
    adapter.ready_timeout = std::chrono::seconds(13);
    adapter.shutdown_grace = std::chrono::seconds(4);
    loa::contracts::SessionConfig session;
    session.session_id = "id";
    session.cwd = dir.path();
    session.executable = "override-omp";
    session.env = {{"A", "one"}, {"B", "two"}};
    session.approval_mode = "always-ask";
    session.model = "anthropic/sonnet";
    session.disabled_tools = {"find", "debug", "not-built-in"};
    const auto opts = spawn_options(adapter, session, Target::resume("resume-id"));
    REQUIRE(opts);
    CHECK(opts->executable == fs::path("override-omp"));
    CHECK(opts->cwd == session.cwd);
    CHECK(opts->ready_timeout == std::chrono::seconds(13));
    const std::vector<std::pair<std::string, std::string>> expected_env{{"A", "one"}, {"B", "two"}};
    CHECK(opts->env == expected_env);
    CHECK(value_after(*opts, "--session-dir") == adapter.session_dir->string());
    CHECK(value_after(*opts, "--approval-mode") == "always-ask");
    CHECK(value_after(*opts, "--model") == "anthropic/sonnet");
    CHECK(value_after(*opts, "--resume") == "resume-id");
    CHECK(contains(*opts, "--tools=read,edit,write,bash,grep,glob,eval,task,wait,todo,ask,web_search,lsp,ast_edit"));
    session.disabled_tools.clear();
    auto all = spawn_options(adapter, session, Target{});
    REQUIRE(all);
    CHECK_FALSE(contains(*all, "--resume"));
    CHECK_FALSE(std::ranges::any_of(all->args, [](const std::string& arg) { return arg.starts_with("--tools="); }));
    CHECK_FALSE(contains(*all, "--no-tools"));
    session.approval_mode.reset();
    all = spawn_options(adapter, session, Target{});
    REQUIRE(all);
    CHECK(value_after(*all, "--approval-mode") == "write");
}

TEST_CASE("omp launch: unavailable tools are removed only when omp reports them") {
    rpc::SpawnOptions opts;
    opts.args = {"--config", "file", "--tools=read,find,lsp"};
    CHECK_FALSE(drop_unavailable_tools(opts, "a different error"));
    CHECK(drop_unavailable_tools(opts, "warning\nBuilt-in tools unavailable in this session: find, lsp.\n"));
    CHECK(opts.args.back() == "--tools=read");
    CHECK_FALSE(drop_unavailable_tools(opts, "Built-in tool unavailable in this session: find."));
    CHECK(drop_unavailable_tools(opts, "Built-in tool unavailable in this session: read."));
    CHECK(opts.args.back() == "--no-tools");
    CHECK_FALSE(drop_unavailable_tools(opts, "Built-in tool unavailable in this session: read."));
}

TEST_CASE("omp launch: selector splits only on the first slash") {
    auto selector = split_selector("provider/model/variant");
    REQUIRE(selector);
    CHECK(selector->first == "provider");
    CHECK(selector->second == "model/variant");
    for (auto invalid : {"provider", "/model", "provider/", ""}) {
        auto result = split_selector(invalid);
        REQUIRE_FALSE(result);
        CHECK(result.error().get<loa::contracts::AdapterError::Protocol>() != nullptr);
    }
}

TEST_CASE("omp launch: state file errors propagate as adapter I/O errors") {
    auto dir = loa::runtime::TempDir::create().value();
    const auto file = dir.path() / "not-a-directory";
    { std::ofstream output(file); output << "existing"; }
    OmpAdapterConfig adapter("owner", file);
    auto result = spawn_options(adapter, loa::contracts::SessionConfig{}, Target{});
    REQUIRE_FALSE(result);
    CHECK(result.error().get<loa::contracts::AdapterError::Io>() != nullptr);
}
