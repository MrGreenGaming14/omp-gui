#pragma once

// Sandbox providers (Rust `loa_sandbox::provider`): each starts a place to work, runs the exec
// stub there and hands back a byte channel to it. A `ProviderFactory` per kind probes this
// machine and builds its provider from the settings; the `ProviderRegistry` holds the factories.

#include <cstdint>
#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/registry.hpp"
#include "loa/sandbox/channel.hpp"

namespace loa::sandbox {

struct SandboxConfig;

namespace detail {
class Process;
}

// The kinds of sandbox `sandbox_task` can ask for. `Local` is a child process on the host
// without isolation of its own; `Vm` is qemu. Ordered as declared.
enum class SandboxKind : std::uint8_t { Local, Docker, Firecracker, Vm, Ssh, Peer };

inline constexpr SandboxKind SANDBOX_KINDS[] = {SandboxKind::Local, SandboxKind::Docker, SandboxKind::Firecracker,
                                                SandboxKind::Vm,    SandboxKind::Ssh,    SandboxKind::Peer};

// "local", "docker", "firecracker", "vm", "ssh", "peer".
[[nodiscard]] std::string_view as_str(SandboxKind kind) noexcept;
[[nodiscard]] std::optional<SandboxKind> parse_kind(std::string_view name) noexcept;

// Per call options from the `sandbox_task` arguments. Each provider reads the ones that apply
// to it and falls back to its own configuration.
struct SandboxOptions {
    // Container image (docker).
    std::optional<std::string> image;
    // SSH host alias or `user@host` (ssh).
    std::optional<std::string> host;
    // Node key or name of the peer (peer).
    std::optional<std::string> peer;
    // Allow network access from inside the sandbox (docker, firecracker, vm).
    std::optional<bool> network;
    std::optional<std::uint32_t> memory_mib;
    std::optional<std::uint32_t> cpus;

    // serde: `#[serde(default, deny_unknown_fields)]`.
    [[nodiscard]] static std::expected<SandboxOptions, std::string> from_json(const runtime::json::Value& json);
    // The schemars schema.
    [[nodiscard]] static runtime::json::Value schema();
    friend bool operator==(const SandboxOptions&, const SandboxOptions&) = default;
};

// What a provider needs to start one sandbox.
struct SandboxSpec {
    // Unique per run: names containers and temp files.
    std::string name;
    // Host directory the worker works on.
    std::filesystem::path workspace;
    // The whole environment the stub (and every command it runs) gets.
    std::vector<std::pair<std::string, std::string>> env;
    SandboxOptions options;
};

// How the workspace reaches the stub's root directory.
enum class WorkspaceMode {
    // The stub's root is the host workspace itself (local) or a mount of it (docker).
    Shared,
    // The stub's root starts empty; `sync` copies the workspace in and out.
    Copied,
};

// One running sandbox with the stub started in it.
class Sandbox {
  public:
    virtual ~Sandbox() = default;
    // The channel to the stub. None after the first call.
    [[nodiscard]] virtual std::optional<Channel> take_channel() = 0;
    [[nodiscard]] virtual WorkspaceMode workspace_mode() const = 0;
    // Stops the sandbox and removes what the provider created for it. Destroying a sandbox
    // without calling this cleans up on a best effort basis.
    [[nodiscard]] virtual contracts::Awaitable<Result<>> stop() = 0;
};

// Starts sandboxes of one kind.
class SandboxProvider {
  public:
    virtual ~SandboxProvider() = default;
    [[nodiscard]] virtual SandboxKind kind() const = 0;
    // What this sandbox is and which options it takes, for the agent.
    [[nodiscard]] virtual std::string describe() const = 0;
    [[nodiscard]] virtual contracts::Awaitable<Result<std::unique_ptr<Sandbox>>> start(const SandboxSpec& spec) const = 0;
};

// A host command line: program and arguments, with `clear_env` + `env` for its environment.
struct HostCommand {
    std::filesystem::path program;
    std::vector<std::string> args;
    std::optional<std::filesystem::path> cwd;
    bool clear_env = false;
    std::vector<std::pair<std::string, std::string>> env;
};

// What a sandbox leaves behind outside its process, and the command that removes it: awaited
// by `stop`, and started detached when the sandbox is destroyed.
class Teardown {
  public:
    virtual ~Teardown() = default;
    [[nodiscard]] virtual HostCommand command() const = 0;
    // A failure that only means there was nothing left to remove.
    [[nodiscard]] virtual bool gone(std::string_view /*stderr*/) const { return false; }
};

// A stub talking over the stdio of a host process, with an optional teardown.
class ProcessSandbox final : public Sandbox {
  public:
    ProcessSandbox(std::unique_ptr<detail::Process> child, Channel channel, WorkspaceMode mode,
                   std::unique_ptr<Teardown> teardown, std::string_view provider);
    ~ProcessSandbox() override;

