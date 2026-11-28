#pragma once

// What the app holds: the sandbox settings, which providers this machine can run (and why not),
// and the `sandbox_task` host tool for each new session (Rust `loa_sandbox::service`).

#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/runtime/json.hpp"
#include "loa/sandbox/provider.hpp"
#include "loa/sandbox/tool.hpp"

namespace loa::sandbox {

class WorkerJournal;

// Sandbox settings, kept by the app as JSON with the Rust field names (`#[serde(default)]`).
struct SandboxConfig {
    // Image used when a docker call names none. The host stub is mounted into it, so it needs a
    // C library compatible with the host's.
    std::string docker_image = "debian:stable-slim";
    bool docker_network = false;
    // Guest kernel and root filesystem for Firecracker. The rootfs init must put ttyS0 in raw
    // mode and run `loa-sandbox-stub /workspace` on it.
    std::optional<std::filesystem::path> firecracker_kernel;
    std::optional<std::filesystem::path> firecracker_rootfs;
    // Raw disk image for qemu, whose init runs the stub on the first serial port.
    std::optional<std::filesystem::path> vm_disk;
    std::optional<std::filesystem::path> vm_kernel;
    // SSH hosts the agent may use. Empty: the hosts of `~/.ssh/config`.
    std::vector<std::string> ssh_hosts;

    // serde_json form, fields in declaration order, `null` for unset paths.
    [[nodiscard]] runtime::json::Value to_json() const;
    // Missing fields take their defaults; unknown fields are ignored.
    [[nodiscard]] static Result<SandboxConfig> from_json(const runtime::json::Value& json);
    // Reads the settings; a missing file gives the defaults.
    [[nodiscard]] static Result<SandboxConfig> load(const std::filesystem::path& path);
    [[nodiscard]] Result<> save(const std::filesystem::path& path) const;

    friend bool operator==(const SandboxConfig&, const SandboxConfig&) = default;
};

// The app's sandbox service: settings, provider availability and the `sandbox_task` host tool
// for sessions.
class SandboxService {
  public:
    // `journal` keeps the worker sessions on the host.
    SandboxService(SandboxConfig config, std::shared_ptr<const WorkerJournal> journal);

    // Uses this stub binary (none: there is none).
    SandboxService& with_stub(std::optional<std::filesystem::path> stub);
    // Enables the peer provider.
    SandboxService& with_peers(std::shared_ptr<PeerLink> peers);

    [[nodiscard]] const SandboxConfig& config() const noexcept { return config_; }
    // New settings apply to sessions started afterwards.
    void set_config(SandboxConfig config) { config_ = std::move(config); }
    // The stub binary, for the peer side (`serve_stub`).
    [[nodiscard]] const std::optional<std::filesystem::path>& stub() const noexcept { return machine_.stub; }
    // Every `sandbox_task` run of every session.
    [[nodiscard]] std::shared_ptr<SandboxEvents::Receiver> subscribe() const { return events_->subscribe(); }

    // The providers this machine has the programs and settings for.
    [[nodiscard]] std::vector<std::shared_ptr<SandboxProvider>> providers() const;

    // The `sandbox_task` host tool for a new session, whose worker runs on `adapter` with
    // `templ`'s settings in `templ.cwd`. None when no provider can run here.
    [[nodiscard]] std::optional<std::pair<contracts::HostToolSpec, std::shared_ptr<const contracts::HostTools>>> host_tool(
        std::shared_ptr<const contracts::Adapter> adapter, const contracts::SessionConfig& templ) const;

    // Checks every provider kind on this machine. Copies what it needs, so it can run as a
    // background task.
    [[nodiscard]] contracts::Awaitable<std::vector<ProviderStatus>> probe() const;

  private:
    SandboxConfig config_;
    std::shared_ptr<const WorkerJournal> journal_;
    std::shared_ptr<SandboxEvents> events_ = std::make_shared<SandboxEvents>();
    Machine machine_;
    ProviderRegistry registry_;
};

} // namespace loa::sandbox
