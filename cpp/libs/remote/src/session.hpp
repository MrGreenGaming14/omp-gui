#pragma once

// Internal: an authenticated libssh connection (`Link`), the tunnel a jump hop runs through, and
// the pooled `Session` with its SFTP channel.
//
// libssh sessions are not thread-safe: every call on a link's `ssh_session` or one of its
// channels happens with `Link::mu` held. Long waits poll in short slices so other channels of
// the same connection keep moving.

#include <libssh/libssh.h>

#include <atomic>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "loa/remote/error.hpp"
#include "loa/remote/hosts.hpp"

namespace loa::remote::detail {

struct Link;

// Forwards one end of a socket pair through a `direct-tcpip` channel of `via`; the other end is
// the next hop's transport.
class Tunnel {
  public:
    // Opens the channel to `hop` on `via`; `inner_fd` receives the next hop's end.
    [[nodiscard]] static Result<std::unique_ptr<Tunnel>> open(std::shared_ptr<Link> via, const Endpoint& hop, const std::string& label,
                                                             int& inner_fd);
    ~Tunnel();
    Tunnel(const Tunnel&) = delete;
    Tunnel& operator=(const Tunnel&) = delete;
    [[nodiscard]] bool alive() const noexcept { return !done_.load(); }

  private:
    Tunnel(std::shared_ptr<Link> via, ssh_channel channel, int fd);
    void pump(std::stop_token stop);

    std::shared_ptr<Link> via_;
    ssh_channel channel_;
    int fd_;
    std::atomic<bool> done_{false};
    std::jthread thread_;
};

// One authenticated hop.
struct Link {
    std::mutex mu;
    ssh_session ssh = nullptr;
    // The tunnel this hop runs through (jump chains), destroyed after `ssh`.
    std::unique_ptr<Tunnel> tunnel;
    // Set when a keepalive went unanswered.
    std::atomic<bool> dead{false};

    Link() = default;
    Link(const Link&) = delete;
    Link& operator=(const Link&) = delete;
    ~Link();
    [[nodiscard]] bool alive();
};

class Sftp;

// An authenticated connection, with the jump connections it tunnels through kept alive.
struct Session {
    std::string host;
    std::shared_ptr<Link> link;
    std::vector<std::shared_ptr<Link>> jumps;

    // The session's SFTP channel, opened on first use.
    [[nodiscard]] Result<std::shared_ptr<Sftp>> sftp();
    // Drops the SFTP channel so the next call opens a new one.
    void reset_sftp();
    [[nodiscard]] bool alive();

  private:
    std::mutex sftp_mu_;
    std::shared_ptr<Sftp> sftp_;
};

// Opens a session channel on `link`; the caller frees it with `close_channel`.
[[nodiscard]] Result<ssh_channel> open_channel(Link& link, const std::string& host);
// Closes and frees a channel (lock taken here).
void close_channel(Link& link, ssh_channel channel);
// `ssh_get_error` text of the link.
[[nodiscard]] std::string ssh_error(Link& link);

} // namespace loa::remote::detail