    [[nodiscard]] std::optional<Channel> take_channel() override;
    [[nodiscard]] WorkspaceMode workspace_mode() const override { return mode_; }
    [[nodiscard]] contracts::Awaitable<Result<>> stop() override;
    // `stop` on the calling thread.
    [[nodiscard]] Result<> stop_blocking();

  private:
    std::unique_ptr<detail::Process> child_;
    std::optional<Channel> channel_;
    WorkspaceMode mode_;
    std::unique_ptr<Teardown> teardown_;
    std::string_view provider_;
};

// Spawns `command` with the stub on its stdio (stderr discarded).
[[nodiscard]] Result<std::unique_ptr<ProcessSandbox>> process(std::string_view provider, const HostCommand& command,
                                                              WorkspaceMode mode, std::unique_ptr<Teardown> teardown);

// Authenticated streams to peers that granted this node sandbox access.
class PeerLink {
  public:
    virtual ~PeerLink() = default;
    // Whether peer networking runs now; peers can't be reached otherwise.
    [[nodiscard]] virtual bool running() const { return true; }
    // Names or keys of the peers that can run a sandbox now.
    [[nodiscard]] virtual std::vector<std::string> peers() const = 0;
    // What the user calls `peer`, when it has a name besides its key.
    [[nodiscard]] virtual std::optional<std::string> name(std::string_view /*peer*/) const { return std::nullopt; }
    // Opens a stream to the peer's `serve_stub`.
    [[nodiscard]] virtual contracts::Awaitable<Result<Channel>> open(std::string peer) const = 0;
};

// What the providers find on this machine besides its programs.
struct Machine {
    // The stub binary, if one was found.
    std::optional<std::filesystem::path> stub;
    std::shared_ptr<PeerLink> peers;

    [[nodiscard]] static Machine self();
    [[nodiscard]] std::optional<std::filesystem::path> which(std::string_view name) const;
    [[nodiscard]] bool kvm() const;
};

// Whether a provider can be used on this machine now.
enum class Availability {
    Ready,
    // Installed, but it needs settings (an image, a host, a grant).
    NotConfigured,
    // Missing software, hardware or permission.
    Unavailable,
};

// One provider's availability with the reason, as the Tools panel shows it.
struct ProviderStatus {
    SandboxKind kind;
    Availability availability;
    std::string detail;
    friend bool operator==(const ProviderStatus&, const ProviderStatus&) = default;
};

inline constexpr std::string_view NO_STUB =
    "loa-sandbox-stub was not found next to the app or on PATH (set LOA_SANDBOX_STUB)";
// Why a Windows host offers neither Docker nor SSH: they run the stub inside a Linux guest, and
// the stub the host has is a Windows binary.
inline constexpr std::string_view REMOTE_STUB_UNSUPPORTED = "the sandbox stub on Windows runs only on the local provider";

// Probes one kind of provider and builds it from the settings.
class ProviderFactory {
  public:
    virtual ~ProviderFactory() = default;
    [[nodiscard]] virtual SandboxKind kind() const = 0;
    // Whether the provider can run here, and why not.
    [[nodiscard]] virtual contracts::Awaitable<ProviderStatus> probe(const Machine& machine, const SandboxConfig& config) const = 0;
    // The provider, when this machine has the programs and settings for it. It can still fail
    // at run time (a stopped daemon, an unreachable host).
    [[nodiscard]] virtual std::shared_ptr<SandboxProvider> build(const Machine& machine, const SandboxConfig& config) const = 0;
};

// The provider factories by kind.
class ProviderRegistry {
  public:
    // Every built in provider this build has.
    [[nodiscard]] static ProviderRegistry builtin();
    // Adds `factory`, replacing one of the same kind (Rust `register`).
    ProviderRegistry& add(std::shared_ptr<ProviderFactory> factory);
    // The factories in `SandboxKind` order, as the app lists them.
    [[nodiscard]] std::vector<std::shared_ptr<ProviderFactory>> factories() const;

  private:
    runtime::Registry<ProviderFactory> registry_;
};

} // namespace loa::sandbox
