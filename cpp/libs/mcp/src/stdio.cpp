// Stdio transport: the server as a child in its own process group, newline-delimited JSON on its stdio.

#include <atomic>
#include <cerrno>
#include <cstring>
#include <mutex>
#include <thread>

#include "connect.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/log.hpp"

#ifdef _WIN32
#include <windows.h>
#else
#include <csignal>
#include <fcntl.h>
#include <poll.h>
#include <sys/wait.h>
#include <unistd.h>
extern char** environ;
#endif

namespace loa::mcp::detail {

namespace exec = loa::runtime::exec;

#ifdef _WIN32

// Windows runs the server in a job object (kill on close, CREATE_NO_WINDOW), as the Rust
// `process_wrap` `JobObject` + `KillOnDrop` + `CreationFlags` wrappers do. Not built yet.
std::expected<std::unique_ptr<Transport>, std::string> spawn_stdio(const StdioSpec&, Sink) {
    return std::unexpected(std::string("stdio MCP servers aren't supported on this platform yet"));
}

#else

namespace {

std::string os_error(int code) { return std::string(std::strerror(code)) + " (os error " + std::to_string(code) + ")"; }

class StdioTransport final : public Transport {
  public:
    StdioTransport(pid_t pid, int in, int out, int wake_r, int wake_w, exec::StderrTail tail, Sink sink)
        : pid_(pid), in_(in), out_(out), wake_r_(wake_r), wake_w_(wake_w), tail_(std::move(tail)), sink_(std::move(sink)) {
        reader_ = std::thread([this] { read_loop(); });
    }

    ~StdioTransport() override { close(); }

    std::expected<void, std::string> send(const std::string& message, const Cancel&) override {
        std::lock_guard lock(write_mutex_);
        if (in_ < 0) return std::unexpected(std::string("Transport closed"));
        std::string line = message + "\n";
        std::size_t done = 0;
        while (done < line.size()) {
            const auto n = ::write(in_, line.data() + done, line.size() - done);
            if (n < 0 && errno == EINTR) continue;
            if (n <= 0) return std::unexpected(os_error(errno));
            done += static_cast<std::size_t>(n);
        }
        return {};
    }

    bool synchronous() const noexcept override { return false; }

    void close() override {
        std::call_once(closed_, [this] {
            // The whole group: `npx` runs the server as a grandchild.
            ::kill(-pid_, SIGKILL);
            {
                std::lock_guard lock(write_mutex_);
                if (in_ >= 0) ::close(std::exchange(in_, -1));
            }
            const char byte = 1;
            (void)!::write(wake_w_, &byte, 1);
            if (reader_.joinable() && reader_.get_id() != std::this_thread::get_id()) reader_.join();
            else if (reader_.joinable()) reader_.detach();
            while (::waitpid(pid_, nullptr, 0) < 0 && errno == EINTR) {}
            ::close(out_);
            ::close(wake_r_);
            ::close(wake_w_);
        });
    }

    std::optional<std::string> last_line() override {
        (void)tail_.wait_closed(std::chrono::seconds(1));
        return last_nonempty_line(tail_.joined());
    }

  private:
    void read_loop() {
        std::string buffer;
        char chunk[65536];
        for (;;) {
            pollfd fds[2] = {{out_, POLLIN, 0}, {wake_r_, POLLIN, 0}};
            if (::poll(fds, 2, -1) < 0) {
                if (errno == EINTR) continue;
                break;
            }
            if (fds[1].revents != 0) break;
            if (fds[0].revents == 0) continue;
            const auto n = ::read(out_, chunk, sizeof chunk);
            if (n < 0 && errno == EINTR) continue;
            if (n <= 0) break;
            buffer.append(chunk, static_cast<std::size_t>(n));
            std::size_t start = 0;
            for (auto nl = buffer.find('\n', start); nl != std::string::npos; nl = buffer.find('\n', start)) {
                std::string_view line(buffer.data() + start, nl - start);
                if (!line.empty() && line.back() == '\r') line.remove_suffix(1);
                if (!line.empty()) sink_.message(line);
                start = nl + 1;
            }
            buffer.erase(0, start);
        }
        sink_.closed();
    }

