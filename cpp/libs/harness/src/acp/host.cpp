#include "loa/harness/acp/host.hpp"

#include <asio.hpp>

#include <cerrno>
#include <cstring>
#include <format>
#include <fstream>
#include <iterator>
#include <limits>
#include <optional>
#include <thread>
#include <utility>
#include <vector>

#if defined(_WIN32)
#include <windows.h>
#else
#include <fcntl.h>
#include <pty.h>
#include <signal.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>
#endif

#include "../kit/sync.hpp"
#include "loa/harness/acp/wire.hpp"
#include "loa/harness/kit/proc.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/fs.hpp"

namespace loa::harness::acp {

namespace {

namespace fs = std::filesystem;
using kit::Json;
using kit::RpcError;
using kit::RpcResult;
using loa::runtime::json::Object;

constexpr std::int64_t METHOD_NOT_FOUND = -32601;
constexpr std::int64_t INVALID_PARAMS = -32602;
constexpr std::int64_t INTERNAL = -32603;
constexpr std::int64_t RESOURCE_NOT_FOUND = -32002;

RpcError fail(std::int64_t code, std::string message) {
    return RpcError(RpcError::Command{"request", std::move(message), std::to_string(code)});
}

RpcError io_error(std::string_view path, const loa::Error& err) {
    std::int64_t code = INTERNAL;
    if (err.code() == ErrorCode::permission_denied) code = INVALID_PARAMS;
    else if (err.code() == ErrorCode::not_found) code = RESOURCE_NOT_FOUND;
    return fail(code, std::format("{}: {}", path, err.message()));
}

// An OS error as Rust's `io::Error` shows it.
loa::Error os_error(int err) {
    ErrorCode code = ErrorCode::io;
    if (err == ENOENT) code = ErrorCode::not_found;
    else if (err == EACCES || err == EPERM) code = ErrorCode::permission_denied;
    return loa::Error(code, std::format("{} (os error {})", std::strerror(err), err));
}

template <typename T>
RpcResult<T> params(wire::Decoded<T> decoded) {
    if (!decoded) return std::unexpected(fail(INVALID_PARAMS, std::move(decoded).error()));
    return std::move(*decoded);
}

RpcError unknown(std::string_view id) { return fail(INVALID_PARAMS, std::format("unknown terminal {}", id)); }

bool valid_utf8(std::string_view s) {
    std::size_t i = 0;
    while (i < s.size()) {
        const auto c = static_cast<unsigned char>(s[i]);
        std::size_t n = 0;
        std::uint32_t cp = 0;
        if (c < 0x80) { ++i; continue; }
        if ((c & 0xE0) == 0xC0) { n = 1; cp = c & 0x1F; }
        else if ((c & 0xF0) == 0xE0) { n = 2; cp = c & 0x0F; }
        else if ((c & 0xF8) == 0xF0) { n = 3; cp = c & 0x07; }
        else return false;
        for (std::size_t k = 1; k <= n; ++k) {
            if (i + k >= s.size()) return false;
            const auto cc = static_cast<unsigned char>(s[i + k]);
            if ((cc & 0xC0) != 0x80) return false;
            cp = (cp << 6) | (cc & 0x3F);
        }
        if ((n == 1 && cp < 0x80) || (n == 2 && cp < 0x800) || (n == 3 && (cp < 0x10000 || cp > 0x10FFFF)) ||
            (cp >= 0xD800 && cp <= 0xDFFF))
            return false;
        i += n + 1;
    }
    return true;
}

std::expected<std::string, loa::Error> read_to_string(const fs::path& path) {
    std::error_code ec;
    if (fs::is_directory(path, ec)) return std::unexpected(os_error(EISDIR));
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::unexpected(os_error(errno ? errno : ENOENT));
    std::string text{std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
    if (in.bad()) return std::unexpected(os_error(EIO));
    if (!valid_utf8(text)) return std::unexpected(loa::Error(ErrorCode::invalid_argument, "stream did not contain valid UTF-8"));
    return text;
}

std::expected<void, loa::Error> write_file(const fs::path& path, std::string_view content) {
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    if (!out) return std::unexpected(os_error(errno ? errno : EACCES));
    out.write(content.data(), static_cast<std::streamsize>(content.size()));
    out.flush();
    if (!out) return std::unexpected(os_error(EIO));
    return {};
}

// Whether `prefix` is a leading run of `path`'s components; the rest goes to `rest`.
bool strip_prefix(const fs::path& path, const fs::path& prefix, fs::path& rest) {
    auto p = path.begin();
    for (auto q = prefix.begin(); q != prefix.end(); ++q, ++p) {
        if (q->empty()) continue;
        if (p == path.end() || *p != *q) return false;
    }
    rest.clear();
    for (; p != path.end(); ++p)
        if (!p->empty()) rest /= *p;
    return true;
}

#if !defined(_WIN32)
std::string signal_name(int signal) {
    switch (signal) {
    case 1: return "SIGHUP";
    case 2: return "SIGINT";
    case 6: return "SIGABRT";
    case 9: return "SIGKILL";
    case 11: return "SIGSEGV";
    case 13: return "SIGPIPE";
    case 15: return "SIGTERM";
    default: return std::format("SIG{}", signal);
    }
}

Json exit_json(int status) {
    Json code;
    Json signal;
    if (WIFEXITED(status)) code = Json(std::int64_t{WEXITSTATUS(status)});
    if (WIFSIGNALED(status)) signal = Json(signal_name(WTERMSIG(status)));
    return Json(Object{{"exitCode", std::move(code)}, {"signal", std::move(signal)}});
}
#else
Json exit_json(DWORD status) {
    return Json(Object{{"exitCode", Json(static_cast<std::int64_t>(static_cast<std::int32_t>(status)))}, {"signal", Json()}});
}
#endif

} // namespace

struct Host::Terminal {
    std::mutex mutex;
    kit::TailBuffer output;
    std::optional<Json> exit;
    kit::detail::WaitList waiters;
    bool killed = false;
#if defined(_WIN32)
    HANDLE job = nullptr;
#else
    pid_t pid = -1;
    bool reaped = false;
#endif

