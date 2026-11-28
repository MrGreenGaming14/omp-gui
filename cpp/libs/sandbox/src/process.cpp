#include "process.hpp"

#include <fcntl.h>
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cerrno>
#include <condition_variable>
#include <mutex>
#include <cstring>
#include <format>
#include <system_error>
#include <thread>

#include "loa/runtime/exec.hpp"

extern char** environ;

namespace loa::sandbox {

namespace error {
loa::Error stub(std::string message) { return loa::Error(ErrorCode::internal, "sandbox: " + message); }
loa::Error disconnected() { return loa::Error(ErrorCode::io, "the sandbox stub disconnected"); }
loa::Error provider(std::string_view provider, std::string message) {
    return loa::Error(ErrorCode::unknown, std::format("{}: {}", provider, message));
}
loa::Error isolation(std::string message) { return loa::Error(ErrorCode::internal, "isolation: " + message); }
loa::Error args(std::string message) { return loa::Error(ErrorCode::invalid_argument, "invalid arguments: " + message); }
loa::Error settings(std::string message) { return loa::Error(ErrorCode::parse, "invalid sandbox settings: " + message); }
loa::Error no_provider(std::string_view kind) {
    return loa::Error(ErrorCode::not_found, std::format("no provider for sandbox kind `{}`", kind));
}
loa::Error os(std::string_view what, int err) {
    const auto code = err == ENOENT ? ErrorCode::not_found : err == EACCES || err == EPERM ? ErrorCode::permission_denied : ErrorCode::io;
    auto text = std::format("{} (os error {})", std::generic_category().message(err), err);
    if (!what.empty()) text = std::format("{}: {}", what, text);
    return loa::Error(code, std::move(text));
}
} // namespace error

FdReader::~FdReader() {
    if (fd_ >= 0) ::close(fd_);
}

Result<std::size_t> FdReader::read(std::span<char> buf) {
    for (;;) {
        const auto n = ::read(fd_, buf.data(), buf.size());
        if (n >= 0) return static_cast<std::size_t>(n);
        if (errno != EINTR) return std::unexpected(error::os("", errno));
    }
}

FdWriter::~FdWriter() {
    if (fd_ >= 0) ::close(fd_);
}

Result<> FdWriter::write_all(std::string_view bytes) {
    if (fd_ < 0) return std::unexpected(error::os("", EPIPE));
    // A closed reader must fail the write, not kill the process: SIGPIPE is blocked on this
    // thread for the write and a pending one consumed.
    sigset_t pipe, old;
    sigemptyset(&pipe);
    sigaddset(&pipe, SIGPIPE);
    pthread_sigmask(SIG_BLOCK, &pipe, &old);
    int failed = 0;
    while (!bytes.empty()) {
        const auto n = ::write(fd_, bytes.data(), bytes.size());
        if (n < 0) {
            if (errno == EINTR) continue;
            failed = errno;
            break;
        }
        bytes.remove_prefix(static_cast<std::size_t>(n));
    }
    if (failed == EPIPE) {
        const timespec zero{};
        while (sigtimedwait(&pipe, nullptr, &zero) > 0) {
        }
    }
    pthread_sigmask(SIG_SETMASK, &old, nullptr);
    if (failed) return std::unexpected(error::os("", failed));
    return {};
}

Result<> FdWriter::shutdown() {
    if (fd_ >= 0) ::close(std::exchange(fd_, -1));
    return {};
}

} // namespace loa::sandbox

