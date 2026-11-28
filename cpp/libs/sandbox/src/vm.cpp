// VM providers (CMake option `LOA_SANDBOX_VM`): Firecracker through its API socket (Linux only),
// and a generic qemu VM. The guest init must run the stub on the first serial port.

#include "loa/sandbox/providers.hpp"

#ifdef LOA_SANDBOX_VM

#include <asio.hpp>

#include <stdlib.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include <chrono>
#include <format>
#include <thread>

#include "loa/runtime/json.hpp"
#include "loa/sandbox/service.hpp"
#include "offload.hpp"
#include "process.hpp"

namespace loa::sandbox {

namespace fs = std::filesystem;
namespace json = runtime::json;
using contracts::Awaitable;

std::string_view qemu_program() noexcept {
#if defined(__aarch64__)
    return "qemu-system-aarch64";
#else
    return "qemu-system-x86_64";
#endif
}

namespace {
ProviderStatus status(SandboxKind kind, Availability availability, std::string detail) {
    return ProviderStatus{kind, availability, std::move(detail)};
}
} // namespace

#if defined(__linux__)

namespace {

loa::Error fc_error(std::string message) { return error::provider("firecracker", std::move(message)); }

Result<> api(const fs::path& socket_path, std::string_view path, const json::Value& body_json) {
    const int fd = ::socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) return std::unexpected(error::os("socket", errno));
    FdReader reader(fd);
    sockaddr_un addr{};
    addr.sun_family = AF_UNIX;
    const auto name = socket_path.string();
    if (name.size() >= sizeof addr.sun_path) return std::unexpected(fc_error("API socket path is too long"));
    std::copy(name.begin(), name.end(), addr.sun_path);
    if (::connect(fd, reinterpret_cast<const sockaddr*>(&addr), sizeof addr) < 0) return std::unexpected(error::os(name, errno));
    const auto body = json::dump(body_json);
    const auto request = std::format(
        "PUT {} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", path,
        body.size(), body);
    for (std::string_view rest = request; !rest.empty();) {
        const auto n = ::send(fd, rest.data(), rest.size(), MSG_NOSIGNAL);
        if (n < 0) {
            if (errno == EINTR) continue;
            return std::unexpected(error::os(name, errno));
        }
        rest.remove_prefix(static_cast<std::size_t>(n));
    }
    std::string response;
    char buf[4096];
    for (;;) {
        auto n = reader.read(buf);
        if (!n) return std::unexpected(n.error());
        if (*n == 0) break;
        response.append(buf, *n);
    }
    if (response.starts_with("HTTP/1.1 2") || response.starts_with("HTTP/1.0 2")) return {};
    return std::unexpected(fc_error(std::format("{}: {}", path, response)));
}

// Removes the run directory with the API socket and the private rootfs copy.
class RemoveDir final : public Teardown {
  public:
    explicit RemoveDir(fs::path dir) : dir_(std::move(dir)) {}
    HostCommand command() const override { return HostCommand{"rm", {"-rf", "--", dir_.string()}, std::nullopt, false, {}}; }

  private:
    fs::path dir_;
};

} // namespace

std::string FirecrackerProvider::describe() const {
    return "a Firecracker microVM without network; the workspace is copied in and out";
}

Result<std::unique_ptr<Sandbox>> FirecrackerProvider::start_blocking(const SandboxSpec& spec) const {
    if (spec.options.network == true) return std::unexpected(fc_error("network access requires a configured guest TAP device"));
    // Runs never change the configured image and leave nothing behind.
    auto pattern = (fs::temp_directory_path() / "loa-fc-XXXXXX").string();
    if (!::mkdtemp(pattern.data())) return std::unexpected(error::os("mkdtemp", errno));
    const fs::path run_dir(pattern);
    const auto socket = run_dir / "api.sock", rootfs = run_dir / "rootfs";
    auto teardown = std::make_unique<RemoveDir>(run_dir);
    std::error_code ec;
    fs::copy_file(rootfs_, rootfs, ec);
    if (ec) return std::unexpected(fc_error(std::format("copying {}: {}", rootfs_.string(), error::os("", ec.value()).message())));
    auto sandbox = process("firecracker", HostCommand{firecracker_, {"--api-sock", socket.string()}}, WorkspaceMode::Copied, std::move(teardown));
    if (!sandbox) return std::unexpected(sandbox.error());
    for (int i = 0; i < 100 && !fs::exists(socket, ec); ++i) std::this_thread::sleep_for(std::chrono::milliseconds(50));
    const auto cpus = spec.options.cpus.value_or(cpus_);
    const auto memory = spec.options.memory_mib.value_or(memory_mib_);
    if (auto r = api(socket, "/machine-config", json::Object{{"vcpu_count", std::uint64_t{cpus}}, {"mem_size_mib", std::uint64_t{memory}}}); !r)
        return std::unexpected(r.error());
    if (auto r = api(socket, "/boot-source",
                     json::Object{{"kernel_image_path", kernel_.string()}, {"boot_args", "console=ttyS0 reboot=k panic=1 pci=off"}});
        !r)
        return std::unexpected(r.error());
    if (auto r = api(socket, "/drives/rootfs",
                     json::Object{{"drive_id", "rootfs"}, {"path_on_host", rootfs.string()}, {"is_root_device", true}, {"is_read_only", false}});
        !r)
        return std::unexpected(r.error());
    if (auto r = api(socket, "/actions", json::Object{{"action_type", "InstanceStart"}}); !r) return std::unexpected(r.error());
    return std::unique_ptr<Sandbox>(std::move(*sandbox));
}

