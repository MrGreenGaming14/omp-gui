#include "loa/remote/connection.hpp"

#include <libssh/libssh.h>
#include <libssh/server.h>

#include <netdb.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>

#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <system_error>

#include "loa/remote/agent.hpp"
#include "loa/runtime/log.hpp"
#include "session.hpp"
#include "sftp.hpp"
#include "wire.hpp"

namespace loa::remote {

namespace detail {

namespace {

// Slice the pump waits on its channel before checking the socket again.
constexpr int PUMP_SLICE_MS = 10;

std::string os_error(int code) { return std::error_code(code, std::generic_category()).message(); }

bool write_all(int fd, const char* data, std::size_t len) {
    while (len > 0) {
        const auto n = ::send(fd, data, len, MSG_NOSIGNAL);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) return false;
        data += n;
        len -= static_cast<std::size_t>(n);
    }
    return true;
}

} // namespace

Link::~Link() {
    if (ssh) {
        {
            std::lock_guard lock(mu);
            ssh_disconnect(ssh);
            ssh_free(ssh);
            ssh = nullptr;
        }
    }
    tunnel.reset();
}

bool Link::alive() {
    if (dead.load()) return false;
    if (tunnel && !tunnel->alive()) return false;
    std::lock_guard lock(mu);
    return ssh && ssh_is_connected(ssh) != 0;
}

Result<std::unique_ptr<Tunnel>> Tunnel::open(std::shared_ptr<Link> via, const Endpoint& hop, const std::string& label, int& inner_fd) {
    int fds[2];
    if (::socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, fds) != 0) return std::unexpected(Error::connect(label, os_error(errno)));
    ssh_channel channel = nullptr;
    {
        std::lock_guard lock(via->mu);
        channel = ssh_channel_new(via->ssh);
        if (!channel || ssh_channel_open_forward(channel, hop.hostname.c_str(), hop.port, "127.0.0.1", 0) != SSH_OK) {
            auto e = Error::ssh(label, ssh_get_error(via->ssh));
            if (channel) ssh_channel_free(channel);
            ::close(fds[0]);
            ::close(fds[1]);
            return std::unexpected(std::move(e));
        }
    }
    inner_fd = fds[1];
    return std::unique_ptr<Tunnel>(new Tunnel(std::move(via), channel, fds[0]));
}

Tunnel::Tunnel(std::shared_ptr<Link> via, ssh_channel channel, int fd) : via_(std::move(via)), channel_(channel), fd_(fd) {
    thread_ = std::jthread([this](std::stop_token stop) { pump(stop); });
}

Tunnel::~Tunnel() {
    thread_.request_stop();
    if (thread_.joinable()) thread_.join();
    ::close(fd_);
    close_channel(*via_, channel_);
}

void Tunnel::pump(std::stop_token stop) {
    char buf[32 * 1024];
    bool socket_open = true;
    bool channel_open = true;
    while (!stop.stop_requested() && (socket_open || channel_open)) {
        if (socket_open) {
            pollfd p{fd_, POLLIN, 0};
            const int ready = ::poll(&p, 1, channel_open ? 0 : PUMP_SLICE_MS);
            if (ready > 0) {
                const auto n = ::recv(fd_, buf, sizeof buf, 0);
                std::lock_guard lock(via_->mu);
                if (n > 0) {
                    if (ssh_channel_write(channel_, buf, static_cast<std::uint32_t>(n)) == SSH_ERROR) break;
                } else if (n == 0 || (errno != EINTR && errno != EAGAIN)) {
                    socket_open = false;
                    ssh_channel_send_eof(channel_);
                }
            }
        }
        if (channel_open) {
            int n = 0;
            {
                std::lock_guard lock(via_->mu);
                const int avail = ssh_channel_poll_timeout(channel_, PUMP_SLICE_MS, 0);
                if (avail == SSH_ERROR) break;
                if (avail == SSH_EOF || ssh_channel_is_closed(channel_)) {
                    channel_open = false;
                } else if (avail > 0) {
                    n = ssh_channel_read_nonblocking(channel_, buf, sizeof buf, 0);
                    if (n == SSH_ERROR) break;
                }
            }
            if (n > 0 && !write_all(fd_, buf, static_cast<std::size_t>(n))) break;
            if (!channel_open) ::shutdown(fd_, SHUT_WR);
        }
    }
    done_ = true;
    ::shutdown(fd_, SHUT_RDWR);
}

