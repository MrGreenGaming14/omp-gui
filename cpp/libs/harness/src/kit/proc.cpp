#include "loa/harness/kit/proc.hpp"

#include <algorithm>
#include <cerrno>
#include <cstring>
#include <fcntl.h>
#include <format>
#include <map>
#include <signal.h>
#include <spawn.h>
#include <sys/wait.h>
#include <unistd.h>

#include "utf8.hpp"

extern char** environ;

namespace loa::harness::kit {

namespace exec = loa::runtime::exec;

Fd& Fd::operator=(Fd&& other) noexcept {
    if (this != &other) {
        reset();
        fd_ = std::exchange(other.fd_, -1);
    }
    return *this;
}

void Fd::reset() noexcept {
    if (fd_ >= 0) ::close(fd_);
    fd_ = -1;
}

ChildProcess& ChildProcess::operator=(ChildProcess&& other) noexcept {
    if (this != &other) {
        if (pid_ > 0) {
            kill();
            (void)wait();
        }
        pid_ = std::exchange(other.pid_, -1);
    }
    return *this;
}

ChildProcess::~ChildProcess() {
    if (pid_ > 0) {
        kill();
        (void)wait();
    }
}

std::optional<ExitStatus> ChildProcess::try_wait() {
    if (pid_ <= 0) return std::nullopt;
    int status = 0;
    const pid_t r = ::waitpid(pid_, &status, WNOHANG);
    if (r == pid_) {
        pid_ = -1;
        return ExitStatus{status};
    }
    if (r < 0 && errno != EINTR) pid_ = -1;
    return std::nullopt;
}

void ChildProcess::kill() noexcept {
    if (pid_ > 0) ::kill(pid_, SIGKILL);
}

std::optional<ExitStatus> ChildProcess::wait() {
    if (pid_ <= 0) return std::nullopt;
    int status = 0;
    pid_t r;
    do r = ::waitpid(pid_, &status, 0);
    while (r < 0 && errno == EINTR);
    pid_ = -1;
    if (r < 0) return std::nullopt;
    return ExitStatus{status};
}

namespace {

std::expected<std::pair<Fd, Fd>, std::error_code> make_pipe() {
    int fds[2];
    if (::pipe2(fds, O_CLOEXEC) != 0) return std::unexpected(std::error_code(errno, std::system_category()));
    return std::pair{Fd(fds[0]), Fd(fds[1])};
}

// The app's environment with `overrides` on top.
std::vector<std::string> environment(const std::vector<std::pair<std::string, std::string>>& overrides,
                                     const std::vector<std::string>& remove) {
    std::map<std::string, std::string> vars;
    std::vector<std::string> order;
    for (char** e = environ; e && *e; ++e) {
        std::string_view kv(*e);
        const auto eq = kv.find('=');
        if (eq == std::string_view::npos) continue;
        std::string key(kv.substr(0, eq));
        if (std::ranges::find(remove, key) != remove.end()) continue;
        if (vars.emplace(key, std::string(kv.substr(eq + 1))).second) order.push_back(std::move(key));
    }
    for (const auto& [k, v] : overrides) {
        if (vars.insert_or_assign(k, v).second) order.push_back(k);
    }
    std::vector<std::string> out;
    out.reserve(order.size());
    for (const auto& k : order) out.push_back(k + "=" + vars[k]);
    return out;
}

} // namespace

RpcResult<Piped> spawn_piped(const exec::Command& cmd) {
    auto in = make_pipe();
    if (!in) return std::unexpected(spawn_error(in.error(), cmd));
    auto out = make_pipe();
    if (!out) return std::unexpected(spawn_error(out.error(), cmd));
    auto err = make_pipe();
    if (!err) return std::unexpected(spawn_error(err.error(), cmd));

    const std::string program = cmd.program.string();
    std::vector<std::string> args{program};
    args.insert(args.end(), cmd.args.begin(), cmd.args.end());
    std::vector<char*> argv;
    for (auto& a : args) argv.push_back(a.data());
    argv.push_back(nullptr);
    std::vector<std::string> env = environment(cmd.env, cmd.env_remove);
    std::vector<char*> envp;
    for (auto& e : env) envp.push_back(e.data());
    envp.push_back(nullptr);

    auto spawned = exec::retry_busy([&]() -> std::expected<pid_t, std::error_code> {
        posix_spawn_file_actions_t actions;
        posix_spawn_file_actions_init(&actions);
        posix_spawn_file_actions_adddup2(&actions, in->first.get(), 0);
        posix_spawn_file_actions_adddup2(&actions, out->second.get(), 1);
        posix_spawn_file_actions_adddup2(&actions, err->second.get(), 2);
        if (cmd.current_dir) posix_spawn_file_actions_addchdir_np(&actions, cmd.current_dir->c_str());
        pid_t pid = -1;
        const bool search = program.find('/') == std::string::npos;
        const int rc = search ? ::posix_spawnp(&pid, program.c_str(), &actions, nullptr, argv.data(), envp.data())
                              : ::posix_spawn(&pid, program.c_str(), &actions, nullptr, argv.data(), envp.data());
        posix_spawn_file_actions_destroy(&actions);
        if (rc != 0) return std::unexpected(std::error_code(rc, std::system_category()));
        return pid;
    });
    if (!spawned) return std::unexpected(spawn_error(spawned.error(), cmd));
    ChildProcess child(*spawned);
    // The child's ends close here; the parent keeps its own.
    in->first.reset();
    out->second.reset();
    err->second.reset();
    auto tail = exec::StderrTail::spawn(err->first.release(), STDERR_LINES);
    return Piped{std::move(child), std::move(in->second), std::move(out->first), std::move(tail)};
}

RpcError spawn_error(std::error_code err, const exec::Command& cmd) {
    if (err == std::errc::no_such_file_or_directory) return RpcError::NotFound{cmd.program.string()};
    // Rust's `io::Error` display for OS errors.
    return RpcError::Io{loa::Error(ErrorCode::io, std::format("{} (os error {})", err.message(), err.value()))};
}

void TailBuffer::push(std::string_view chunk) {
    bytes_.append(chunk);
    if (!limit_ || bytes_.size() <= *limit_) return;
    std::size_t cut = bytes_.size() - *limit_;
    while (cut < bytes_.size() && (static_cast<unsigned char>(bytes_[cut]) & 0xC0) == 0x80) ++cut;
    bytes_.erase(0, cut);
    truncated_ = true;
}

std::string TailBuffer::text() const { return detail::utf8_lossy(bytes_); }

} // namespace loa::harness::kit
