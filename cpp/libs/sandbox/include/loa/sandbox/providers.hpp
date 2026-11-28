#pragma once

// The built in providers: local, docker, ssh, peer and, with `LOA_SANDBOX_VM`, firecracker and qemu.

#include "loa/sandbox/provider.hpp"

namespace loa::sandbox {

// ---- local: the stub is a child in the host workspace ----------------------------------------

// File name of the stub binary.
inline constexpr std::string_view STUB_NAME = "loa-sandbox-stub";

// Finds the stub: `LOA_SANDBOX_STUB`, then next to the running executable or up to five
// directories above it, then `PATH`.
[[nodiscard]] std::optional<std::filesystem::path> find_stub();

// What Windows processes need to start at all; empty on Unix.
[[nodiscard]] std::vector<std::pair<std::string, std::string>> system_env();

class LocalProvider final : public SandboxProvider {
  public:
    explicit LocalProvider(std::filesystem::path stub) : stub_(std::move(stub)) {}
    [[nodiscard]] SandboxKind kind() const override { return SandboxKind::Local; }
    [[nodiscard]] std::string describe() const override;
    [[nodiscard]] contracts::Awaitable<Result<std::unique_ptr<Sandbox>>> start(const SandboxSpec& spec) const override;
    // `start` on the calling thread.
    [[nodiscard]] Result<std::unique_ptr<ProcessSandbox>> start_blocking(const SandboxSpec& spec) const;

  private:
    std::filesystem::path stub_;
};

class LocalFactory final : public ProviderFactory {
  public:
    [[nodiscard]] SandboxKind kind() const override { return SandboxKind::Local; }
    [[nodiscard]] contracts::Awaitable<ProviderStatus> probe(const Machine& machine, const SandboxConfig& config) const override;
    [[nodiscard]] std::shared_ptr<SandboxProvider> build(const Machine& machine, const SandboxConfig& config) const override;
};

// ---- docker: mounts the host workspace and keeps the stub on stdio ---------------------------

class DockerProvider final : public SandboxProvider {
  public:
    DockerProvider(std::filesystem::path docker, std::filesystem::path stub, std::string image, bool network)
        : docker_(std::move(docker)), stub_(std::move(stub)), image_(std::move(image)), network_(network) {}
    [[nodiscard]] SandboxKind kind() const override { return SandboxKind::Docker; }
    [[nodiscard]] std::string describe() const override;
    [[nodiscard]] contracts::Awaitable<Result<std::unique_ptr<Sandbox>>> start(const SandboxSpec& spec) const override;
    // The `docker run` command `start` spawns.
    [[nodiscard]] HostCommand run_command(const SandboxSpec& spec) const;

  private:
    std::filesystem::path docker_;
    std::filesystem::path stub_;
    std::string image_;
    bool network_;
};

class DockerFactory final : public ProviderFactory {
  public:
    [[nodiscard]] SandboxKind kind() const override { return SandboxKind::Docker; }
    [[nodiscard]] contracts::Awaitable<ProviderStatus> probe(const Machine& machine, const SandboxConfig& config) const override;
    [[nodiscard]] std::shared_ptr<SandboxProvider> build(const Machine& machine, const SandboxConfig& config) const override;
};

// ---- ssh: the host's `ssh` client runs the uploaded stub on an SSH host ----------------------

class SshProvider final : public SandboxProvider {
  public:
    SshProvider(std::filesystem::path ssh, std::vector<std::string> hosts, std::filesystem::path local_stub)
        : ssh_(std::move(ssh)), hosts_(std::move(hosts)), local_stub_(std::move(local_stub)) {}
    [[nodiscard]] SandboxKind kind() const override { return SandboxKind::Ssh; }
    [[nodiscard]] std::string describe() const override;
    [[nodiscard]] contracts::Awaitable<Result<std::unique_ptr<Sandbox>>> start(const SandboxSpec& spec) const override;

  private:
    Result<std::unique_ptr<Sandbox>> start_blocking(const SandboxSpec& spec) const;

    std::filesystem::path ssh_;
    // Hosts the agent may pick from. With exactly one, calls may leave `options.host` out.
    std::vector<std::string> hosts_;
    // The stub binary to upload. It must run on the remote host's OS and CPU.
    std::filesystem::path local_stub_;
};

class SshFactory final : public ProviderFactory {
  public:
    [[nodiscard]] SandboxKind kind() const override { return SandboxKind::Ssh; }
    [[nodiscard]] contracts::Awaitable<ProviderStatus> probe(const Machine& machine, const SandboxConfig& config) const override;
    [[nodiscard]] std::shared_ptr<SandboxProvider> build(const Machine& machine, const SandboxConfig& config) const override;
    // The hosts of the settings, else the aliases of `~/.ssh/config`.
    [[nodiscard]] static std::vector<std::string> hosts(const SandboxConfig& config);
};

// ---- peer: the stub runs on a peer node over a `PeerLink` stream -----------------------------

// Protocol name for peer sandbox streams on the node's router.
inline constexpr std::string_view PEER_PROTOCOL = "loa-sandbox";

class PeerProvider final : public SandboxProvider {
  public:
    explicit PeerProvider(std::shared_ptr<PeerLink> link) : link_(std::move(link)) {}
    [[nodiscard]] SandboxKind kind() const override { return SandboxKind::Peer; }
    [[nodiscard]] std::string describe() const override;
    [[nodiscard]] contracts::Awaitable<Result<std::unique_ptr<Sandbox>>> start(const SandboxSpec& spec) const override;

