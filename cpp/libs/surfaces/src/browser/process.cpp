#include "browser/process.hpp"

#include <asio/steady_timer.hpp>
#include <asio/this_coro.hpp>
#include <asio/use_awaitable.hpp>

#include <fcntl.h>
#include <signal.h>
#include <spawn.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cerrno>
#include <cstring>
#include <map>
#include <thread>

#include "loa/runtime/log.hpp"

extern char** environ;

namespace loa::surfaces::browser::detail {

namespace {

void close_fd(int& fd) noexcept {
    if (fd >= 0) ::close(fd);
    fd = -1;
}

asio::awaitable<void> sleep_for(std::chrono::milliseconds d) {
    asio::steady_timer timer(co_await asio::this_coro::executor, d);
    co_await timer.async_wait(asio::use_awaitable);
}

} // namespace

Result<Process> Process::spawn(const std::filesystem::path& program, const std::vector<std::string>& args,
                               const std::vector<std::pair<std::string, std::string>>& env, bool pipe_stdout) {
    int out[2] = {-1, -1};
    int err[2] = {-1, -1};
    if ((pipe_stdout && ::pipe2(out, O_CLOEXEC) != 0) || ::pipe2(err, O_CLOEXEC) != 0) {
        const int e = errno;
        close_fd(out[0]);
        close_fd(out[1]);
        return std::unexpected(BrowserError::io(std::strerror(e)));
    }
    posix_spawn_file_actions_t actions;
    posix_spawn_file_actions_init(&actions);
    posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0);
    if (pipe_stdout) {
        posix_spawn_file_actions_adddup2(&actions, out[1], 1);
    } else {
        posix_spawn_file_actions_addopen(&actions, 1, "/dev/null", O_WRONLY, 0);
    }
    posix_spawn_file_actions_adddup2(&actions, err[1], 2);

    std::map<std::string, std::string> vars;
    for (char** e = environ; e && *e; ++e) {
        std::string_view entry(*e);
        const auto eq = entry.find('=');
        if (eq != std::string_view::npos) vars.insert_or_assign(std::string(entry.substr(0, eq)), std::string(entry.substr(eq + 1)));
    }
    for (const auto& [k, v] : env) vars.insert_or_assign(k, v);
    std::vector<std::string> env_strings;
    env_strings.reserve(vars.size());
    for (const auto& [k, v] : vars) env_strings.push_back(k + "=" + v);
    std::vector<char*> envp;
    for (auto& s : env_strings) envp.push_back(s.data());
    envp.push_back(nullptr);

    const auto program_str = program.string();
    std::vector<std::string> argv_strings{program_str};
    argv_strings.insert(argv_strings.end(), args.begin(), args.end());
    std::vector<char*> argv;
    for (auto& s : argv_strings) argv.push_back(s.data());
    argv.push_back(nullptr);

    pid_t pid = -1;
    const int rc = ::posix_spawn(&pid, program_str.c_str(), &actions, nullptr, argv.data(), envp.data());
    posix_spawn_file_actions_destroy(&actions);
    close_fd(out[1]);
    close_fd(err[1]);
    if (rc != 0) {
        close_fd(out[0]);
        close_fd(err[0]);
        return std::unexpected(BrowserError::io(std::strerror(rc)));
    }
    return Process(pid, out[0], err[0]);
}

Process::Process(Process&& other) noexcept
    : pid_(std::exchange(other.pid_, -1)), reaped_(other.reaped_), out_(std::exchange(other.out_, -1)),
      err_(std::exchange(other.err_, -1)) {}

Process& Process::operator=(Process&& other) noexcept {
    if (this != &other) {
        reset();
        pid_ = std::exchange(other.pid_, -1);
        reaped_ = other.reaped_;
        out_ = std::exchange(other.out_, -1);
        err_ = std::exchange(other.err_, -1);
    }
    return *this;
}

Process::~Process() { reset(); }

void Process::reset() noexcept {
    if (pid_ > 0 && !reaped_) {
        ::kill(pid_, SIGKILL);
        int status = 0;
        while (::waitpid(pid_, &status, 0) < 0 && errno == EINTR) {
        }
    }
    pid_ = -1;
    close_fd(out_);
    close_fd(err_);
}

void Process::kill() noexcept {
    if (pid_ > 0 && !reaped_) ::kill(pid_, SIGKILL);
}

bool Process::try_wait() noexcept {
    if (pid_ <= 0 || reaped_) return true;
    int status = 0;
    const pid_t r = ::waitpid(pid_, &status, WNOHANG);
    if (r == pid_ || (r < 0 && errno == ECHILD)) reaped_ = true;
    return reaped_;
}

asio::awaitable<bool> Process::wait(std::chrono::milliseconds timeout) {
    const auto deadline = std::chrono::steady_clock::now() + timeout;
    while (!try_wait()) {
        if (std::chrono::steady_clock::now() >= deadline) co_return false;
        co_await sleep_for(std::chrono::milliseconds(10));
    }
    co_return true;
}

void drain_lines(int fd, std::string target) {
    if (fd < 0) return;
    std::thread([fd, target = std::move(target)] {
        std::string line;
        char buf[4096];
        for (;;) {
            const auto n = ::read(fd, buf, sizeof buf);
            if (n < 0 && errno == EINTR) continue;
            if (n <= 0) break;
            for (ssize_t i = 0; i < n; ++i) {
                if (buf[i] == '\n') {
                    loa::runtime::log::debug(target, line);
                    line.clear();
                } else {
                    line.push_back(buf[i]);
                }
            }
        }
        ::close(fd);
    }).detach();
}

asio::awaitable<std::optional<std::string>> read_line(int fd, std::chrono::steady_clock::time_point deadline) {
    const int flags = ::fcntl(fd, F_GETFL);
    ::fcntl(fd, F_SETFL, flags | O_NONBLOCK);
    std::string line;
    std::optional<std::string> result;
    for (;;) {
        char c = 0;
        const auto n = ::read(fd, &c, 1);
        if (n == 1) {
            if (c == '\n') {
                result = std::move(line);
                break;
            }
            line.push_back(c);
            continue;
        }
        if (n < 0 && errno == EINTR) continue;
        if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK) && std::chrono::steady_clock::now() < deadline) {
            co_await sleep_for(std::chrono::milliseconds(10));
            continue;
        }
        break;
    }
    ::fcntl(fd, F_SETFL, flags);
    co_return result;
}

} // namespace loa::surfaces::browser::detail
