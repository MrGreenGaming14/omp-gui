#include "loa/harness/codex/adapter.hpp"

#include <asio.hpp>
#include <fstream>
#include <functional>
#include <utility>

#include "loa/core/writer.hpp"
#include "loa/harness/account_support/process.hpp"
#include "loa/harness/codex/account.hpp"
#include "loa/harness/codex/mapper.hpp"
#include "loa/harness/codex/rollout.hpp"
#include "loa/harness/codex/session.hpp"
#include "loa/harness/registry/registry.hpp"

namespace loa::harness::codex {
namespace {
using namespace loa::contracts;

Result<std::string> native_thread(const loa::core::Dom& dom) {
    const auto* root = dom.root();
    auto id = root ? root->str_attr(loa::core::attr::NATIVE_ID) : std::nullopt;
    if (!id) return std::unexpected(AdapterError::Format{"the session has no Codex thread id (`native_id`)"});
    return std::string(*id);
}

} // namespace

CodexAdapter::CodexAdapter(std::string node) : node_(std::move(node)) {}
CodexAdapter CodexAdapter::with_executable(std::filesystem::path executable) const {
    auto copy = *this;
    copy.executable_ = std::move(executable);
    return copy;
}
CodexAdapter CodexAdapter::with_args(std::vector<std::string> args) const {
    auto copy = *this;
    copy.args_ = std::move(args);
    return copy;
}
std::string_view CodexAdapter::harness() const { return HARNESS; }
Awaitable<Availability> CodexAdapter::availability() const {
    co_return Availability::probe(executable_, {"--version"});
}
Capabilities CodexAdapter::capabilities() const {
    return Capabilities{.schema_version = loa::core::SCHEMA_VERSION,
                        .features = {Feature::Fork, Feature::Compaction, Feature::Subagents, Feature::PlanMode},
                        .model_switch = ModelSwitch::PerTurn,
                        .decisions = {loa::core::DecisionKind::Approval, loa::core::DecisionKind::Question,
                                      loa::core::DecisionKind::Form},
                        .context_delivery = {ContextDelivery::SystemPrompt},
                        .host_tools = true, .mcp_servers = true, .queue_mid_turn = false,
                        .effort = true, .import = true, .export_ = true};
}
Awaitable<Result<SessionPtr>> CodexAdapter::start(SessionConfig config,
                                                   std::shared_ptr<loa::core::PatchSink> sink) const {
    auto attach = config;
    co_return co_await CodexSession::open(*this, std::move(config), std::move(sink), Open::start(),
        [attach = std::move(attach)](std::string_view thread, std::optional<std::string_view> model,
                                      loa::core::DomWriter& writer) -> Result<Mapper> {
            return Mapper::start(attach, thread, model, writer);
        });
}
Awaitable<Result<SessionPtr>> CodexAdapter::resume(SessionConfig config, const loa::core::Dom& dom,
                                                    std::shared_ptr<loa::core::PatchSink> sink) const {
    auto thread = native_thread(dom);
    if (!thread) co_return std::unexpected(std::move(thread).error());
    co_return co_await CodexSession::open(*this, std::move(config), std::move(sink), Open::resume(std::move(*thread)),
        [&dom](std::string_view, std::optional<std::string_view>, loa::core::DomWriter& writer) -> Result<Mapper> {
            return Mapper::attach(dom, writer);
        });
}
Awaitable<Result<SessionPtr>> CodexAdapter::fork(SessionConfig config, const loa::core::Dom& dom,
                                                  std::shared_ptr<loa::core::PatchSink> sink) const {
    auto thread = native_thread(dom);
    if (!thread) co_return std::unexpected(std::move(thread).error());
    const auto* root = dom.root();
    if (!root) co_return std::unexpected(AdapterError::Format{"empty DOM"});
    std::optional<std::string> last_turn;
    for (const auto* turn : dom.children_of_kind(root->id, loa::core::ElementKind::Turn)) {
        if (auto id = turn->str_attr(loa::core::attr::ID)) last_turn = *id;
    }
    co_return co_await CodexSession::open(*this, std::move(config), std::move(sink),
        Open::fork(std::move(*thread), std::move(last_turn)),
        [&dom, root_id = root->id](std::string_view thread_id, std::optional<std::string_view>,
                                      loa::core::DomWriter& writer) -> Result<Mapper> {
            auto set = writer.set(root_id, loa::core::attr::NATIVE_ID, std::string(thread_id));
            if (!set) return std::unexpected(AdapterError::Commit{set.error()});
            return Mapper::attach(dom, writer);
        });
}
Result<> CodexAdapter::import(const std::filesystem::path& native, loa::core::PatchSink& sink) const {
    std::ifstream in(native, std::ios::binary);
    if (!in) return std::unexpected(AdapterError::Io{loa::Error{loa::ErrorCode::io, "couldn't open Codex rollout: " + native.string()}});
    return rollout::import(in, sink, node_);
}
Result<> CodexAdapter::export_(const loa::core::Dom& dom, const std::filesystem::path& out) const {
    return rollout::CodexNative::export_(dom, out);
}
void register_factory() {
    registry::register_build(HARNESS, registry::HarnessBuild{
        .adapter = [](const registry::HarnessEnv& env, std::optional<std::filesystem::path> exe) -> std::shared_ptr<Adapter> {
            auto adapter = CodexAdapter(env.node);
            if (exe) adapter = adapter.with_executable(std::move(*exe));
            return std::make_shared<CodexAdapter>(std::move(adapter));
        },
        .account = [](const registry::HarnessEnv&, std::optional<std::filesystem::path> exe) -> std::shared_ptr<HarnessAccount> {
            return std::make_shared<Codex>(exe.value_or("codex"),
                registry::config_dir("CODEX_HOME", ".codex") / "config.toml",
                std::make_shared<account_support::AsioProcessRunner>());
        }});
}

} // namespace loa::harness::codex