    pid_t pid_;
    int in_;
    int out_;
    int wake_r_;
    int wake_w_;
    exec::StderrTail tail_;
    Sink sink_;
    std::mutex write_mutex_;
    std::once_flag closed_;
    std::thread reader_;
};

void close_pair(int fds[2]) {
    if (fds[0] >= 0) ::close(fds[0]);
    if (fds[1] >= 0) ::close(fds[1]);
}

} // namespace

std::expected<std::unique_ptr<Transport>, std::string> spawn_stdio(const StdioSpec& spec, Sink sink) {
    static const bool ignore_sigpipe = [] {
        ::signal(SIGPIPE, SIG_IGN);
        return true;
    }();
    (void)ignore_sigpipe;

    std::optional<std::string_view> path_var;
    for (const auto& [key, value] : spec.env)
        if (exec::is_path_var(key)) path_var = value;
    const auto program = exec::resolve_in(spec.command, path_var).string();

    std::vector<std::string> argv_store{program};
    argv_store.insert(argv_store.end(), spec.args.begin(), spec.args.end());
    std::vector<char*> argv;
    for (auto& arg : argv_store) argv.push_back(arg.data());
    argv.push_back(nullptr);

    std::vector<std::string> env_store;
    for (char** e = environ; *e != nullptr; ++e) {
        std::string_view entry(*e);
        const auto key = entry.substr(0, entry.find('='));
        if (!spec.env.contains(std::string(key))) env_store.emplace_back(entry);
    }
    for (const auto& [key, value] : spec.env) env_store.push_back(key + "=" + value);
    std::vector<char*> envp;
    for (auto& entry : env_store) envp.push_back(entry.data());
    envp.push_back(nullptr);
    const std::string cwd = spec.cwd ? spec.cwd->string() : std::string();

    int in[2] = {-1, -1}, out[2] = {-1, -1}, err[2] = {-1, -1}, status[2] = {-1, -1}, wake[2] = {-1, -1};
    auto cleanup = [&] {
        close_pair(in);
        close_pair(out);
        close_pair(err);
        close_pair(status);
        close_pair(wake);
    };
    if (::pipe2(in, O_CLOEXEC) < 0 || ::pipe2(out, O_CLOEXEC) < 0 || ::pipe2(err, O_CLOEXEC) < 0 || ::pipe2(status, O_CLOEXEC) < 0 ||
        ::pipe2(wake, O_CLOEXEC) < 0) {
        const int code = errno;
        cleanup();
        return std::unexpected(os_error(code));
    }

    auto spawned = exec::retry_busy([&]() -> std::expected<pid_t, std::error_code> {
        const pid_t pid = ::fork();
        if (pid < 0) return std::unexpected(std::error_code(errno, std::generic_category()));
        if (pid == 0) {
            ::setpgid(0, 0);
            ::dup2(in[0], 0);
            ::dup2(out[1], 1);
            ::dup2(err[1], 2);
            if (!cwd.empty() && ::chdir(cwd.c_str()) < 0) {
                const int code = errno;
                (void)!::write(status[1], &code, sizeof code);
                ::_exit(127);
            }
            ::execve(argv[0], argv.data(), envp.data());
            const int code = errno;
            (void)!::write(status[1], &code, sizeof code);
            ::_exit(127);
        }
        ::close(status[1]);
        status[1] = -1;
        int code = 0;
        ssize_t got = 0;
        while ((got = ::read(status[0], &code, sizeof code)) < 0 && errno == EINTR) {}
        ::close(status[0]);
        status[0] = -1;
        if (got == sizeof code) {
            while (::waitpid(pid, nullptr, 0) < 0 && errno == EINTR) {}
            if (::pipe2(status, O_CLOEXEC) < 0) return std::unexpected(std::error_code(errno, std::generic_category()));
            return std::unexpected(std::error_code(code, std::generic_category()));
        }
        return pid;
    });
    if (!spawned) {
        cleanup();
        return std::unexpected(os_error(spawned.error().value()));
    }
    ::close(in[0]);
    ::close(out[1]);
    ::close(err[1]);
    auto tail = exec::StderrTail::spawn(err[0], 20);
    return std::make_unique<StdioTransport>(*spawned, in[1], out[0], wake[0], wake[1], std::move(tail), std::move(sink));
}

#endif

} // namespace loa::mcp::detail