namespace loa::sandbox::detail {

namespace {

void close_fd(int& fd) noexcept {
    if (fd >= 0) ::close(std::exchange(fd, -1));
}

struct Pipe {
    int read = -1;
    int write = -1;
    ~Pipe() {
        close_fd(read);
        close_fd(write);
    }
};

std::expected<Process, std::error_code> spawn_once(const Spawn& spec, const std::string& program,
                                                   std::vector<char*>& argv, std::vector<char*>& envp, int null_fd);

} // namespace

Result<Process> Process::spawn(const Spawn& spec) {
    // Everything the child needs is built before fork: only async-signal-safe calls follow it.
    // Searched like execvp: the child's `PATH` when it sets one, else this process's, else the
    // libc default.
    std::optional<std::string> path_var;
    for (const auto& [key, value] : spec.env) {
        if (key == "PATH") path_var = value;
    }
    if (!path_var && !spec.clear_env) {
        if (const char* own = std::getenv("PATH")) path_var = own;
    }
    const auto resolved = loa::runtime::exec::resolve_in(spec.program, path_var.value_or("/bin:/usr/bin")).string();
    std::vector<std::string> args{spec.program.string()};
    args.insert(args.end(), spec.args.begin(), spec.args.end());
    std::vector<std::string> env;
    if (!spec.clear_env) {
        for (char** e = environ; e && *e; ++e) {
            std::string_view entry(*e);
            const auto name = entry.substr(0, entry.find('='));
            bool replaced = false;
            for (const auto& [key, _] : spec.env) replaced |= key == name;
            if (!replaced) env.emplace_back(entry);
        }
    }
    for (const auto& [key, value] : spec.env) env.push_back(key + "=" + value);
    std::vector<char*> argv, envp;
    for (auto& a : args) argv.push_back(a.data());
    argv.push_back(nullptr);
    for (auto& e : env) envp.push_back(e.data());
    envp.push_back(nullptr);
    const int null_fd = ::open("/dev/null", O_RDWR | O_CLOEXEC);
    auto child = loa::runtime::exec::retry_busy([&] { return spawn_once(spec, resolved, argv, envp, null_fd); });
    if (null_fd >= 0) ::close(null_fd);
    if (!child) return std::unexpected(error::os("", child.error().value()));
    return std::move(*child);
}

namespace {

std::expected<Process, std::error_code> spawn_once(const Spawn& spec, const std::string& program,
                                                   std::vector<char*>& argv, std::vector<char*>& envp, int null_fd) {
    Pipe in, out, err, report;
    const auto make = [](Pipe& p) { return ::pipe2(&p.read, O_CLOEXEC) == 0; };
    // Pipe's two ints are adjacent, as pipe2 expects.
    static_assert(sizeof(Pipe) == 2 * sizeof(int));
    if ((spec.in == Stdio::piped && !make(in)) || (spec.out == Stdio::piped && !make(out)) ||
        (spec.err == Stdio::piped && !make(err)) || !make(report))
        return std::unexpected(std::error_code(errno, std::generic_category()));
    const std::string cwd = spec.cwd ? spec.cwd->string() : std::string();

    const pid_t pid = ::fork();
    if (pid < 0) return std::unexpected(std::error_code(errno, std::generic_category()));
    if (pid == 0) {
        sigset_t none;
        sigemptyset(&none);
        sigprocmask(SIG_SETMASK, &none, nullptr);
        signal(SIGPIPE, SIG_DFL);
        const auto fail = [&] {
            const int e = errno;
            (void)!::write(report.write, &e, sizeof e);
            _exit(127);
        };
        const auto wire = [&](Stdio mode, int piped, int target) {
            if (mode == Stdio::inherit) return;
            const int fd = mode == Stdio::piped ? piped : null_fd;
            if (fd < 0 || ::dup2(fd, target) < 0) fail();
        };
        wire(spec.in, in.read, 0);
        wire(spec.out, out.write, 1);
        wire(spec.err, err.write, 2);
        if (spec.new_group && ::setpgid(0, 0) < 0) fail();
        if (!cwd.empty() && ::chdir(cwd.c_str()) < 0) fail();
        ::execve(program.c_str(), argv.data(), envp.data());
        fail();
    }
    close_fd(report.write);
    int child_errno = 0;
    ssize_t n;
    do {
        n = ::read(report.read, &child_errno, sizeof child_errno);
    } while (n < 0 && errno == EINTR);
    if (n == sizeof child_errno) {
        int status;
        while (::waitpid(pid, &status, 0) < 0 && errno == EINTR) {
        }
        return std::unexpected(std::error_code(child_errno, std::generic_category()));
    }
    close_fd(in.read);
    close_fd(out.write);
    close_fd(err.write);
    return Process::adopt(pid, std::exchange(in.write, -1), std::exchange(out.read, -1), std::exchange(err.read, -1),
                          spec.kill_on_drop);
}

} // namespace

Process::Process(Process&& other) noexcept
    : pid_(std::exchange(other.pid_, -1)), in_(std::exchange(other.in_, -1)), out_(std::exchange(other.out_, -1)),
      err_(std::exchange(other.err_, -1)), kill_on_drop_(other.kill_on_drop_), status_(other.status_) {}

Process& Process::operator=(Process&& other) noexcept {
    if (this != &other) {
        reset();
        pid_ = std::exchange(other.pid_, -1);
        in_ = std::exchange(other.in_, -1);
        out_ = std::exchange(other.out_, -1);
        err_ = std::exchange(other.err_, -1);
        kill_on_drop_ = other.kill_on_drop_;
        status_ = other.status_;
    }
    return *this;
}

Process::~Process() { reset(); }

void Process::reset() noexcept {
    close_fd(in_);
    close_fd(out_);
    close_fd(err_);
    if (pid_ > 0 && !status_) {
        if (kill_on_drop_) {
            ::kill(pid_, SIGKILL);
            int status;
            while (::waitpid(pid_, &status, 0) < 0 && errno == EINTR) {
            }
        } else {
            // Reaped in the background so it never lingers as a zombie.
            std::thread([pid = pid_] {
                int status;
                while (::waitpid(pid, &status, 0) < 0 && errno == EINTR) {
                }
            }).detach();
        }
    }
    pid_ = -1;
}

Result<int> Process::wait() {
    if (status_) return *status_;
    int status = 0;
    while (::waitpid(pid_, &status, 0) < 0) {
        if (errno != EINTR) return std::unexpected(error::os("", errno));
    }
    status_ = status;
    return status;
}

Result<std::optional<int>> Process::try_wait() {
    if (status_) return status_;
    int status = 0;
    const auto r = ::waitpid(pid_, &status, WNOHANG);
    if (r < 0) return std::unexpected(error::os("", errno));
    if (r == 0) return std::optional<int>{};
    status_ = status;
    return status_;
}

void Process::kill() noexcept {
    if (pid_ > 0 && !status_) ::kill(pid_, SIGKILL);
}

bool Captured::success() const noexcept { return WIFEXITED(status) && WEXITSTATUS(status) == 0; }

Result<Captured> run(Spawn spec, std::string_view input, std::optional<std::chrono::milliseconds> timeout) {
    spec.in = Stdio::piped;
    spec.out = Stdio::piped;
    spec.err = Stdio::piped;
    auto child = Process::spawn(spec);
    if (!child) return std::unexpected(child.error());
    FdWriter in(child->take_stdin());
    FdReader out(child->take_stdout()), err(child->take_stderr());
    std::string input_copy(input);
    std::jthread feeder([&] {
        (void)in.write_all(input_copy);
        (void)in.shutdown();
    });
    Captured captured;
    std::jthread errors([&] {
        char buf[8192];
        while (auto n = err.read(buf)) {
            if (*n == 0) break;
            captured.err.append(buf, *n);
        }
    });
    std::mutex mutex;
    std::condition_variable wake;
    bool finished = false, expired = false;
    std::jthread watchdog;
    if (timeout) {
        watchdog = std::jthread([&, pid = child->pid(), deadline = std::chrono::steady_clock::now() + *timeout] {
            std::unique_lock lock(mutex);
            if (!wake.wait_until(lock, deadline, [&] { return finished; })) {
                ::kill(pid, SIGKILL);
                expired = true;
            }
        });
    }
    char buf[65536];
    while (auto n = out.read(buf)) {
        if (*n == 0) break;
        captured.out.append(buf, *n);
    }
    errors.join();
    feeder.join();
    {
        std::scoped_lock lock(mutex);
        finished = true;
    }
    wake.notify_all();
    if (watchdog.joinable()) watchdog.join();
    auto status = child->wait();
    if (!status) return std::unexpected(status.error());
    if (expired) return std::unexpected(loa::Error(ErrorCode::timeout, std::format("no answer within {} ms", timeout->count())));
    captured.status = *status;
    return captured;
}

std::string status_string(int raw) { return loa::runtime::exec::ExitStatus{raw}.to_string(); }

std::optional<int> exit_code(int raw) noexcept {
    if (WIFEXITED(raw)) return WEXITSTATUS(raw);
    return std::nullopt;
}

std::optional<int> exit_signal(int raw) noexcept {
    if (WIFSIGNALED(raw)) return WTERMSIG(raw);
    return std::nullopt;
}

std::string lossy(std::string_view s) {
    std::string out;
    std::size_t i = 0;
    while (i < s.size()) {
        const auto c = static_cast<std::uint8_t>(s[i]);
        std::size_t n = c < 0x80 ? 0 : (c & 0xe0) == 0xc0 ? 1 : (c & 0xf0) == 0xe0 ? 2 : (c & 0xf8) == 0xf0 ? 3 : 4;
        bool ok = n < 4 && i + n < s.size();
        std::uint32_t cp = n == 0 ? c : n == 1 ? c & 0x1f : n == 2 ? c & 0x0f : c & 0x07;
        for (std::size_t k = 1; ok && k <= n; ++k) {
            const auto cc = static_cast<std::uint8_t>(s[i + k]);
            ok = (cc & 0xc0) == 0x80;
            cp = (cp << 6) | (cc & 0x3f);
        }
        ok = ok && !((n == 1 && cp < 0x80) || (n == 2 && cp < 0x800) || (n == 3 && cp < 0x10000) || cp > 0x10ffff ||
                     (cp >= 0xd800 && cp <= 0xdfff));
        if (ok) {
            out.append(s.substr(i, n + 1));
            i += n + 1;
        } else {
            out += "\xEF\xBF\xBD";
            ++i;
        }
    }
    return out;
}

bool valid_utf8(std::string_view s) { return lossy(s) == s; }

std::string lossy_trim(std::string_view s) {
    auto out = lossy(s);
    const auto ws = " \t\n\r\f\v";
    const auto start = out.find_first_not_of(ws);
    if (start == std::string::npos) return {};
    return out.substr(start, out.find_last_not_of(ws) - start + 1);
}

} // namespace loa::sandbox::detail
