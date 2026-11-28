#include "loa/remote/exec.hpp"

#include "ops.hpp"

#include <libssh/callbacks.h>

#include <algorithm>
#include <thread>

#include "text.hpp"

namespace loa::remote {

std::string_view to_string(ExecEnd end) noexcept {
    switch (end) {
    case ExecEnd::exited: return "exited";
    case ExecEnd::timed_out: return "timed_out";
    case ExecEnd::cancelled: return "cancelled";
    }
    return "exited";
}

loa::runtime::json::Value ExecOutput::to_json() const {
    using loa::runtime::json::Value;
    loa::runtime::json::Object o;
    o.emplace_back("stdout", stdout_text);
    o.emplace_back("stderr", stderr_text);
    o.emplace_back("exit_status", exit_status ? Value(std::uint64_t{*exit_status}) : Value());
    o.emplace_back("exit_signal", exit_signal ? Value(*exit_signal) : Value());
    o.emplace_back("stdout_dropped", Value(stdout_dropped));
    o.emplace_back("stderr_dropped", Value(stderr_dropped));
    o.emplace_back("end", std::string(to_string(end)));
    return Value(std::move(o));
}

namespace detail {

namespace {

constexpr int SLICE_MS = 20;

// Captured bytes up to a cap, and how many were dropped past it.
struct Capped {
    std::string bytes;
    std::uint64_t dropped = 0;

    void push(std::string_view data, std::size_t cap) {
        const auto room = cap > bytes.size() ? cap - bytes.size() : 0;
        const auto take = std::min(room, data.size());
        bytes.append(data.substr(0, take));
        dropped += data.size() - take;
    }
};

struct ExitState {
    std::optional<std::uint32_t> status;
    std::optional<std::string> signal;
};

void on_exit_status(ssh_session, ssh_channel, int status, void* userdata) {
    static_cast<ExitState*>(userdata)->status = static_cast<std::uint32_t>(status);
}

void on_exit_signal(ssh_session, ssh_channel, const char* signal, int, const char*, const char*, void* userdata) {
    static_cast<ExitState*>(userdata)->signal = std::string(signal ? signal : "");
}

} // namespace

Result<ExecOutput> run(Link& link, const std::string& host, const std::string& command, std::size_t max_output,
                       std::chrono::milliseconds timeout, const CancelToken& cancel) {
    auto opened = open_channel(link, host);
    if (!opened) return std::unexpected(opened.error());
    ssh_channel channel = *opened;
    ExitState exit;
    ssh_channel_callbacks_struct callbacks{};
    callbacks.userdata = &exit;
    callbacks.channel_exit_status_function = on_exit_status;
    callbacks.channel_exit_signal_function = on_exit_signal;
    ssh_callbacks_init(&callbacks);
    {
        std::lock_guard lock(link.mu);
        ssh_set_channel_callbacks(channel, &callbacks);
        if (ssh_channel_request_exec(channel, command.c_str()) != SSH_OK) {
            const bool connected = ssh_is_connected(link.ssh) != 0;
            auto e = connected ? Error::invalid_request(host + " refused to run the command") : Error::ssh(host, ssh_get_error(link.ssh));
            ssh_remove_channel_callbacks(channel, &callbacks);
            ssh_channel_free(channel);
            return std::unexpected(std::move(e));
        }
    }
    Capped out, err;
    const auto deadline = std::chrono::steady_clock::now() + timeout;
    ExecEnd end = ExecEnd::exited;
    char buf[16 * 1024];
    for (;;) {
        if (cancel.cancelled()) {
            end = ExecEnd::cancelled;
            break;
        }
        if (std::chrono::steady_clock::now() >= deadline) {
            end = ExecEnd::timed_out;
            break;
        }
        bool closed = false;
        bool idle = true;
        {
            std::lock_guard lock(link.mu);
            const int polled = ssh_channel_poll_timeout(channel, SLICE_MS, 0);
            if (polled == SSH_ERROR && !ssh_channel_is_closed(channel)) {
                auto e = Error::ssh(host, ssh_get_error(link.ssh));
                ssh_remove_channel_callbacks(channel, &callbacks);
                ssh_channel_free(channel);
                return std::unexpected(std::move(e));
            }
            for (int stream = 0; stream < 2; ++stream) {
                for (;;) {
                    const int n = ssh_channel_read_nonblocking(channel, buf, sizeof buf, stream);
                    if (n <= 0) break;
                    idle = false;
                    (stream == 0 ? out : err).push(std::string_view(buf, static_cast<std::size_t>(n)), max_output);
                }
            }
            closed = ssh_channel_is_closed(channel) != 0;
            if (polled != SSH_EOF) idle = false;
        }
        if (closed) break;
        // At EOF the poll returns at once; wait a slice for the exit status and close.
        if (idle) cancel.wait_for(std::chrono::milliseconds(SLICE_MS));
    }
    {
        std::lock_guard lock(link.mu);
        if (end != ExecEnd::exited) {
            // Ask the server to stop the process; closing the channel ends it on servers that ignore signals.
            ssh_channel_request_send_signal(channel, "KILL");
            ssh_channel_close(channel);
        }
        ssh_remove_channel_callbacks(channel, &callbacks);
        ssh_channel_free(channel);
    }
    return ExecOutput{text::utf8_lossy(out.bytes), text::utf8_lossy(err.bytes), exit.status, exit.signal, out.dropped, err.dropped, end};
}

} // namespace detail

} // namespace loa::remote