  private:
    std::shared_ptr<PeerLink> link_;
};

class PeerFactory final : public ProviderFactory {
  public:
    [[nodiscard]] SandboxKind kind() const override { return SandboxKind::Peer; }
    [[nodiscard]] contracts::Awaitable<ProviderStatus> probe(const Machine& machine, const SandboxConfig& config) const override;
    [[nodiscard]] std::shared_ptr<SandboxProvider> build(const Machine& machine, const SandboxConfig& config) const override;
};

// The peer side of a sandbox stream: runs `stub` with an empty environment in a new temporary
// directory and pipes the stream to it until either side ends. The directory is removed afterwards.
[[nodiscard]] contracts::Awaitable<Result<>> serve_stub(Channel channel, std::filesystem::path stub);
[[nodiscard]] Result<> serve_stub_blocking(Channel channel, const std::filesystem::path& stub);

#ifdef LOA_SANDBOX_VM
// ---- vm: Firecracker (Linux) through its API socket, and qemu --------------------------------

// `qemu-system-aarch64` on arm64 hosts, else `qemu-system-x86_64`.
[[nodiscard]] std::string_view qemu_program() noexcept;

#if defined(__linux__)
class FirecrackerProvider final : public SandboxProvider {
  public:
    FirecrackerProvider(std::filesystem::path firecracker, std::filesystem::path kernel, std::filesystem::path rootfs,
                        std::uint32_t memory_mib = 512, std::uint32_t cpus = 1)
        : firecracker_(std::move(firecracker)), kernel_(std::move(kernel)), rootfs_(std::move(rootfs)),
          memory_mib_(memory_mib), cpus_(cpus) {}
    [[nodiscard]] SandboxKind kind() const override { return SandboxKind::Firecracker; }
    [[nodiscard]] std::string describe() const override;
    [[nodiscard]] contracts::Awaitable<Result<std::unique_ptr<Sandbox>>> start(const SandboxSpec& spec) const override;

  private:
    Result<std::unique_ptr<Sandbox>> start_blocking(const SandboxSpec& spec) const;

    std::filesystem::path firecracker_;
    std::filesystem::path kernel_;
    std::filesystem::path rootfs_;
    std::uint32_t memory_mib_;
    std::uint32_t cpus_;
};
#endif

class QemuProvider final : public SandboxProvider {
  public:
    QemuProvider(std::filesystem::path qemu, std::filesystem::path disk, std::optional<std::filesystem::path> kernel,
                 bool kvm, std::uint32_t memory_mib = 512, std::uint32_t cpus = 1)
        : qemu_(std::move(qemu)), disk_(std::move(disk)), kernel_(std::move(kernel)), kvm_(kvm), memory_mib_(memory_mib),
          cpus_(cpus) {}
    [[nodiscard]] SandboxKind kind() const override { return SandboxKind::Vm; }
    [[nodiscard]] std::string describe() const override;
    [[nodiscard]] contracts::Awaitable<Result<std::unique_ptr<Sandbox>>> start(const SandboxSpec& spec) const override;
    // The qemu command `start` spawns.
    [[nodiscard]] HostCommand run_command(const SandboxSpec& spec) const;

  private:
    std::filesystem::path qemu_;
    // Raw guest disk. Runs boot it with `-snapshot`, so it never changes.
    std::filesystem::path disk_;
    // Booted with `console=ttyS0 root=/dev/vda rw` when set.
    std::optional<std::filesystem::path> kernel_;
    bool kvm_;
    std::uint32_t memory_mib_;
    std::uint32_t cpus_;
};

class FirecrackerFactory final : public ProviderFactory {
  public:
    [[nodiscard]] SandboxKind kind() const override { return SandboxKind::Firecracker; }
    [[nodiscard]] contracts::Awaitable<ProviderStatus> probe(const Machine& machine, const SandboxConfig& config) const override;
    [[nodiscard]] std::shared_ptr<SandboxProvider> build(const Machine& machine, const SandboxConfig& config) const override;
};

class QemuFactory final : public ProviderFactory {
  public:
    [[nodiscard]] SandboxKind kind() const override { return SandboxKind::Vm; }
    [[nodiscard]] contracts::Awaitable<ProviderStatus> probe(const Machine& machine, const SandboxConfig& config) const override;
    [[nodiscard]] std::shared_ptr<SandboxProvider> build(const Machine& machine, const SandboxConfig& config) const override;
};
#endif

} // namespace loa::sandbox