    explicit Terminal(std::optional<std::size_t> limit) : output(limit) {}

    // Ends the command and everything it started; idempotent.
    void kill() {
        std::scoped_lock lock(mutex);
        if (killed) return;
        killed = true;
#if defined(_WIN32)
        if (job) TerminateJobObject(job, 1);
#else
        if (!reaped && pid > 0) ::kill(-pid, SIGKILL);
#endif
    }

    void finish(Json status) {
        {
            std::scoped_lock lock(mutex);
            exit = std::move(status);
        }
        waiters.notify_all();
    }
};

std::expected<std::shared_ptr<Host>, loa::Error> Host::create(const fs::path& cwd) {
    auto root = loa::runtime::fs::canonicalize(cwd);
    if (!root) return std::unexpected(std::move(root).error());
    return std::shared_ptr<Host>(new Host(std::move(*root)));
}

Host::~Host() { release_all(); }

std::expected<fs::path, loa::Error> Host::resolve(std::string_view path) const {
    const fs::path p(path);
    fs::path rel;
    if (!strip_prefix(p, root_, rel)) {
        if (p.is_absolute())
            return std::unexpected(loa::Error(ErrorCode::permission_denied, std::format("{} is outside the session directory", p.string())));
        rel = p;
    }
    return loa::runtime::fs::confine(root_, rel);
}

kit::Awaitable<RpcResult<Json>> Host::serve(std::string method, Json request) const {
    if (method == "fs/read_text_file") {
        auto req = params(wire::decode_read_text_file(request));
        if (!req) co_return std::unexpected(std::move(req).error());
        auto path = resolve(req->path);
        if (!path) co_return std::unexpected(io_error(req->path, path.error()));
        auto text = read_to_string(*path);
        if (!text) co_return std::unexpected(io_error(req->path, text.error()));
        const std::size_t skip = req->line ? (*req->line > 0 ? *req->line - 1 : 0) : 0;
        const std::size_t take = req->limit ? *req->limit : std::numeric_limits<std::size_t>::max();
        std::string out;
        std::size_t index = 0;
        std::size_t taken = 0;
        for (std::size_t start = 0; start < text->size() && taken < take;) {
            std::size_t end = text->find('\n', start);
            end = end == std::string::npos ? text->size() : end + 1;
            if (index >= skip) {
                out.append(*text, start, end - start);
                ++taken;
            }
            ++index;
            start = end;
        }
        co_return Json(Object{{"content", std::move(out)}});
    }
    if (method == "fs/write_text_file") {
        auto req = params(wire::decode_write_text_file(request));
        if (!req) co_return std::unexpected(std::move(req).error());
        auto path = resolve(req->path);
        if (!path) co_return std::unexpected(io_error(req->path, path.error()));
        if (path->has_parent_path()) {
            std::error_code ec;
            fs::create_directories(path->parent_path(), ec);
            if (ec) co_return std::unexpected(io_error(req->path, os_error(ec.value())));
        }
        auto written = write_file(*path, req->content);
        if (!written) co_return std::unexpected(io_error(req->path, written.error()));
        co_return Json();
    }
    if (method == "terminal/create") co_return create_terminal(request);
    if (method == "terminal/output") {
        auto terminal = this->terminal(request);
        if (!terminal) co_return std::unexpected(std::move(terminal).error());
        std::scoped_lock lock((*terminal)->mutex);
        const auto& t = **terminal;
        co_return Json(Object{{"output", t.output.text()}, {"truncated", t.output.truncated()}, {"exitStatus", t.exit.value_or(Json())}});
    }
    if (method == "terminal/wait_for_exit") {
        auto terminal = this->terminal(request);
        if (!terminal) co_return std::unexpected(std::move(terminal).error());
        auto t = std::move(*terminal);
        auto executor = co_await asio::this_coro::executor;
        for (;;) {
            std::shared_ptr<kit::detail::Signal> signal;
            {
                std::scoped_lock lock(t->mutex);
                if (t->exit) co_return *t->exit;
                signal = t->waiters.prepare(executor);
            }
            co_await kit::detail::WaitList::wait(signal);
        }
    }
    // `kill` keeps the terminal so its output stays readable; `release` forgets it.
    if (method == "terminal/kill") {
        auto terminal = this->terminal(request);
        if (!terminal) co_return std::unexpected(std::move(terminal).error());
        (*terminal)->kill();
        co_return Json(Object{});
    }
    if (method == "terminal/release") {
        auto ref = params(wire::decode_terminal_ref(request));
        if (!ref) co_return std::unexpected(std::move(ref).error());
        std::shared_ptr<Terminal> terminal;
        {
            std::scoped_lock lock(mutex_);
            auto it = terminals_.find(ref->terminal_id);
            if (it == terminals_.end()) co_return std::unexpected(unknown(ref->terminal_id));
            terminal = std::move(it->second);
            terminals_.erase(it);
        }
        terminal->kill();
        co_return Json(Object{});
    }
    co_return std::unexpected(fail(METHOD_NOT_FOUND, std::format("method not found: {}", method)));
}

RpcResult<std::shared_ptr<Host::Terminal>> Host::terminal(const Json& request) const {
    auto ref = params(wire::decode_terminal_ref(request));
    if (!ref) return std::unexpected(std::move(ref).error());
    std::scoped_lock lock(mutex_);
    auto it = terminals_.find(ref->terminal_id);
    if (it == terminals_.end()) return std::unexpected(unknown(ref->terminal_id));
    return it->second;
}

void Host::release_all() const {
    std::map<std::string, std::shared_ptr<Terminal>> drained;
    {
        std::scoped_lock lock(mutex_);
        drained.swap(terminals_);
    }
    for (auto& [id, terminal] : drained) terminal->kill();
}

RpcResult<Json> Host::create_terminal(const Json& raw) const {
    auto req = params(wire::decode_create_terminal(raw));
    if (!req) return std::unexpected(std::move(req).error());
    fs::path cwd = root_;
    if (req->cwd) {
        auto resolved = resolve(*req->cwd);
        if (!resolved) return std::unexpected(io_error(*req->cwd, resolved.error()));
        cwd = std::move(*resolved);
    }
    std::vector<wire::EnvVariable> env = req->env.value_or(std::vector<wire::EnvVariable>{});
    // The command is looked up on the PATH it runs with; the last entry wins, as in `envs`.
    std::optional<std::string_view> path_var;
    for (auto it = env.rbegin(); it != env.rend(); ++it)
        if (loa::runtime::exec::is_path_var(it->name)) { path_var = it->value; break; }
    const fs::path program = loa::runtime::exec::resolve_in(fs::path(req->command), path_var);
    const std::vector<std::string> args = req->args.value_or(std::vector<std::string>{});
    auto start_failed = [&](const loa::Error& e) {
        return fail(INTERNAL, std::format("can't run `{}`: {}", req->command, e.message()));
    };
    std::optional<std::size_t> limit;
    if (req->output_byte_limit)
        limit = *req->output_byte_limit > std::numeric_limits<std::size_t>::max() ? std::numeric_limits<std::size_t>::max()
                                                                                    : static_cast<std::size_t>(*req->output_byte_limit);
    auto terminal = std::make_shared<Terminal>(limit);

#if defined(_WIN32)
    // A pseudo console in a job object, so a kill also ends what the command spawned.
    HANDLE in_read = nullptr, in_write = nullptr, out_read = nullptr, out_write = nullptr;
    if (!CreatePipe(&in_read, &in_write, nullptr, 0) || !CreatePipe(&out_read, &out_write, nullptr, 0))
        return std::unexpected(start_failed(loa::Error(ErrorCode::io, std::format("os error {}", GetLastError()))));
    HPCON console = nullptr;
    if (FAILED(CreatePseudoConsole(COORD{200, 50}, in_read, out_write, 0, &console)))
        return std::unexpected(start_failed(loa::Error(ErrorCode::io, "CreatePseudoConsole failed")));
    CloseHandle(in_read);
    CloseHandle(out_write);
    CloseHandle(in_write);
    STARTUPINFOEXW info{};
    info.StartupInfo.cb = sizeof(info);
    SIZE_T size = 0;
    InitializeProcThreadAttributeList(nullptr, 1, 0, &size);
    std::vector<char> attrs(size);
    info.lpAttributeList = reinterpret_cast<LPPROC_THREAD_ATTRIBUTE_LIST>(attrs.data());
    InitializeProcThreadAttributeList(info.lpAttributeList, 1, 0, &size);
    UpdateProcThreadAttribute(info.lpAttributeList, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, console, sizeof(console), nullptr, nullptr);
    auto quote = [](const std::wstring& arg) {
        if (!arg.empty() && arg.find_first_of(L" \t\"") == std::wstring::npos) return arg;
        std::wstring out = L"\"";
        for (std::size_t i = 0; i < arg.size(); ++i) {
            std::size_t slashes = 0;
            while (i < arg.size() && arg[i] == L'\\') { ++slashes; ++i; }
            if (i == arg.size()) { out.append(slashes * 2, L'\\'); break; }
            if (arg[i] == L'"') { out.append(slashes * 2 + 1, L'\\'); out.push_back(L'"'); }
            else { out.append(slashes, L'\\'); out.push_back(arg[i]); }
        }
        return out + L"\"";
    };
    std::wstring line = quote(program.wstring());
    for (const auto& arg : args) line += L" " + quote(fs::path(arg).wstring());
    std::wstring block;
    {
        std::map<std::wstring, std::wstring> merged;
        for (LPWCH all = GetEnvironmentStringsW(), at = all; at && *at; at += wcslen(at) + 1) {
            std::wstring entry(at);
            auto eq = entry.find(L'=', 1);
            if (eq != std::wstring::npos) merged[entry.substr(0, eq)] = entry.substr(eq + 1);
        }
        for (const auto& var : env) merged[fs::path(var.name).wstring()] = fs::path(var.value).wstring();
        for (const auto& [k, v] : merged) block += k + L"=" + v + L'\0';
        block.push_back(L'\0');
    }
    PROCESS_INFORMATION process{};
    const BOOL started = CreateProcessW(nullptr, line.data(), nullptr, nullptr, FALSE,
                                        EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
                                        block.data(), cwd.wstring().c_str(), &info.StartupInfo, &process);
    DeleteProcThreadAttributeList(info.lpAttributeList);
    if (!started) {
        ClosePseudoConsole(console);
        CloseHandle(out_read);
        return std::unexpected(start_failed(loa::Error(ErrorCode::io, std::format("os error {}", GetLastError()))));
    }
    terminal->job = CreateJobObjectW(nullptr, nullptr);
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    SetInformationJobObject(terminal->job, JobObjectExtendedLimitInformation, &limits, sizeof(limits));
    AssignProcessToJobObject(terminal->job, process.hProcess);
    ResumeThread(process.hThread);
    CloseHandle(process.hThread);
    std::thread([terminal, process = process.hProcess, console, out_read] {
        std::thread reader([terminal, out_read] {
            char buf[8192];
            DWORD n = 0;
            while (ReadFile(out_read, buf, sizeof(buf), &n, nullptr) && n > 0) {
                std::scoped_lock lock(terminal->mutex);
                terminal->output.push(std::string_view(buf, n));
            }
        });
        // Wait for the command itself, not the job: background helpers would hold it open.
        WaitForSingleObject(process, INFINITE);
        DWORD code = 0;
        GetExitCodeProcess(process, &code);
        CloseHandle(process);
        ClosePseudoConsole(console);
        reader.join();
        CloseHandle(out_read);
        {
            std::scoped_lock lock(terminal->mutex);
            CloseHandle(terminal->job);
            terminal->job = nullptr;
        }
        terminal->finish(exit_json(code));
    }).detach();
#else
    // The exec failure travels back through a close-on-exec pipe.
    int report[2];
    if (::pipe2(report, O_CLOEXEC) != 0) return std::unexpected(start_failed(os_error(errno)));
    std::vector<std::string> argv_store{program.string()};
    argv_store.insert(argv_store.end(), args.begin(), args.end());
    std::vector<char*> argv;
    for (auto& a : argv_store) argv.push_back(a.data());
    argv.push_back(nullptr);
    const std::string dir = cwd.string();
    // `env` on top of the app's environment, built before the fork (the child only execs).
    std::vector<std::string> env_store;
    for (char** at = environ; at && *at; ++at) {
        std::string_view entry(*at);
        const auto name = entry.substr(0, entry.find('='));
        if (std::ranges::none_of(env, [&](const auto& var) { return var.name == name; })) env_store.emplace_back(entry);
    }
    for (const auto& var : env) {
        std::erase_if(env_store, [&](const std::string& e) { return e.starts_with(var.name + "="); });
        env_store.push_back(var.name + "=" + var.value);
    }
    std::vector<char*> envp;
    for (auto& e : env_store) envp.push_back(e.data());
    envp.push_back(nullptr);
    // Raw output: the bytes reach the agent as the command wrote them.
    termios tio{};
    ::cfmakeraw(&tio);
    tio.c_cflag |= CREAD | CS8;
    int master = -1;
    const pid_t pid = ::forkpty(&master, nullptr, &tio, nullptr);
    if (pid < 0) {
        const int err = errno;
        ::close(report[0]);
        ::close(report[1]);
        return std::unexpected(start_failed(os_error(err)));
    }
    if (pid == 0) {
        ::close(report[0]);
        ::signal(SIGPIPE, SIG_DFL);
        int null = ::open("/dev/null", O_RDONLY);
        if (null >= 0) { ::dup2(null, STDIN_FILENO); ::close(null); }
        int err = 0;
        if (::chdir(dir.c_str()) != 0) err = errno;
        else {
            ::execve(argv[0], argv.data(), envp.data());
            err = errno;
        }
        (void)!::write(report[1], &err, sizeof(err));
        ::_exit(127);
    }
    ::close(report[1]);
    int child_err = 0;
    ssize_t got;
    do got = ::read(report[0], &child_err, sizeof(child_err));
    while (got < 0 && errno == EINTR);
    ::close(report[0]);
    if (got == static_cast<ssize_t>(sizeof(child_err))) {
        ::close(master);
        int status = 0;
        while (::waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
        return std::unexpected(start_failed(os_error(child_err)));
    }
    terminal->pid = pid;
    std::thread([terminal, master, pid] {
        char buf[8192];
        for (;;) {
            const ssize_t n = ::read(master, buf, sizeof(buf));
            if (n > 0) {
                std::scoped_lock lock(terminal->mutex);
                terminal->output.push(std::string_view(buf, static_cast<std::size_t>(n)));
            } else if (n < 0 && errno == EINTR) {
                continue;
            } else {
                break;
            }
        }
        ::close(master);
        int status = 0;
        pid_t done;
        do done = ::waitpid(pid, &status, 0);
        while (done < 0 && errno == EINTR);
        {
            std::scoped_lock lock(terminal->mutex);
            terminal->reaped = true;
        }
        // Publish the exit only once all output is in.
        terminal->finish(done == pid ? exit_json(status) : Json(Object{{"exitCode", Json()}, {"signal", Json()}}));
    }).detach();
#endif

    std::string id = std::format("term-{}", next_.fetch_add(1));
    {
        std::scoped_lock lock(mutex_);
        terminals_.insert_or_assign(id, std::move(terminal));
    }
    return Json(Object{{"terminalId", std::move(id)}});
}

} // namespace loa::harness::acp
