#include "loa/sandbox/providers.hpp"

#include <asio.hpp>

#include <cstdlib>
#include <format>
#include <fstream>
#include <iterator>
#include <thread>

#include "loa/runtime/exec.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/runtime/ssh.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/sandbox/isolation.hpp"
#include "loa/sandbox/service.hpp"
#include "offload.hpp"
#include "process.hpp"

namespace loa::sandbox {

namespace fs = std::filesystem;
using contracts::Awaitable;

namespace {

constexpr bool WINDOWS =
#ifdef _WIN32
    true;
#else
    false;
#endif

ProviderStatus status(SandboxKind kind, Availability availability, std::string detail) {
    return ProviderStatus{kind, availability, std::move(detail)};
}

std::optional<std::string> env_var(const char* key) {
    const char* value = std::getenv(key);
    if (!value) return std::nullopt;
    return std::string(value);
}

Awaitable<Result<std::unique_ptr<Sandbox>>> started(Result<std::unique_ptr<ProcessSandbox>> sandbox) {
    if (!sandbox) co_return std::unexpected(sandbox.error());
    co_return std::unique_ptr<Sandbox>(std::move(*sandbox));
}

#if defined(_WIN32)
constexpr std::string_view OS = "windows";
#elif defined(__APPLE__)
constexpr std::string_view OS = "macos";
#else
constexpr std::string_view OS = "linux";
#endif

#if defined(__aarch64__)
constexpr std::string_view ARCH = "aarch64";
#else
constexpr std::string_view ARCH = "x86_64";
#endif

} // namespace

// ---- local ---------------------------------------------------------------------------------------

std::optional<fs::path> find_stub() {
    std::vector<fs::path> candidates;
    if (auto explicit_stub = env_var("LOA_SANDBOX_STUB")) candidates.emplace_back(*explicit_stub);
    std::error_code ec;
    const auto exe = fs::read_symlink("/proc/self/exe", ec);
    if (!ec) {
        auto dir = exe.parent_path();
        for (int i = 0; i < 5 && !dir.empty(); ++i) {
            candidates.push_back(dir / STUB_NAME);
            if (dir == dir.parent_path()) break;
            dir = dir.parent_path();
        }
    }
    for (const auto& candidate : candidates) {
        if (auto found = runtime::exec::which(candidate)) return found;
    }
    return runtime::exec::which(fs::path(STUB_NAME));
}

std::vector<std::pair<std::string, std::string>> system_env() {
    std::vector<std::pair<std::string, std::string>> env;
    if (!WINDOWS) return env;
    for (const char* key : {"SystemRoot", "windir", "ComSpec", "PATHEXT"}) {
        if (auto value = env_var(key)) env.emplace_back(key, *value);
    }
    if (auto root = env_var("SystemRoot")) {
        std::string sh;
        if (auto found = runtime::exec::which("sh")) sh = ";" + found->parent_path().string();
        env.emplace_back("PATH", std::format("{}\\System32;{}{}", *root, *root, sh));
    }
    return env;
}

std::string LocalProvider::describe() const {
    return "this machine, in the workspace or its isolated checkout; no OS level isolation";
}

Result<std::unique_ptr<ProcessSandbox>> LocalProvider::start_blocking(const SandboxSpec& spec) const {
    HostCommand cmd{stub_, {spec.workspace.string()}, spec.workspace, true, system_env()};
    cmd.env.insert(cmd.env.end(), spec.env.begin(), spec.env.end());
    return process("local", cmd, WorkspaceMode::Shared, nullptr);
}

Awaitable<Result<std::unique_ptr<Sandbox>>> LocalProvider::start(const SandboxSpec& spec) const {
    return started(start_blocking(spec));
}

Awaitable<ProviderStatus> LocalFactory::probe(const Machine& machine, const SandboxConfig&) const {
    if (machine.stub) {
        co_return status(kind(), Availability::Ready,
                         std::format("Runs on this machine without OS isolation; isolated tasks use a {} checkout", backend_name()));
    }
    co_return status(kind(), Availability::Unavailable, std::string(NO_STUB));
}

std::shared_ptr<SandboxProvider> LocalFactory::build(const Machine& machine, const SandboxConfig&) const {
    if (!machine.stub) return nullptr;
    return std::make_shared<LocalProvider>(*machine.stub);
}

// ---- docker --------------------------------------------------------------------------------------

namespace {

// Where the host stub is mounted, read only, inside the container.
constexpr std::string_view MOUNTED_STUB = "/opt/loa/loa-sandbox-stub";

// Variables of the host the Docker CLI itself needs to find its daemon and configuration.
// They reach the `docker` client process only, never the container.
constexpr const char* CLIENT_ENV[] = {
    "PATH", "HOME", "XDG_RUNTIME_DIR", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG", "DOCKER_CERT_PATH", "DOCKER_TLS_VERIFY",
    // Windows: where processes find the system, and the user profile that holds `.docker`.
    "SystemRoot", "windir", "ComSpec", "PATHEXT", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "ProgramData", "ProgramFiles",
};

// The `docker` client with only `CLIENT_ENV` from the host.
HostCommand docker(const fs::path& program, std::vector<std::string> args) {
    HostCommand cmd{program, std::move(args), std::nullopt, true, {}};
    for (const char* key : CLIENT_ENV) {
        if (auto value = env_var(key)) cmd.env.emplace_back(key, *value);
    }
    return cmd;
}

// Removes the container by name.
class RemoveContainer final : public Teardown {
  public:
    RemoveContainer(fs::path docker, std::string name) : docker_(std::move(docker)), name_(std::move(name)) {}
    HostCommand command() const override { return docker(docker_, {"rm", "-f", name_}); }
    // `--rm` may have removed it already when the stub exited.
    bool gone(std::string_view stderr_text) const override { return stderr_text.find("No such container") != std::string_view::npos; }

