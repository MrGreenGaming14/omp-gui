#include "loa/remote/pty.hpp"

#include <libssh/callbacks.h>

#include "ops.hpp"

namespace loa::remote {

namespace detail {

namespace {
constexpr int SLICE_MS = 20;
}

struct ShellChannel {
    std::string host;
    std::shared_ptr<Session> session;
    ssh_channel channel = nullptr;
    ssh_channel_callbacks_struct callbacks{};
    std::optional<std::uint32_t> exit_status;
    // Guarded by the link mutex.
    bool done = false;
    bool hung_up = false;

    ~ShellChannel() {
        std::lock_guard lock(session->link->mu);
        ssh_remove_channel_callbacks(channel, &callbacks);
        if (ssh_channel_is_open(channel)) ssh_channel_close(channel);
        ssh_channel_free(channel);
    }
};

namespace {

void on_exit_status(ssh_session, ssh_channel, int status, void* userdata) {
    static_cast<ShellChannel*>(userdata)->exit_status = static_cast<std::uint32_t>(status);
}

} // namespace

Result<Shell> open_shell(std::shared_ptr<Session> session, std::uint16_t cols, std::uint16_t rows) {
    const auto host = session->host;
    auto opened = open_channel(*session->link, host);
    if (!opened) return std::unexpected(opened.error());
    auto shell = std::make_shared<ShellChannel>();
    shell->host = host;
    shell->session = session;
    shell->channel = *opened;
    shell->callbacks.userdata = shell.get();
    shell->callbacks.channel_exit_status_function = on_exit_status;
    ssh_callbacks_init(&shell->callbacks);
    auto& link = *session->link;
    std::lock_guard lock(link.mu);
    ssh_set_channel_callbacks(shell->channel, &shell->callbacks);
    const auto refused = [&](const char* what) -> Result<Shell> {
        if (ssh_is_connected(link.ssh) == 0) return std::unexpected(Error::ssh(host, ssh_get_error(link.ssh)));
        if (ssh_channel_is_closed(shell->channel)) return std::unexpected(Error::invalid_request(host + " closed the shell channel at once"));
        return std::unexpected(Error::invalid_request(host + " refused to open " + what));
    };
    if (ssh_channel_request_pty_size(shell->channel, std::string(TERM).c_str(), cols, rows) != SSH_OK) return refused("a terminal");
    if (ssh_channel_request_shell(shell->channel) != SSH_OK) return refused("a shell");
    return Shell{ShellReader(shell), ShellWriter(shell)};
}

} // namespace detail

const std::string& ShellReader::host() const noexcept { return channel_->host; }

std::optional<ShellRead> ShellReader::read_for(std::chrono::milliseconds timeout) {
    auto& c = *channel_;
    auto& link = *c.session->link;
    const auto deadline = std::chrono::steady_clock::now() + timeout;
    char buf[16 * 1024];
    for (;;) {
        {
            std::lock_guard lock(link.mu);
            if (!c.done) {
                for (int stream = 0; stream < 2; ++stream) {
                    const int n = ssh_channel_read_nonblocking(c.channel, buf, sizeof buf, stream);
                    if (n > 0) return ShellRead::output(std::string(buf, static_cast<std::size_t>(n)));
                }
                if (ssh_channel_is_closed(c.channel) || ssh_is_connected(link.ssh) == 0) {
                    c.done = true;
                } else {
                    const int polled = ssh_channel_poll_timeout(c.channel, detail::SLICE_MS, 0);
                    if (polled == SSH_ERROR) c.done = true;
                    // At EOF the poll returns at once; the close follows.
                    if (polled == SSH_EOF && !ssh_channel_is_closed(c.channel)) {
                        ssh_channel_poll_timeout(c.channel, detail::SLICE_MS, 1);
                    }
                }
            }
            if (c.done) return ShellRead::closed_with(c.exit_status);
        }
        if (std::chrono::steady_clock::now() >= deadline) return std::nullopt;
    }
}

ShellRead ShellReader::read() {
    for (;;) {
        if (auto r = read_for(std::chrono::hours(1))) return std::move(*r);
    }
}

ShellWriter::~ShellWriter() {
    if (channel_) (void)close();
}

const std::string& ShellWriter::host() const noexcept { return channel_->host; }

Result<void> ShellWriter::write(std::string_view bytes) {
    auto& c = *channel_;
    std::lock_guard lock(c.session->link->mu);
    if (c.hung_up) return std::unexpected(Error::invalid_request("the shell on " + c.host + " is closed"));
    if (ssh_channel_write(c.channel, bytes.data(), static_cast<std::uint32_t>(bytes.size())) == SSH_ERROR) {
        return std::unexpected(Error::ssh(c.host, ssh_get_error(c.session->link->ssh)));
    }
    return {};
}

Result<void> ShellWriter::resize(std::uint16_t cols, std::uint16_t rows) {
    auto& c = *channel_;
    std::lock_guard lock(c.session->link->mu);
    if (c.hung_up) return std::unexpected(Error::invalid_request("the shell on " + c.host + " is closed"));
    if (ssh_channel_change_pty_size(c.channel, cols, rows) == SSH_ERROR) {
        return std::unexpected(Error::ssh(c.host, ssh_get_error(c.session->link->ssh)));
    }
    return {};
}

Result<void> ShellWriter::close() {
    auto& c = *channel_;
    std::lock_guard lock(c.session->link->mu);
    if (c.hung_up) return {};
    c.hung_up = true;
    if (!ssh_channel_is_open(c.channel)) return {};
    ssh_channel_send_eof(c.channel);
    if (ssh_channel_close(c.channel) == SSH_ERROR) return std::unexpected(Error::ssh(c.host, ssh_get_error(c.session->link->ssh)));
    return {};
}

} // namespace loa::remote
