#include "loa/runtime/exec.hpp"

#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <spawn.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include <algorithm>
#include <condition_variable>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <mutex>
#include <ranges>

#include "sys_error.hpp"

extern char** environ;

namespace loa::runtime::exec {

using detail::os_error;

namespace {

std::optional<std::string_view> env_view(const char* key) {
    const char* v = std::getenv(key);
    return v == nullptr ? std::nullopt : std::optional<std::string_view>(v);
}

std::optional<Path> first_executable(const Path& base, [[maybe_unused]] std::optional<std::string_view> pathext,
                                     [[maybe_unused]] bool searched) {
#ifdef _WIN32
    const bool has_ext = base.has_extension();
    if (has_ext && is_executable(base)) return base;
    if (searched || !has_ext) {
        std::string exts(pathext.value_or(""));
        if (exts.find_first_not_of(" \t") == std::string::npos) exts = ".COM;.EXE;.BAT;.CMD";
        for (auto part : exts | std::views::split(';')) {
            std::string ext(part.begin(), part.end());
            ext.erase(0, ext.find_first_not_of(" \t"));
            ext.erase(ext.find_last_not_of(" \t") + 1);
            if (ext.empty()) continue;
            std::ranges::transform(ext, ext.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
            auto name = base.native();
            if (!ext.starts_with('.')) name += L".";
            name += Path(ext).native();
            Path candidate(name);
            if (is_executable(candidate)) return candidate;
        }
    }
    return std::nullopt;
#else
    if (is_executable(base)) return base;
    return std::nullopt;
#endif
}

// Closes a descriptor on scope exit.
struct Fd {
    int fd = -1;
    Fd() = default;
    explicit Fd(int f) : fd(f) {}
    Fd(const Fd&) = delete;
    Fd& operator=(const Fd&) = delete;
    ~Fd() {
        if (fd >= 0) ::close(fd);
    }
    int release() { return std::exchange(fd, -1); }
};

struct Spawned {
    pid_t pid = -1;
    int stdin_w = -1;
    int out_r = -1;
    int err_r = -1;
};

// posix_spawnp with stdin from /dev/null (or a pipe when `pipe_stdin`), stdout and
// stderr piped (or inherited when `pipe_output` is false).
std::expected<Spawned, std::error_code> spawn_raw(const Command& cmd, bool pipe_stdin, bool pipe_output) {
    int in[2] = {-1, -1}, out[2] = {-1, -1}, err[2] = {-1, -1};
    auto fail = [&](int e) {
        for (int fd : {in[0], in[1], out[0], out[1], err[0], err[1]})
            if (fd >= 0) ::close(fd);
        return std::unexpected(std::error_code(e, std::generic_category()));
    };
    if (pipe_stdin && ::pipe2(in, O_CLOEXEC) != 0) return fail(errno);
    if (pipe_output && (::pipe2(out, O_CLOEXEC) != 0 || ::pipe2(err, O_CLOEXEC) != 0)) return fail(errno);

    posix_spawn_file_actions_t actions;
    posix_spawn_file_actions_init(&actions);
    if (pipe_stdin) posix_spawn_file_actions_adddup2(&actions, in[0], 0);
    else posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0);
    if (pipe_output) {
        posix_spawn_file_actions_adddup2(&actions, out[1], 1);
        posix_spawn_file_actions_adddup2(&actions, err[1], 2);
    }
    if (cmd.current_dir) posix_spawn_file_actions_addchdir_np(&actions, cmd.current_dir->c_str());

    std::vector<std::string> args;
    args.push_back(cmd.program.string());
    args.insert(args.end(), cmd.args.begin(), cmd.args.end());
    std::vector<char*> argv;
    for (auto& a : args) argv.push_back(a.data());
    argv.push_back(nullptr);

    std::vector<std::string> env;
    for (char** e = environ; *e != nullptr; ++e) {
        std::string_view entry(*e);
        const auto key = entry.substr(0, entry.find('='));
        const bool overridden = std::ranges::any_of(cmd.env, [&](const auto& kv) { return kv.first == key; });
        const bool removed = std::ranges::find(cmd.env_remove, key) != cmd.env_remove.end();
        if (!overridden && !removed) env.emplace_back(entry);
    }
    for (const auto& [k, v] : cmd.env) env.push_back(k + "=" + v);
    std::vector<char*> envp;
    for (auto& e : env) envp.push_back(e.data());
    envp.push_back(nullptr);

    // A child's own PATH decides where a bare name is searched, as Rust's Command does.
    pid_t pid = -1;
    int rc = 0;
    const auto path_override = std::ranges::find_if(cmd.env, [](const auto& kv) { return kv.first == "PATH"; });
    if (path_override != cmd.env.end() && cmd.program.native().find('/') == std::string::npos) {
        auto found = which_in(cmd.program, path_override->second);
        std::string prog = found ? found->string() : cmd.program.string();
        rc = ::posix_spawn(&pid, prog.c_str(), &actions, nullptr, argv.data(), envp.data());
    } else {
        rc = ::posix_spawnp(&pid, cmd.program.c_str(), &actions, nullptr, argv.data(), envp.data());
    }
    posix_spawn_file_actions_destroy(&actions);
    if (rc != 0) return fail(rc);

    Spawned s{pid, in[1], out[0], err[0]};
    for (int fd : {in[0], out[1], err[1]})
        if (fd >= 0) ::close(fd);
    return s;
}

std::string first_stderr(std::string_view stderr_text) {
    for (std::size_t pos = 0; pos <= stderr_text.size();) {
        auto nl = stderr_text.find('\n', pos);
        auto line = stderr_text.substr(pos, nl == std::string_view::npos ? std::string_view::npos : nl - pos);
        const auto b = line.find_first_not_of(" \t\r\v\f");
        if (b != std::string_view::npos) {
            const auto e = line.find_last_not_of(" \t\r\v\f");
            return std::string(line.substr(b, e - b + 1));
        }
        if (nl == std::string_view::npos) break;
        pos = nl + 1;
    }
    return "no output";
}

// Rust's `String::from_utf8_lossy`: each maximal invalid sequence becomes U+FFFD.
std::string lossy(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    std::size_t i = 0;
    while (i < s.size()) {
        const auto b = static_cast<unsigned char>(s[i]);
        std::size_t need = 0;
        unsigned char lo = 0x80, hi = 0xBF;
        if (b < 0x80) need = 0;
        else if (b >= 0xC2 && b <= 0xDF) need = 1;
        else if (b >= 0xE0 && b <= 0xEF) {
            need = 2;
            if (b == 0xE0) lo = 0xA0;
            if (b == 0xED) hi = 0x9F;
        } else if (b >= 0xF0 && b <= 0xF4) {
            need = 3;
            if (b == 0xF0) lo = 0x90;
            if (b == 0xF4) hi = 0x8F;
        } else {
            out += "\xEF\xBF\xBD";
            ++i;
            continue;
        }
        std::size_t k = 1;
        for (; k <= need; ++k) {
            if (i + k >= s.size()) break;
            const auto c = static_cast<unsigned char>(s[i + k]);
            if (c < (k == 1 ? lo : 0x80) || c > (k == 1 ? hi : 0xBF)) break;
        }
        if (k == need + 1) {
            out.append(s.substr(i, need + 1));
            i += need + 1;
        } else {
            out += "\xEF\xBF\xBD";
            i += k;
        }
    }
    return out;
}

std::string join(const std::vector<std::string>& args) {
    std::string out;
    for (std::size_t i = 0; i < args.size(); ++i) out += (i ? " " : "") + args[i];
    return out;
}

} // namespace

std::expected<void, Error> write_executable(const Path& path, std::string_view contents) {
    Command sh{"/bin/sh", {"-c", R"(cat > "$1" && chmod 755 "$1")", "sh", path.string()}, {}, {}};
    auto spawned = spawn_raw(sh, true, false);
    if (!spawned) return std::unexpected(os_error(spawned.error(), "/bin/sh"));
    int write_err = 0;
    {
        Fd in(spawned->stdin_w);
        // A reader that exits early must not kill us with SIGPIPE.
        sigset_t pipe_set, old;
        sigemptyset(&pipe_set);
        sigaddset(&pipe_set, SIGPIPE);
        pthread_sigmask(SIG_BLOCK, &pipe_set, &old);
        while (!contents.empty()) {
            const auto n = ::write(in.fd, contents.data(), contents.size());
            if (n < 0) {
                if (errno == EINTR) continue;
                write_err = errno;
                break;
            }
            contents.remove_prefix(static_cast<std::size_t>(n));
        }
        if (write_err == EPIPE) {
            timespec zero{};
            sigtimedwait(&pipe_set, nullptr, &zero);
        }
        pthread_sigmask(SIG_SETMASK, &old, nullptr);
    }
    int status = 0;
    while (::waitpid(spawned->pid, &status, 0) < 0) {
        if (errno != EINTR) return std::unexpected(os_error(errno, "waiting for /bin/sh"));
    }
    if (write_err != 0) return std::unexpected(os_error(write_err, "writing " + path.string()));
    if (!ExitStatus{status}.success())
        return std::unexpected(Error(ErrorCode::io, "writing " + path.string() + " failed: " + ExitStatus{status}.to_string()));
    return {};
}

bool is_executable(const Path& file) {
    struct stat st {};
    return ::stat(file.c_str(), &st) == 0 && S_ISREG(st.st_mode) && (st.st_mode & 0111) != 0;
}

std::optional<Path> which_in(const Path& program, std::optional<std::string_view> path_var, std::optional<std::string_view> pathext) {
    if (program.empty()) return std::nullopt;
    const auto parts = std::distance(program.begin(), program.end());
    if (parts > 1 || program.has_root_directory()) return first_executable(program, pathext, false);
    if (!path_var) return std::nullopt;
#ifdef _WIN32
    constexpr char sep = ';';
#else
    constexpr char sep = ':';
#endif
    for (auto part : *path_var | std::views::split(sep)) {
        std::string_view dir(part.begin(), part.end());
        if (dir.empty()) continue;
        if (auto found = first_executable(Path(dir) / program, pathext, true)) return found;
    }
    return std::nullopt;
}

std::optional<Path> which(const Path& program) { return which_in(program, env_view("PATH"), env_view("PATHEXT")); }

Path resolve(const Path& program) { return resolve_in(program, std::nullopt); }

Path resolve_in(const Path& program, std::optional<std::string_view> path_var) {
    auto found = which_in(program, path_var ? path_var : env_view("PATH"), env_view("PATHEXT"));
    return found ? *found : program;
}

bool is_path_var(std::string_view name) noexcept {
#ifdef _WIN32
    return name.size() == 4 && std::ranges::equal(name, std::string_view("PATH"),
                                                   [](char a, char b) { return std::toupper(a) == b; });
#else
    return name == "PATH";
#endif
}

Command command(const Path& program, std::vector<std::string> args, std::vector<std::pair<std::string, std::string>> env,
                std::optional<Path> current_dir) {
    return Command{resolve(program), std::move(args), std::move(env), std::move(current_dir)};
}

bool ExitStatus::success() const noexcept { return WIFEXITED(raw) && WEXITSTATUS(raw) == 0; }

std::optional<int> ExitStatus::code() const noexcept {
    return WIFEXITED(raw) ? std::optional<int>(WEXITSTATUS(raw)) : std::nullopt;
}

std::string ExitStatus::to_string() const {
    if (WIFEXITED(raw)) return "exit status: " + std::to_string(WEXITSTATUS(raw));
    if (WIFSIGNALED(raw)) {
        const int sig = WTERMSIG(raw);
        std::string out = "signal: " + std::to_string(sig);
        if (const char* name = sigabbrev_np(sig)) out += std::string(" (SIG") + name + ")";
        if (WCOREDUMP(raw)) out += " (core dumped)";
        return out;
    }
    return "unrecognized wait status: " + std::to_string(raw);
}

std::expected<Child, Error> Child::spawn(const Command& cmd) {
    auto spawned = retry_busy([&] { return spawn_raw(cmd, false, true); });
    if (!spawned) return std::unexpected(os_error(spawned.error(), ""));
    return Child(spawned->pid, spawned->out_r, spawned->err_r);
}

Child::Child(Child&& other) noexcept
    : pid_(std::exchange(other.pid_, -1)), out_(std::exchange(other.out_, -1)), err_(std::exchange(other.err_, -1)) {}

Child& Child::operator=(Child&& other) noexcept {
    if (this != &other) {
        reset();
        pid_ = std::exchange(other.pid_, -1);
        out_ = std::exchange(other.out_, -1);
        err_ = std::exchange(other.err_, -1);
    }
    return *this;
}

Child::~Child() { reset(); }

void Child::reset() noexcept {
    if (out_ >= 0) ::close(std::exchange(out_, -1));
    if (err_ >= 0) ::close(std::exchange(err_, -1));
    if (pid_ > 0) {
        ::kill(pid_, SIGKILL);
        while (::waitpid(pid_, nullptr, 0) < 0 && errno == EINTR) {}
        pid_ = -1;
    }
}

std::expected<Output, Error> Child::wait_with_output(std::optional<std::chrono::milliseconds> timeout) {
    using Clock = std::chrono::steady_clock;
    const auto deadline = timeout ? std::optional(Clock::now() + *timeout) : std::nullopt;
    Output result;
    std::string* sinks[2] = {&result.out, &result.err};
    int* fds[2] = {&out_, &err_};
    char buf[65536];
    while (out_ >= 0 || err_ >= 0) {
        pollfd p[2];
        nfds_t n = 0;
        int which_fd[2];
        for (int k = 0; k < 2; ++k)
            if (*fds[k] >= 0) {
                p[n] = {*fds[k], POLLIN, 0};
                which_fd[n++] = k;
            }
        int wait_ms = -1;
        if (deadline) {
            const auto left = std::chrono::duration_cast<std::chrono::milliseconds>(*deadline - Clock::now()).count();
            if (left <= 0) return std::unexpected(Error(ErrorCode::timeout, "timed out"));
            wait_ms = static_cast<int>(std::min<long long>(left, INT32_MAX));
        }
        const int rc = ::poll(p, n, wait_ms);
        if (rc < 0) {
            if (errno == EINTR) continue;
            return std::unexpected(os_error(errno, "poll"));
        }
        if (rc == 0) return std::unexpected(Error(ErrorCode::timeout, "timed out"));
        for (nfds_t i = 0; i < n; ++i) {
            if (p[i].revents == 0) continue;
            const int k = which_fd[i];
            const auto got = ::read(*fds[k], buf, sizeof buf);
            if (got < 0 && errno == EINTR) continue;
            if (got <= 0) {
                ::close(std::exchange(*fds[k], -1));
                continue;
            }
            sinks[k]->append(buf, static_cast<std::size_t>(got));
        }
    }
    int status = 0;
    for (;;) {
        const pid_t r = ::waitpid(pid_, &status, deadline ? WNOHANG : 0);
        if (r == pid_) break;
        if (r < 0 && errno == EINTR) continue;
        if (r < 0) return std::unexpected(os_error(errno, "waitpid"));
        // Streams closed but the child lingers: poll its exit until the deadline.
        if (Clock::now() >= *deadline) return std::unexpected(Error(ErrorCode::timeout, "timed out"));
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    pid_ = -1;
    result.status.raw = status;
    return result;
}

std::expected<Output, Error> output(const Command& cmd, std::optional<std::chrono::milliseconds> timeout) {
    auto child = Child::spawn(cmd);
    if (!child) return std::unexpected(child.error());
    return child->wait_with_output(timeout);
}

std::expected<std::string, Error> probe_cli(const Path& exe, const std::vector<std::string>& args, std::chrono::milliseconds timeout) {
    const auto shown = exe.string();
    auto out = output(Command{exe, args, {}, {}}, timeout);
    if (!out && out.error().code() == ErrorCode::timeout)
        return std::unexpected(Error(ErrorCode::timeout, "`" + shown + "` didn't answer " + join(args)));
    if (!out) return std::unexpected(Error(out.error().code(), "`" + shown + "` didn't run: " + out.error().message()));
    if (!out->status.success()) {
        const auto cmdline = shown + " " + join(args);
        return std::unexpected(
            Error(ErrorCode::io, "`" + cmdline + "` failed (" + out->status.to_string() + "): " + first_stderr(lossy(out->err))));
    }
    return lossy(out->out);
}

std::expected<std::string, std::string> run_checked(const Command& cmd) {
    auto out = output(cmd);
    if (!out) return std::unexpected(out.error().to_string());
    if (!out->status.success()) return std::unexpected(out->status.to_string() + ": " + first_stderr(lossy(out->err)));
    return lossy(out->out);
}

struct StderrTail::State {
    std::mutex mutex;
    std::condition_variable cv;
    std::deque<std::string> ring;
    bool closed = false;
};

StderrTail StderrTail::spawn(int fd, std::size_t lines) {
    auto state = std::make_shared<State>();
    const auto cap = std::max<std::size_t>(lines, 1);
    std::thread([state, fd, cap] {
        auto push = [&](std::string_view raw) {
            auto text = lossy(raw);
            text.erase(text.find_last_not_of(" \t\n\r\v\f") + 1);
            std::scoped_lock lock(state->mutex);
            if (state->ring.size() >= cap) state->ring.pop_front();
            state->ring.push_back(std::move(text));
        };
        std::string pending;
        char buf[4096];
        for (;;) {
            const auto n = ::read(fd, buf, sizeof buf);
            if (n < 0 && errno == EINTR) continue;
            if (n <= 0) break;
            pending.append(buf, static_cast<std::size_t>(n));
            for (auto nl = pending.find('\n'); nl != std::string::npos; nl = pending.find('\n')) {
                push(std::string_view(pending).substr(0, nl + 1));
                pending.erase(0, nl + 1);
            }
        }
        if (!pending.empty()) push(pending);
        ::close(fd);
        std::scoped_lock lock(state->mutex);
        state->closed = true;
        state->cv.notify_all();
    }).detach();
    return StderrTail(std::move(state));
}

std::string StderrTail::joined() const {
    std::scoped_lock lock(state_->mutex);
    std::string out;
    for (std::size_t i = 0; i < state_->ring.size(); ++i) out += (i ? "\n" : "") + state_->ring[i];
    return out;
}

bool StderrTail::wait_closed(std::chrono::milliseconds timeout) const {
    std::unique_lock lock(state_->mutex);
    return state_->cv.wait_for(lock, timeout, [&] { return state_->closed; });
}

std::string shell_quote(std::string_view s) {
    std::string out = "'";
    for (const char c : s) {
        if (c == '\'') out += R"('\'')";
        else out += c;
    }
    return out + "'";
}

std::optional<std::string> parse_version(std::string_view text) {
    const auto alnum = [](char c) { return std::isalnum(static_cast<unsigned char>(c)) != 0; };
    const auto digit = [](char c) { return c >= '0' && c <= '9'; };
    const auto word_start = [&](std::size_t i) { return i == 0 || !alnum(text[i - 1]); };
    const auto trim_dots = [](std::string_view s) {
        while (s.ends_with('.')) s.remove_suffix(1);
        return s;
    };
    for (std::size_t start = 0; start < text.size(); ++start) {
        if (!digit(text[start]) || !(word_start(start) || (text[start - 1] == 'v' && word_start(start - 1)))) continue;
        const auto rest = text.substr(start);
        std::size_t core = 0;
        while (core < rest.size() && (digit(rest[core]) || rest[core] == '.')) ++core;
        const auto numbers = trim_dots(rest.substr(0, core));
        if (numbers.find('.') == std::string_view::npos) continue;
        const auto tail = rest.substr(core);
        if (numbers.size() == core && (tail.starts_with('-') || tail.starts_with('+'))) {
            std::size_t suffix = 0;
            while (suffix < tail.size() && (alnum(tail[suffix]) || tail[suffix] == '-' || tail[suffix] == '+' || tail[suffix] == '.'))
                ++suffix;
            return std::string(trim_dots(rest.substr(0, core + suffix)));
        }
        return std::string(numbers);
    }
    return std::nullopt;
}

bool kvm_available() noexcept {
    const int fd = ::open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (fd < 0) return false;
    ::close(fd);
    return true;
}

} // namespace loa::runtime::exec