  private:
    fs::path docker_;
    std::string name_;
};

Result<std::string> server_version(const fs::path& program) {
    auto spec = docker(program, {"version", "--format", "{{.Server.Version}}"});
    detail::Spawn run{spec.program, spec.args, std::nullopt, true, spec.env};
    auto out = detail::run(run, {}, std::chrono::seconds(5));
    if (!out) {
        if (out.error().code() == ErrorCode::timeout) return std::unexpected(loa::Error(ErrorCode::timeout, "no answer within 5 s"));
        return std::unexpected(out.error());
    }
    if (!out->success()) {
        const auto err = detail::lossy_trim(out->err);
        return std::unexpected(loa::Error(ErrorCode::io, std::format("{}: {}", detail::status_string(out->status), err.substr(0, err.find('\n')))));
    }
    return detail::lossy_trim(out->out);
}

} // namespace

std::string DockerProvider::describe() const {
    return std::format("a Docker container (options.image, default {}; network {} unless options.network)", image_,
                       network_ ? "on" : "off");
}

HostCommand DockerProvider::run_command(const SandboxSpec& spec) const {
    const auto network = spec.options.network.value_or(network_) ? "bridge" : "none";
    auto cmd = docker(docker_, {"run", "--rm", "-i", "--name", spec.name, "--network", network, "--mount",
                                std::format("type=bind,src={},dst=/workspace", spec.workspace.string()), "--workdir", "/workspace"});
    cmd.args.insert(cmd.args.end(), {"--mount", std::format("type=bind,src={},dst={},readonly", stub_.string(), MOUNTED_STUB)});
    if (spec.options.memory_mib) cmd.args.insert(cmd.args.end(), {"--memory", std::format("{}m", *spec.options.memory_mib)});
    if (spec.options.cpus) cmd.args.insert(cmd.args.end(), {"--cpus", std::to_string(*spec.options.cpus)});
    for (const auto& [key, value] : spec.env) cmd.args.insert(cmd.args.end(), {"--env", key + "=" + value});
    const auto image = spec.options.image && !spec.options.image->empty() ? *spec.options.image : image_;
    cmd.args.insert(cmd.args.end(), {image, std::string(MOUNTED_STUB), "/workspace"});
    return cmd;
}

Awaitable<Result<std::unique_ptr<Sandbox>>> DockerProvider::start(const SandboxSpec& spec) const {
    return started(process("docker", run_command(spec), WorkspaceMode::Shared, std::make_unique<RemoveContainer>(docker_, spec.name)));
}

Awaitable<ProviderStatus> DockerFactory::probe(const Machine& machine, const SandboxConfig& config) const {
    if (WINDOWS) co_return status(kind(), Availability::Unavailable, std::string(REMOTE_STUB_UNSUPPORTED));
    const auto program = machine.which("docker");
    if (!program) co_return status(kind(), Availability::Unavailable, "Docker is not installed");
    if (!machine.stub) co_return status(kind(), Availability::Unavailable, std::string(NO_STUB));
    auto version = co_await detail::offload([program = *program] { return server_version(program); });
    if (!version) co_return status(kind(), Availability::Unavailable, "The Docker daemon is not reachable: " + version.error().message());
    co_return status(kind(), Availability::Ready, std::format("Docker {}, default image {}", *version, config.docker_image));
}

std::shared_ptr<SandboxProvider> DockerFactory::build(const Machine& machine, const SandboxConfig& config) const {
    if (WINDOWS) return nullptr;
    auto program = machine.which("docker");
    if (!program || !machine.stub) return nullptr;
    return std::make_shared<DockerProvider>(*program, *machine.stub, config.docker_image, config.docker_network);
}

// ---- ssh -----------------------------------------------------------------------------------------

namespace {

// Where runs get their private directories on the host.
constexpr std::string_view REMOTE_ROOT = "/tmp/loa-sandbox";

// Runs a remote command on one host without ever prompting: the app has no terminal to answer on.
class Remote final : public Teardown {
  public:
    Remote(fs::path ssh, std::string host, std::string script) : ssh_(std::move(ssh)), host_(std::move(host)), script_(std::move(script)) {}
    HostCommand command() const override { return HostCommand{ssh_, {"-T", "-o", "BatchMode=yes", "--", host_, script_}, std::nullopt, false, {}}; }