Result<std::shared_ptr<Sftp>> Session::sftp() {
    std::lock_guard lock(sftp_mu_);
    if (sftp_) return sftp_;
    auto opened = Sftp::open(link, host);
    if (!opened) return std::unexpected(opened.error());
    sftp_ = std::move(*opened);
    return sftp_;
}

void Session::reset_sftp() {
    std::lock_guard lock(sftp_mu_);
    sftp_.reset();
}

bool Session::alive() {
    if (!link->alive()) return false;
    for (const auto& j : jumps) {
        if (!j->alive()) return false;
    }
    return true;
}

Result<ssh_channel> open_channel(Link& link, const std::string& host) {
    std::lock_guard lock(link.mu);
    ssh_channel channel = ssh_channel_new(link.ssh);
    if (!channel) return std::unexpected(Error::ssh(host, ssh_get_error(link.ssh)));
    if (ssh_channel_open_session(channel) != SSH_OK) {
        auto e = Error::ssh(host, ssh_get_error(link.ssh));
        ssh_channel_free(channel);
        return std::unexpected(std::move(e));
    }
    return channel;
}

void close_channel(Link& link, ssh_channel channel) {
    if (!channel) return;
    std::lock_guard lock(link.mu);
    if (ssh_channel_is_open(channel)) ssh_channel_close(channel);
    ssh_channel_free(channel);
}

std::string ssh_error(Link& link) { return ssh_get_error(link.ssh); }

struct PoolSlot {
    std::mutex mu;
    std::vector<Endpoint> hops;
    std::shared_ptr<Session> session;
};

} // namespace detail

using detail::Link;
using detail::Session;

namespace {

std::optional<std::filesystem::path> default_agent_from_env() {
    std::optional<std::filesystem::path> env;
    if (const char* sock = std::getenv("SSH_AUTH_SOCK"); sock && *sock) env = std::filesystem::path(sock);
#if defined(_WIN32)
    if (!env) env = std::filesystem::path(R"(\\.\pipe\openssh-ssh-agent)");
#endif
    return env;
}

// Host key algorithms, certificates first so hosts with a certificate present it.
constexpr const char* HOSTKEYS =
    "ssh-ed25519-cert-v01@openssh.com,ecdsa-sha2-nistp521-cert-v01@openssh.com,ecdsa-sha2-nistp384-cert-v01@openssh.com,"
    "ecdsa-sha2-nistp256-cert-v01@openssh.com,rsa-sha2-512-cert-v01@openssh.com,rsa-sha2-256-cert-v01@openssh.com,"
    "ssh-ed25519,ecdsa-sha2-nistp521,ecdsa-sha2-nistp384,ecdsa-sha2-nistp256,rsa-sha2-512,rsa-sha2-256";

// A TCP connection to `hostname:port` within `timeout`.
Result<int> dial(const std::string& hostname, std::uint16_t port, std::chrono::milliseconds timeout, const std::string& label) {
    addrinfo hints{};
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_family = AF_UNSPEC;
    addrinfo* found = nullptr;
    const auto service = std::to_string(port);
    if (const int rc = ::getaddrinfo(hostname.c_str(), service.c_str(), &hints, &found); rc != 0) {
        return std::unexpected(Error::connect(label, gai_strerror(rc)));
    }
    std::unique_ptr<addrinfo, decltype(&::freeaddrinfo)> list(found, &::freeaddrinfo);
    const auto deadline = std::chrono::steady_clock::now() + timeout;
    std::string last = "no address";
    for (auto* ai = found; ai; ai = ai->ai_next) {
        const int fd = ::socket(ai->ai_family, ai->ai_socktype | SOCK_CLOEXEC | SOCK_NONBLOCK, ai->ai_protocol);
        if (fd < 0) {
            last = detail::os_error(errno);
            continue;
        }
        int rc = ::connect(fd, ai->ai_addr, ai->ai_addrlen);
        if (rc != 0 && errno == EINPROGRESS) {
            const auto left = std::chrono::duration_cast<std::chrono::milliseconds>(deadline - std::chrono::steady_clock::now());
            pollfd p{fd, POLLOUT, 0};
            const int ready = left.count() > 0 ? ::poll(&p, 1, static_cast<int>(left.count())) : 0;
            if (ready == 0) {
                ::close(fd);
                return std::unexpected(Error::connect_timeout(label));
            }
            int err = 0;
            socklen_t len = sizeof err;
            ::getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &len);
            rc = err == 0 ? 0 : -1;
            errno = err;
        }
        if (rc == 0) {
            ::fcntl(fd, F_SETFL, ::fcntl(fd, F_GETFL) & ~O_NONBLOCK);
            const int one = 1;
            ::setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
            return fd;
        }
        last = detail::os_error(errno);
        ::close(fd);
    }
    return std::unexpected(Error::connect(label, last));
}

} // namespace