Awaitable<Result<std::unique_ptr<Sandbox>>> FirecrackerProvider::start(const SandboxSpec& spec) const {
    co_return co_await detail::offload([this, spec] { return start_blocking(spec); });
}

#endif

std::string QemuProvider::describe() const {
    return "a qemu virtual machine (options.network to allow network); the workspace is copied in and out";
}

HostCommand QemuProvider::run_command(const SandboxSpec& spec) const {
    HostCommand cmd{qemu_,
                    {"-display", "none", "-monitor", "none", "-serial", "stdio", "-snapshot", "-m",
                     std::to_string(spec.options.memory_mib.value_or(memory_mib_)), "-smp", std::to_string(spec.options.cpus.value_or(cpus_)),
                     "-drive", std::format("file={},format=raw,if=virtio", disk_.string()), "-nic",
                     spec.options.network == true ? "user,model=virtio-net-pci" : "none"}};
    if (kvm_) cmd.args.insert(cmd.args.end(), {"-accel", "kvm"});
    if (kernel_) cmd.args.insert(cmd.args.end(), {"-kernel", kernel_->string(), "-append", "console=ttyS0 root=/dev/vda rw"});
    return cmd;
}

Awaitable<Result<std::unique_ptr<Sandbox>>> QemuProvider::start(const SandboxSpec& spec) const {
    auto sandbox = process("vm", run_command(spec), WorkspaceMode::Copied, nullptr);
    if (!sandbox) co_return std::unexpected(sandbox.error());
    co_return std::unique_ptr<Sandbox>(std::move(*sandbox));
}

Awaitable<ProviderStatus> FirecrackerFactory::probe(const Machine& m, const SandboxConfig& c) const {
#if !defined(__linux__)
    co_return status(kind(), Availability::Unavailable, "Firecracker needs Linux (KVM)");
#else
    if (!m.which("firecracker")) co_return status(kind(), Availability::Unavailable, "Firecracker is not installed");
    if (!m.kvm()) co_return status(kind(), Availability::Unavailable, "/dev/kvm is not accessible");
    if (c.firecracker_kernel && c.firecracker_rootfs) {
        co_return status(kind(), Availability::Ready,
                         std::format("Kernel {}, rootfs {}", c.firecracker_kernel->string(), c.firecracker_rootfs->string()));
    }
    co_return status(kind(), Availability::NotConfigured, "Set a guest kernel and a rootfs whose init runs loa-sandbox-stub on ttyS0");
#endif
}

std::shared_ptr<SandboxProvider> FirecrackerFactory::build(const Machine& m, const SandboxConfig& c) const {
#if defined(__linux__)
    if (!c.firecracker_kernel || !c.firecracker_rootfs) return nullptr;
    auto firecracker = m.which("firecracker");
    if (!firecracker) return nullptr;
    return std::make_shared<FirecrackerProvider>(*firecracker, *c.firecracker_kernel, *c.firecracker_rootfs);
#else
    (void)m, (void)c;
    return nullptr;
#endif
}

Awaitable<ProviderStatus> QemuFactory::probe(const Machine& m, const SandboxConfig& c) const {
    const auto speed = m.kvm() ? "KVM accelerated" : "without KVM (slow)";
    if (!m.which(qemu_program())) co_return status(kind(), Availability::Unavailable, std::format("{} is not installed", qemu_program()));
    if (!c.vm_disk) {
        co_return status(kind(), Availability::NotConfigured, "Set a guest disk image whose init runs loa-sandbox-stub on the first serial port");
    }
    co_return status(kind(), Availability::Ready, std::format("Disk {}, {}", c.vm_disk->string(), speed));
}

std::shared_ptr<SandboxProvider> QemuFactory::build(const Machine& m, const SandboxConfig& c) const {
    auto qemu = m.which(qemu_program());
    if (!qemu || !c.vm_disk) return nullptr;
    return std::make_shared<QemuProvider>(*qemu, *c.vm_disk, c.vm_kernel, m.kvm());
}

} // namespace loa::sandbox

#endif
