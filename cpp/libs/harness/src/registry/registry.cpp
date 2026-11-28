#include "loa/harness/registry/registry.hpp"

#include <cstdlib>
#include <map>
#include <mutex>

#include "loa/harness/account_support/opener.hpp"
#include "loa/harness/account_support/process.hpp"
#include "loa/harness/claude/account.hpp"
#include "loa/harness/claude/driver.hpp"
#include "loa/harness/native.hpp"
#include "loa/harness/omp/account.hpp"
#include "loa/harness/omp/session.hpp"
#include "loa/harness/omp2/omp2.hpp"
#include "loa/harness/opencode/opencode.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/fs.hpp"

namespace loa::harness::registry {

namespace {

using loa::contracts::HarnessTraits;

constexpr std::string_view ACP = "acp";
constexpr std::string_view OMP2 = "omp2";
// opencode's default server.
constexpr std::string_view OPENCODE_DEFAULT_URL = "http://127.0.0.1:4096";

const std::array<HarnessEntry, 6> ENTRIES{
    HarnessEntry{native::OMP, "omp", "harness.omp", "omp (from PATH)", HarnessTraits{OMP_APPROVAL_MODES, true, false}},
    HarnessEntry{native::CLAUDE, "Claude Code", "harness.claude", "claude (from PATH)", HarnessTraits{}},
    HarnessEntry{native::CODEX, "Codex", "harness.codex", "codex (from PATH)", HarnessTraits{}},
    HarnessEntry{native::OPENCODE, "OpenCode", "harness.opencode", OPENCODE_DEFAULT_URL, HarnessTraits{{}, false, true}},
    HarnessEntry{ACP, "ACP", "harness.acp", "ACP agent (opencode or gemini from PATH)", HarnessTraits{}},
    HarnessEntry{OMP2, "omp2", "harness.omp", "omp (from PATH)", HarnessTraits{}},
};

std::mutex builds_mutex;
std::map<std::string, HarnessBuild, std::less<>>& builds() {
    static std::map<std::string, HarnessBuild, std::less<>> map;
    return map;
}

class Entry final : public loa::contracts::HarnessFactory {
  public:
    Entry(const HarnessEntry& entry, HarnessBuild build, HarnessEnv env)
        : entry_(entry), build_(std::move(build)), env_(std::move(env)) {}

    std::string_view id() const override { return entry_.id; }
    std::string_view label() const override { return entry_.label; }
    std::string_view icon_token() const override { return entry_.icon; }
    std::string_view default_program() const override { return entry_.program; }
    std::shared_ptr<loa::contracts::Adapter> build(std::optional<std::filesystem::path> exe) const override {
        return build_.adapter ? build_.adapter(env_, std::move(exe)) : nullptr;
    }
    std::shared_ptr<loa::contracts::HarnessAccount> account(std::optional<std::filesystem::path> exe) const override {
        return build_.account ? build_.account(env_, std::move(exe)) : nullptr;
    }
    HarnessTraits traits() const override { return entry_.traits; }

  private:
    const HarnessEntry& entry_;
    HarnessBuild build_;
    HarnessEnv env_;
};

} // namespace

std::span<const HarnessEntry> entries() noexcept { return ENTRIES; }

void register_build(std::string_view id, HarnessBuild build) {
    std::scoped_lock lock(builds_mutex);
    builds().insert_or_assign(std::string(id), std::move(build));
}

std::vector<std::shared_ptr<loa::contracts::HarnessFactory>> factories(const HarnessEnv& env) {
    opencode::register_factory();
    std::vector<std::shared_ptr<loa::contracts::HarnessFactory>> out;
    std::scoped_lock lock(builds_mutex);
    for (const auto& entry : ENTRIES) {
        auto it = builds().find(entry.id);
        if (it == builds().end() && entry.id == native::OMP) {
            HarnessBuild omp_build{
                [](const HarnessEnv& env, std::optional<std::filesystem::path> exe) -> std::shared_ptr<loa::contracts::Adapter> {
                    omp::OmpAdapterConfig config(env.node, env.omp_dir);
                    config.approval_mode = std::string(OMP_APPROVAL_MODES[0]);
                    if (exe) config.executable = std::move(*exe);
                    return std::make_shared<omp::OmpAdapter>(std::move(config));
                },
                [](const HarnessEnv&, std::optional<std::filesystem::path> exe) -> std::shared_ptr<loa::contracts::HarnessAccount> {
                    omp::OmpLaunch launch;
                    launch.executable = exe.value_or(std::filesystem::path("omp"));
                    return std::make_shared<omp::OmpAccount>(std::make_shared<omp::OmpService>(launch),
                                                             std::make_shared<account_support::SystemUrlOpener>(), launch);
                }};
            out.push_back(std::make_shared<Entry>(entry, std::move(omp_build), env));
        } else if (it == builds().end() && entry.id == OMP2) {
            HarnessBuild omp2_build{
                [](const HarnessEnv&, std::optional<std::filesystem::path> exe) -> std::shared_ptr<loa::contracts::Adapter> {
                    return std::make_shared<omp2::Omp2Adapter>(exe.value_or(std::filesystem::path(omp2::EXECUTABLE)));
                }, {}};
            out.push_back(std::make_shared<Entry>(entry, std::move(omp2_build), env));
        } else if (it == builds().end() && entry.id == claude::HARNESS) {
            HarnessBuild build;
            build.adapter = [](const HarnessEnv&, std::optional<std::filesystem::path> exe) -> std::shared_ptr<loa::contracts::Adapter> {
                return std::make_shared<claude::ClaudeAdapter>(exe.value_or("claude"));
            };
            build.account = [](const HarnessEnv&, std::optional<std::filesystem::path> exe) {
                return std::make_shared<claude::ClaudeCode>(
                    exe.value_or("claude"), config_dir("CLAUDE_CONFIG_DIR", ".claude") / "settings.json",
                    std::make_shared<account_support::AsioProcessRunner>());
            };
            out.push_back(std::make_shared<Entry>(entry, std::move(build), env));
        } else {
            out.push_back(std::make_shared<Entry>(entry, it == builds().end() ? HarnessBuild{} : it->second, env));
        }
    }
    return out;
}

AcpLaunch acp_launch(std::optional<std::filesystem::path> exe, std::optional<std::string_view> path_var) {
    namespace exec = loa::runtime::exec;
    auto args_of = [](const AcpAgent& agent) { return std::vector<std::string>(agent.args.begin(), agent.args.end()); };
    if (exe) {
        const std::string name = exe->stem().string();
        for (const auto& agent : ACP_AGENTS)
            if (agent.name == name) return {std::move(*exe), args_of(agent), std::nullopt};
        return {std::move(*exe), {}, std::nullopt};
    }
    if (!path_var) {
        const char* path = std::getenv("PATH");
        if (path) path_var = path;
    }
    for (const auto& agent : ACP_AGENTS)
        if (auto found = exec::which_in(std::filesystem::path(agent.name), path_var))
            return {std::move(*found), args_of(agent), std::nullopt};
    std::string names;
    for (const auto& agent : ACP_AGENTS) {
        if (!names.empty()) names += ", ";
        names += agent.name;
    }
    return {std::filesystem::path(ACP_AGENTS[0].name), args_of(ACP_AGENTS[0]), "no ACP agent on PATH (looked for " + names + ")"};
}

std::filesystem::path config_dir(const char* variable, std::string_view fallback) {
    if (const char* value = std::getenv(variable); value && *value) return value;
    if (auto home = loa::runtime::fs::home()) return *home / fallback;
    return std::filesystem::path(fallback);
}

} // namespace loa::harness::registry