ConnectorConfig ConnectorConfig::from_env() { return with_agent(default_agent_from_env()); }

ConnectorConfig ConnectorConfig::with_agent(std::optional<std::filesystem::path> default_agent) {
    ConnectorConfig c;
    c.default_agent = std::move(default_agent);
    return c;
}

Connector::Connector(std::shared_ptr<KnownHosts> known, std::shared_ptr<HostKeyDecider> decider, ConnectorConfig config)
    : known_(std::move(known)), decider_(std::move(decider)), config_(std::move(config)) {
    keepalive_ = std::jthread([this](std::stop_token stop) { keepalive_loop(stop); });
}

Connector::~Connector() {
    keepalive_.request_stop();
    wake_.notify_all();
    if (keepalive_.joinable()) keepalive_.join();
}

void Connector::keepalive_loop(std::stop_token stop) {
    std::unique_lock lock(mu_);
    while (!stop.stop_requested()) {
        wake_.wait_for(lock, stop, config_.keepalive, [] { return false; });
        if (stop.stop_requested()) return;
        std::vector<std::shared_ptr<detail::PoolSlot>> slots;
        for (const auto& [_, slot] : pool_) slots.push_back(slot);
        lock.unlock();
        for (const auto& slot : slots) {
            std::shared_ptr<Session> session;
            {
                std::unique_lock s(slot->mu, std::try_to_lock);
                if (!s.owns_lock()) continue; // dialling right now
                session = slot->session;
            }
            if (!session) continue;
            std::vector<std::shared_ptr<Link>> links = session->jumps;
            links.push_back(session->link);
            for (const auto& link : links) {
                std::lock_guard l(link->mu);
                if (link->ssh && ssh_send_keepalive(link->ssh) != SSH_OK) link->dead = true;
            }
        }
        lock.lock();
    }
}

void Connector::disconnect(const std::string& host) {
    std::shared_ptr<detail::PoolSlot> slot;
    {
        std::lock_guard lock(mu_);
        const auto it = pool_.find(host);
        if (it == pool_.end()) return;
        slot = it->second;
        pool_.erase(it);
    }
    std::shared_ptr<Session> session;
    {
        std::lock_guard lock(slot->mu);
        session = std::move(slot->session);
        slot->hops.clear();
    }
}

