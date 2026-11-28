#include "loa/sandbox/provider.hpp"

#include <asio.hpp>

#include <algorithm>
#include <format>

#include "loa/runtime/exec.hpp"
#include "loa/sandbox/providers.hpp"
#include "offload.hpp"
#include "process.hpp"

namespace loa::sandbox {

using contracts::Awaitable;

namespace {
constexpr std::string_view KIND_NAMES[] = {"local", "docker", "firecracker", "vm", "ssh", "peer"};

detail::Spawn to_spawn(const HostCommand& command) {
    detail::Spawn spec;
    spec.program = command.program;
    spec.args = command.args;
    spec.cwd = command.cwd;
    spec.clear_env = command.clear_env;
    spec.env = command.env;
    return spec;
}
} // namespace

std::string_view as_str(SandboxKind kind) noexcept { return KIND_NAMES[static_cast<std::size_t>(kind)]; }

std::optional<SandboxKind> parse_kind(std::string_view name) noexcept {
    for (const auto kind : SANDBOX_KINDS) {
        if (as_str(kind) == name) return kind;
    }
    return std::nullopt;
}

ProcessSandbox::ProcessSandbox(std::unique_ptr<detail::Process> child, Channel channel, WorkspaceMode mode,
                               std::unique_ptr<Teardown> teardown, std::string_view provider)
    : child_(std::move(child)), channel_(std::move(channel)), mode_(mode), teardown_(std::move(teardown)), provider_(provider) {}

ProcessSandbox::~ProcessSandbox() {
    if (teardown_) {
        auto spec = to_spawn(teardown_->command());
        spec.kill_on_drop = false;
        (void)detail::Process::spawn(spec);
    }
    channel_.reset();
}

std::optional<Channel> ProcessSandbox::take_channel() { return std::exchange(channel_, std::nullopt); }

Result<> ProcessSandbox::stop_blocking() {
    if (auto teardown = std::move(teardown_)) {
        auto out = detail::run(to_spawn(teardown->command()));
        if (!out) return std::unexpected(error::provider(provider_, std::format("teardown failed ({})", out.error().message())));
        if (!out->success()) {
            const auto stderr_text = detail::lossy_trim(out->err);
            if (!teardown->gone(stderr_text)) {
                const auto first = stderr_text.substr(0, stderr_text.find('\n'));
                return std::unexpected(error::provider(provider_, std::format("teardown failed ({}: {})", detail::status_string(out->status), first)));
            }
        }
    }
    channel_.reset();
    auto exited = child_->try_wait();
    if (!exited) return std::unexpected(exited.error());
    if (!*exited) child_->kill();
    if (auto waited = child_->wait(); !waited) return std::unexpected(waited.error());
    return {};
}

Awaitable<Result<>> ProcessSandbox::stop() {
    co_return co_await detail::offload([this] { return stop_blocking(); });
}

Result<std::unique_ptr<ProcessSandbox>> process(std::string_view provider, const HostCommand& command, WorkspaceMode mode,
                                                std::unique_ptr<Teardown> teardown) {
    auto spec = to_spawn(command);
    spec.in = detail::Stdio::piped;
    spec.out = detail::Stdio::piped;
    spec.err = detail::Stdio::null;
    auto child = detail::Process::spawn(spec);
    if (!child) return std::unexpected(child.error());
    auto proc = std::make_unique<detail::Process>(std::move(*child));
    Channel channel{std::make_unique<FdReader>(proc->take_stdout()), std::make_unique<FdWriter>(proc->take_stdin())};
    return std::make_unique<ProcessSandbox>(std::move(proc), std::move(channel), mode, std::move(teardown), provider);
}

Machine Machine::self() { return Machine{find_stub(), nullptr}; }

std::optional<std::filesystem::path> Machine::which(std::string_view name) const { return runtime::exec::which(name); }

bool Machine::kvm() const { return runtime::exec::kvm_available(); }

ProviderRegistry ProviderRegistry::builtin() {
    ProviderRegistry registry;
    registry.add(std::make_shared<LocalFactory>());
    registry.add(std::make_shared<DockerFactory>());
    registry.add(std::make_shared<SshFactory>());
    registry.add(std::make_shared<PeerFactory>());
#ifdef LOA_SANDBOX_VM
    registry.add(std::make_shared<FirecrackerFactory>()).add(std::make_shared<QemuFactory>());
#endif
    return registry;
}

ProviderRegistry& ProviderRegistry::add(std::shared_ptr<ProviderFactory> factory) {
    const auto kind = factory->kind();
    registry_.insert(std::string(as_str(kind)), std::move(factory));
    return *this;
}

std::vector<std::shared_ptr<ProviderFactory>> ProviderRegistry::factories() const {
    std::vector<std::shared_ptr<ProviderFactory>> out;
    for (const auto& [_, factory] : registry_) out.push_back(factory);
    std::ranges::sort(out, {}, [](const auto& factory) { return factory->kind(); });
    return out;
}

} // namespace loa::sandbox