  private:
    fs::path ssh_;
    std::string host_;
    std::string script_;
};

} // namespace

std::string SshProvider::describe() const {
    if (hosts_.empty()) return "an SSH host; set options.host";
    if (hosts_.size() == 1) return std::format("the SSH host {} (options.host may name another)", hosts_.front());
    std::string joined;
    for (const auto& host : hosts_) joined += (joined.empty() ? "" : ", ") + host;
    return "an SSH host; set options.host to one of " + joined;
}

Result<std::unique_ptr<Sandbox>> SshProvider::start_blocking(const SandboxSpec& spec) const {
    std::string host;
    if (spec.options.host) host = *spec.options.host;
    else if (hosts_.size() == 1) host = hosts_.front();
    if (host.empty() || host.starts_with('-')) return std::unexpected(error::provider("ssh", "a valid host is required"));
    // The stub sits beside the workspace root, so copying out never brings it back.
    const auto dir = std::format("{}/{}", REMOTE_ROOT, spec.name);
    const auto q_dir = runtime::exec::shell_quote(dir);
    const auto q_root = runtime::exec::shell_quote(dir + "/workspace");
    const auto q_stub = runtime::exec::shell_quote(dir + "/loa-sandbox-stub");
    const auto remote = [&](std::string script) { return std::make_unique<Remote>(ssh_, host, std::move(script)); };
    auto cleanup = remote(std::format("rm -rf -- {}", q_dir));
    std::ifstream file(local_stub_, std::ios::binary);
    if (!file) return std::unexpected(error::os(local_stub_.string(), errno));
    const std::string binary{std::istreambuf_iterator<char>(file), std::istreambuf_iterator<char>()};
    // One round trip creates the private directory and receives the stub on stdin.
    const auto setup = remote(std::format("umask 077 && mkdir -p -- {} && cat > {} && chmod 700 -- {}", q_root, q_stub, q_stub))->command();
    auto upload = detail::run(detail::Spawn{setup.program, setup.args}, binary);
    if (!upload) return std::unexpected(upload.error());
    if (!upload->success()) {
        const auto teardown = cleanup->command();
        (void)detail::run(detail::Spawn{teardown.program, teardown.args});
        return std::unexpected(error::provider("ssh", std::format("stub upload failed ({}): {}", detail::status_string(upload->status),
                                                                  detail::lossy_trim(upload->err))));
    }
    auto run = remote(std::format("exec {} {}", q_stub, q_root))->command();
    auto sandbox = process("ssh", run, WorkspaceMode::Copied, std::move(cleanup));
    if (!sandbox) return std::unexpected(sandbox.error());
    return std::unique_ptr<Sandbox>(std::move(*sandbox));
}

Awaitable<Result<std::unique_ptr<Sandbox>>> SshProvider::start(const SandboxSpec& spec) const {
    co_return co_await detail::offload([this, spec] { return start_blocking(spec); });
}

std::vector<std::string> SshFactory::hosts(const SandboxConfig& config) {
    if (!config.ssh_hosts.empty()) return config.ssh_hosts;
    if (auto home = runtime::fs::home()) return runtime::ssh::aliases(*home / ".ssh/config");
    return {};
}

Awaitable<ProviderStatus> SshFactory::probe(const Machine& machine, const SandboxConfig& config) const {
    if (WINDOWS) co_return status(kind(), Availability::Unavailable, std::string(REMOTE_STUB_UNSUPPORTED));
    if (!machine.which("ssh")) co_return status(kind(), Availability::Unavailable, "No ssh client is installed");
    if (!machine.stub) co_return status(kind(), Availability::Unavailable, std::string(NO_STUB));
    const auto list = hosts(config);
    if (list.empty()) co_return status(kind(), Availability::NotConfigured, "Add a host to ~/.ssh/config or the sandbox settings");
    std::string joined;
    for (const auto& host : list) joined += (joined.empty() ? "" : ", ") + host;
    co_return status(kind(), Availability::Ready, std::format("{}; hosts must run {} on {}", joined, OS, ARCH));
}

std::shared_ptr<SandboxProvider> SshFactory::build(const Machine& machine, const SandboxConfig& config) const {
    if (WINDOWS) return nullptr;
    auto ssh = machine.which("ssh");
    if (!ssh || !machine.stub) return nullptr;
    return std::make_shared<SshProvider>(*ssh, hosts(config), *machine.stub);
}

// ---- peer ----------------------------------------------------------------------------------------

namespace {
class PeerSandbox final : public Sandbox {
  public:
    explicit PeerSandbox(Channel channel) : channel_(std::move(channel)) {}
    std::optional<Channel> take_channel() override { return std::exchange(channel_, std::nullopt); }
    WorkspaceMode workspace_mode() const override { return WorkspaceMode::Copied; }
    Awaitable<Result<>> stop() override { co_return Result<>{}; }