namespace {

// Connects and authenticates one hop, directly or through `via`.
Result<std::shared_ptr<Link>> hop(const Host& host, const Endpoint& endpoint, const std::shared_ptr<Link>& via, const KnownHosts& known,
                                  HostKeyDecider& decider, const ConnectorConfig& config, const CancelToken& cancel) {
    const auto label = endpoint.hostname + ":" + std::to_string(endpoint.port);
    auto link = std::make_shared<Link>();
    int fd = -1;
    if (via) {
        auto tunnel = detail::Tunnel::open(via, endpoint, label, fd);
        if (!tunnel) return std::unexpected(tunnel.error());
        link->tunnel = std::move(*tunnel);
    } else {
        auto dialled = dial(endpoint.hostname, endpoint.port, config.connect_timeout, label);
        if (!dialled) return std::unexpected(dialled.error());
        fd = *dialled;
    }
    std::lock_guard lock(link->mu);
    link->ssh = ssh_new();
    if (!link->ssh) {
        ::close(fd);
        return std::unexpected(Error::ssh(label, "out of memory"));
    }
    ssh_session s = link->ssh;
    const int no = 0;
    const long timeout = std::max<long>(1, static_cast<long>(config.connect_timeout.count() / 1000));
    ssh_options_set(s, SSH_OPTIONS_PROCESS_CONFIG, &no);
    ssh_options_set(s, SSH_OPTIONS_HOST, endpoint.hostname.c_str());
    const unsigned int port = endpoint.port;
    ssh_options_set(s, SSH_OPTIONS_PORT, &port);
    ssh_options_set(s, SSH_OPTIONS_USER, endpoint.user.c_str());
    ssh_options_set(s, SSH_OPTIONS_TIMEOUT, &timeout);
    ssh_options_set(s, SSH_OPTIONS_HOSTKEYS, HOSTKEYS);
    ssh_options_set(s, SSH_OPTIONS_FD, &fd);
    if (ssh_connect(s) != SSH_OK) return std::unexpected(Error::ssh(label, ssh_get_error(s)));

    ssh_key server_key = nullptr;
    if (ssh_get_server_publickey(s, &server_key) != SSH_OK) return std::unexpected(Error::ssh(label, ssh_get_error(s)));
    char* b64 = nullptr;
    const int exported = ssh_pki_export_pubkey_base64(server_key, &b64);
    ssh_key_free(server_key);
    if (exported != SSH_OK || !b64) return std::unexpected(Error::ssh(label, "cannot read the server's host key"));
    auto blob = wire::base64_decode(b64);
    ssh_string_free_char(b64);
    auto presented = blob ? PublicKey::from_blob(std::move(*blob)) : std::nullopt;
    if (!presented) return std::unexpected(Error::ssh(label, "cannot read the server's host key"));

    // The host key check: trusted, or new/changed and accepted by the decider (then saved).
    auto status = known.check(endpoint.hostname, endpoint.port, *presented);
    if (!status) return std::unexpected(Error::host_key_rejected(label, status.error().message()));
    if (status->kind == HostKeyStatus::Kind::revoked) {
        const auto& e = *status->entry;
        return std::unexpected(Error::host_key_rejected(
            label, "key " + e.fingerprint + " is marked @revoked in " + e.file.string() + ":" + std::to_string(e.line)));
    }
    if (status->kind != HostKeyStatus::Kind::trusted) {
        const bool changed = status->kind == HostKeyStatus::Kind::changed;
        HostKeyChange change{changed ? HostKeyChange::Kind::changed : HostKeyChange::Kind::new_key, std::move(status->previous)};
        HostKeyPrompt prompt{host.name, endpoint.hostname, endpoint.port, presented->algorithm(), fingerprint(*presented), std::move(change)};
        if (!decider.decide(std::move(prompt), cancel)) {
            return std::unexpected(Error::host_key_rejected(
                label, changed ? "the host key CHANGED and the new key was not accepted" : "the new host key was not accepted"));
        }
        if (auto saved = known.add(endpoint.hostname, endpoint.port, presented->key_data()); !saved) {
            return std::unexpected(Error::host_key_rejected(label, "accepted, but saving it failed: " + saved.error().message()));
        }
    }

    auto socket = socket_for(endpoint.agent, config.default_agent, label);
    if (!socket) return std::unexpected(socket.error());
    auto identities = agent_identities(*socket);
    if (!identities) return std::unexpected(identities.error());
    const auto socket_text = socket->string();
    ssh_options_set(s, SSH_OPTIONS_IDENTITY_AGENT, socket_text.c_str());
    switch (identities->empty() ? SSH_AUTH_DENIED : ssh_userauth_agent(s, endpoint.user.c_str())) {
    case SSH_AUTH_SUCCESS: return link;
    case SSH_AUTH_ERROR: return std::unexpected(Error::ssh(label, ssh_get_error(s)));
    default: return std::unexpected(Error::auth_failed(endpoint.user, label, identities->size()));
    }
}

} // namespace

Result<std::shared_ptr<Session>> Connector::session(const Host& host, const CancelToken& cancel) {
    std::shared_ptr<detail::PoolSlot> slot;
    {
        std::lock_guard lock(mu_);
        auto& entry = pool_[host.name];
        if (!entry) entry = std::make_shared<detail::PoolSlot>();
        slot = entry;
    }
    std::lock_guard pooled(slot->mu);
    const auto hops = host.hops();
    if (slot->session && slot->session->alive() && slot->hops == hops) return slot->session;
    slot->session.reset();
    slot->hops.clear();
    std::vector<std::shared_ptr<Link>> jumps;
    for (const auto& jump : host.jumps) {
        auto next = hop(host, jump, jumps.empty() ? nullptr : jumps.back(), *known_, *decider_, config_, cancel);
        if (!next) return std::unexpected(next.error());
        jumps.push_back(std::move(*next));
    }
    auto link = hop(host, host.target, jumps.empty() ? nullptr : jumps.back(), *known_, *decider_, config_, cancel);
    if (!link) return std::unexpected(link.error());
    auto session = std::make_shared<Session>();
    session->host = host.name;
    session->link = std::move(*link);
    session->jumps = std::move(jumps);
    slot->session = session;
    slot->hops = hops;
    return session;
}

} // namespace loa::remote
