#include <doctest/doctest.h>

#include <unistd.h>

#include "loa/harness/registry/registry.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::harness::registry;

TEST_CASE("registry: lists the six harnesses in Rust order") {
    const auto list = factories(HarnessEnv{"node", "/omp"});
    std::vector<std::string_view> ids;
    for (const auto& f : list) ids.push_back(f->id());
    CHECK(ids == std::vector<std::string_view>{"omp", "claude-code", "codex", "opencode", "acp", "omp2"});
    CHECK(list[0]->label() == "omp");
    CHECK(list[0]->icon_token() == "harness.omp");
    CHECK(list[0]->traits().has_roles);
    CHECK(list[0]->traits().approval_modes.size() == 4);
    CHECK(list[0]->traits().approval_modes[0] == "always-ask");
    CHECK(list[1]->label() == "Claude Code");
    CHECK(list[3]->default_program() == "http://127.0.0.1:4096");
    CHECK(list[3]->traits().server_url);
    CHECK(list[4]->default_program() == "ACP agent (opencode or gemini from PATH)");
    CHECK(list[5]->icon_token() == "harness.omp");
    CHECK_FALSE(list[5]->traits().has_roles);
}

TEST_CASE("registry: registered builds receive the env") {
    std::string seen;
    register_build("codex", HarnessBuild{[&](const HarnessEnv& env, std::optional<std::filesystem::path>) {
                                             seen = env.node;
                                             return std::shared_ptr<loa::contracts::Adapter>();
                                         },
                                         {}});
    const auto list = factories(HarnessEnv{"me", {}});
    (void)list[2]->build(std::nullopt);
    CHECK(seen == "me");
    CHECK(list[2]->account(std::nullopt) == nullptr);
    // omp is built into the registry.
    CHECK(list[0]->build(std::nullopt) != nullptr);
    register_build("codex", {});
}

TEST_CASE("registry: ACP agent selection") {
    CHECK(ACP_AGENTS[0].name == "opencode");
    CHECK(ACP_AGENTS[1].args[0] == "--experimental-acp");
    auto named = acp_launch(std::filesystem::path("/opt/gemini"));
    CHECK(named == AcpLaunch{"/opt/gemini", {"--experimental-acp"}, std::nullopt});
    CHECK(acp_launch(std::filesystem::path("/opt/other")) == AcpLaunch{"/opt/other", {}, std::nullopt});

    auto dir = loa::runtime::TempDir::create().value();
    auto none = acp_launch(std::nullopt, dir.path().string());
    CHECK(none == AcpLaunch{"opencode", {"acp"}, "no ACP agent on PATH (looked for opencode, gemini)"});
    REQUIRE(loa::runtime::exec::write_executable(dir.path() / "gemini", "#!/bin/sh\n"));
    auto found = acp_launch(std::nullopt, dir.path().string());
    CHECK(found == AcpLaunch{dir.path() / "gemini", {"--experimental-acp"}, std::nullopt});
}

TEST_CASE("registry: config_dir prefers the variable, then home") {
    ::setenv("LOA_TEST_CONFIG_DIR", "/custom", 1);
    CHECK(config_dir("LOA_TEST_CONFIG_DIR", ".claude") == "/custom");
    ::setenv("LOA_TEST_CONFIG_DIR", "", 1);
    ::setenv("HOME", "/home/u", 1);
    CHECK(config_dir("LOA_TEST_CONFIG_DIR", ".claude") == "/home/u/.claude");
    ::unsetenv("LOA_TEST_CONFIG_DIR");
}