  private:
    std::optional<Channel> channel_;
};
} // namespace

std::string PeerProvider::describe() const {
    std::string joined;
    for (const auto& peer : link_->peers()) {
        auto name = link_->name(peer);
        joined += (joined.empty() ? "" : ", ") + (name ? std::format("{} ({})", peer, *name) : peer);
    }
    if (joined.empty()) return "a peer node that granted sandbox access (none right now); set options.peer";
    return "a peer node; set options.peer to one of " + joined;
}

Awaitable<Result<std::unique_ptr<Sandbox>>> PeerProvider::start(const SandboxSpec& spec) const {
    if (!spec.options.peer) co_return std::unexpected(error::provider("peer", "peer is required"));
    const auto peer = *spec.options.peer;
    const auto known = link_->peers();
    if (std::ranges::find(known, peer) == known.end()) co_return std::unexpected(error::provider("peer", "no sandbox grant from peer " + peer));
    auto channel = co_await link_->open(peer);
    if (!channel) co_return std::unexpected(channel.error());
    co_return std::unique_ptr<Sandbox>(std::make_unique<PeerSandbox>(std::move(*channel)));
}

Result<> serve_stub_blocking(Channel channel, const fs::path& stub) {
    auto root = runtime::TempDir::create();
    if (!root) return std::unexpected(root.error());
    HostCommand cmd{stub, {root->path().string()}, root->path(), true, system_env()};
    auto sandbox = process("peer", cmd, WorkspaceMode::Copied, nullptr);
    if (!sandbox) return std::unexpected(sandbox.error());
    auto stub_channel = (*sandbox)->take_channel();
    // Closing the stub's stdin ends it, which ends the outbound copy.
    Result<> inbound, outbound;
    std::jthread in([&] {
        std::string buf(64 * 1024, '\0');
        for (;;) {
            auto n = channel.reader->read(buf);
            if (!n) {
                inbound = std::unexpected(n.error());
                break;
            }
            if (*n == 0) break;
            if (!stub_channel->writer->write_all(std::string_view(buf).substr(0, *n))) break;
        }
        (void)stub_channel->writer->shutdown();
    });
    {
        std::string buf(64 * 1024, '\0');
        for (;;) {
            auto n = stub_channel->reader->read(buf);
            if (!n) {
                outbound = std::unexpected(n.error());
                break;
            }
            if (*n == 0) break;
            if (auto w = channel.writer->write_all(std::string_view(buf).substr(0, *n)); !w) {
                outbound = std::unexpected(w.error());
                break;
            }
        }
        (void)channel.writer->shutdown();
    }
    // The stub ended (its output closed); stop it so the inbound copy's stream may close too.
    auto stopped = (*sandbox)->stop_blocking();
    in.join();
    if (!stopped) return stopped;
    if (!inbound) return inbound;
    return outbound;
}

Awaitable<Result<>> serve_stub(Channel channel, fs::path stub) {
    auto shared = std::make_shared<Channel>(std::move(channel));
    co_return co_await detail::offload([shared, stub = std::move(stub)] { return serve_stub_blocking(std::move(*shared), stub); });
}

Awaitable<ProviderStatus> PeerFactory::probe(const Machine& machine, const SandboxConfig&) const {
    // The user knows peers by name; the key stands in for an unnamed one.
    if (!machine.peers || !machine.peers->running()) co_return status(kind(), Availability::Unavailable, "Peer networking is not running");
    std::string joined;
    for (const auto& peer : machine.peers->peers()) joined += (joined.empty() ? "" : ", ") + machine.peers->name(peer).value_or(peer);
    if (joined.empty()) co_return status(kind(), Availability::NotConfigured, "No peer has granted this device sandbox access");
    co_return status(kind(), Availability::Ready, std::move(joined));
}

std::shared_ptr<SandboxProvider> PeerFactory::build(const Machine& machine, const SandboxConfig&) const {
    if (!machine.peers || !machine.peers->running()) return nullptr;
    return std::make_shared<PeerProvider>(machine.peers);
}

} // namespace loa::sandbox
