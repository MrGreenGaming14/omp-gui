#pragma once

// Test support: a loopback OpenSSH `sshd` (run unprivileged, logging at DEBUG1 so connections,
// command sessions, forwards and signals can be counted), an `ssh-agent` holding one generated
// key, and a wired `RemoteService` (`Rig`).
//
// `sshd` comes from `LOA_SSHD`, `/usr/sbin/sshd` or `~/.cache/loa/openssh-server/usr/sbin/sshd`
// (an extracted `openssh-server` package); tests that need it are skipped when none is found.

#include <sys/types.h>

#include <filesystem>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <vector>

#include "loa/remote/audit.hpp"
#include "loa/remote/hosts.hpp"
#include "loa/remote/limits.hpp"
#include "loa/remote/ports.hpp"
#include "loa/remote/service.hpp"
#include "loa/runtime/tempfile.hpp"

namespace loa::remote::test {

// The login name the servers accept.
[[nodiscard]] std::string user();
// Whether `sshd` and `sftp-server` were found.
[[nodiscard]] bool have_sshd();
// Runs `/bin/sh -c command`; its exit status.
int sh(const std::string& command);
// A new ed25519 key pair at `path` (`path.pub` holds the public line).
void keygen(const std::filesystem::path& path);
[[nodiscard]] std::string slurp(const std::filesystem::path& path);
// `PublicKey` of an OpenSSH `.pub` file.
[[nodiscard]] PublicKey public_key(const std::filesystem::path& pub);

// An `ssh-agent` with one generated key.
class Agent {
  public:
    explicit Agent(const std::filesystem::path& dir);
    ~Agent();
    std::filesystem::path socket;
    // The key's `.pub` line.
    std::string public_line;

  private:
    pid_t pid_ = -1;
};

// One loopback `sshd`.
class TestServer {
  public:
    // Accepts the key `authorized_line` (an OpenSSH public key line).
    explicit TestServer(const std::string& authorized_line);
    ~TestServer();
    TestServer(const TestServer&) = delete;
    TestServer& operator=(const TestServer&) = delete;

    std::uint16_t port = 0;
    PublicKey host_key;
    // `[127.0.0.1]:port key\n`.
    [[nodiscard]] std::string known_hosts_line() const;
    // Accepted TCP connections so far.
    [[nodiscard]] std::size_t connections() const;
    // Command sessions started so far (SFTP and shells not counted).
    [[nodiscard]] std::size_t commands() const;
    // Whether the log has a `direct-tcpip` forward to `127.0.0.1:port`.
    [[nodiscard]] bool forwarded_to(std::uint16_t port) const;
    // Whether the log has the signal request `name`.
    [[nodiscard]] bool signalled(const std::string& name) const;
    [[nodiscard]] std::string log() const;

  private:
    loa::runtime::TempDir dir_;
    pid_t pid_ = -1;
};

// Records host key prompts and answers them with `accept`.
class Decider final : public HostKeyDecider {
  public:
    explicit Decider(bool accept) : accept_(accept) {}
    bool decide(HostKeyPrompt prompt, const CancelToken&) override;
    [[nodiscard]] std::vector<HostKeyPrompt> prompts() const;

  private:
    bool accept_;
    mutable std::mutex mu_;
    std::vector<HostKeyPrompt> prompts_;
};

// Answers approvals from a script (then denies) and records the requests.
class Scripted final : public Approver {
  public:
    explicit Scripted(std::vector<Approval> answers) : answers_(std::move(answers)) {}
    Approval approve(ApprovalRequest request, const CancelToken&) override;
    [[nodiscard]] std::vector<ApprovalRequest> requests() const;

  private:
    mutable std::mutex mu_;
    std::vector<Approval> answers_;
    std::vector<ApprovalRequest> requests_;
};

// A direct hop to `127.0.0.1:port` as `user()`.
[[nodiscard]] Endpoint endpoint(std::uint16_t port);

// A wired service and what it talks to; the host `box` is the server.
struct Rig {
    loa::runtime::TempDir dir;
    std::unique_ptr<Agent> agent;
    std::unique_ptr<TestServer> server;
    std::filesystem::path known_hosts;
    std::shared_ptr<HostRegistry> hosts;
    std::shared_ptr<MemoryAudit> audit;
    std::shared_ptr<Decider> decider;
    std::shared_ptr<Scripted> approver;
    std::shared_ptr<RemoteService> service;

    [[nodiscard]] AuditRecord last() const;
};

// How `build` wires the service against one server whose host is `box`.
struct Setup {
    HostLimits limits;
    std::vector<Approval> approvals;
    // `known_hosts` text; none trusts the server's key.
    std::optional<std::string> known;
    bool accept_keys = false;
    // Whether the server accepts the agent's key, and whether the app has an agent at all.
    bool authorized = true;
    bool agent = true;

    [[nodiscard]] std::unique_ptr<Rig> build() const;
};

[[nodiscard]] inline Setup setup(HostLimits limits) { return Setup{std::move(limits), {}, std::nullopt, false, true, true}; }

// Every path allowed, writes and commands auto-approved.
[[nodiscard]] HostLimits open_limits();

} // namespace loa::remote::test
