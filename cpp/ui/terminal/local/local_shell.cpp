// Local programs on a PTY as a `PtyChannel`. Unix: `forkpty`, the master read and written through Asio
// descriptors, the child reaped after output ends. Windows: ConPTY, blocking pipe I/O on a private thread.

#include "loa/ui/terminal/local.hpp"

#include <chrono>
#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

#include <asio.hpp>

#if defined(_WIN32)
#include <windows.h>
#else
#include <cerrno>
#include <csignal>
#include <fcntl.h>
#include <pty.h>
#include <pwd.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <thread>
#include <unistd.h>
extern char** environ;
#endif

namespace loa::ui::terminal {

namespace port = loa::view::terminal::port;
using asio::awaitable;
using asio::use_awaitable;

namespace {

constexpr std::size_t READ_CHUNK = 64 * 1024;

std::uint16_t pixels(std::uint16_t cell, std::uint16_t count) {
    const unsigned v = static_cast<unsigned>(cell) * count;
    return static_cast<std::uint16_t>(v > 0xFFFF ? 0xFFFF : v);
}

// The app's environment with `TERM`, `COLORTERM` and then `extra` set over it.
std::vector<std::string> environment(const std::vector<std::pair<std::string, std::string>>& extra) {
    std::map<std::string, std::string> overrides{{"TERM", "xterm-256color"}, {"COLORTERM", "truecolor"}};
    for (const auto& [k, v] : extra) overrides.insert_or_assign(k, v);
    std::vector<std::string> out;
#if defined(_WIN32)
    if (wchar_t* block = GetEnvironmentStringsW()) {
        for (const wchar_t* p = block; *p; p += wcslen(p) + 1) {
            const int n = WideCharToMultiByte(CP_UTF8, 0, p, -1, nullptr, 0, nullptr, nullptr);
            std::string entry(static_cast<std::size_t>(n > 0 ? n - 1 : 0), '\0');
            if (n > 1) WideCharToMultiByte(CP_UTF8, 0, p, -1, entry.data(), n, nullptr, nullptr);
            const auto eq = entry.find('=', 1);
            if (eq != std::string::npos && !overrides.contains(entry.substr(0, eq))) out.push_back(std::move(entry));
        }
        FreeEnvironmentStringsW(block);
    }
#else
    for (char** e = environ; e && *e; ++e) {
        const std::string_view entry(*e);
        const auto eq = entry.find('=');
        if (eq != std::string_view::npos && !overrides.contains(std::string(entry.substr(0, eq)))) out.emplace_back(entry);
    }
#endif
    for (const auto& [k, v] : overrides) out.push_back(k + "=" + v);
    return out;
}

#if !defined(_WIN32)

// The child process; reaped once, hung up (SIGHUP, like closing a terminal window) until then.
struct Child {
    pid_t pid;
    std::mutex mutex;
    bool reaped = false;
    std::optional<std::uint32_t> exit_code;

    explicit Child(pid_t p) : pid(p) {}
    ~Child() {
        if (reaped) return;
        ::kill(pid, SIGHUP);
        std::thread([pid = pid] {
            int status = 0;
            while (::waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
        }).detach();
    }
    // True once the child exited; its status is in `exit_code` (unset when a signal ended it).
    bool try_reap() {
        std::lock_guard lock(mutex);
        if (reaped) return true;
        int status = 0;
        const pid_t r = ::waitpid(pid, &status, WNOHANG);
        if (r == 0 || (r < 0 && errno == EINTR)) return false;
        reaped = true;
        if (r == pid && WIFEXITED(status)) exit_code = static_cast<std::uint32_t>(WEXITSTATUS(status));
        return true;
    }
    void hangup() {
        std::lock_guard lock(mutex);
        if (!reaped) ::kill(pid, SIGHUP);
    }
};

class LocalReader final : public port::ChannelReader {
  public:
    LocalReader(const asio::any_io_executor& ex, int fd, std::shared_ptr<Child> child)
        : fd_(ex, fd), child_(std::move(child)), buffer_(READ_CHUNK) {}

    awaitable<port::Result<port::ChannelRead>> read() override {
        for (;;) {
            auto [ec, n] = co_await fd_.async_read_some(asio::buffer(buffer_), asio::as_tuple(use_awaitable));
            if (!ec && n > 0) {
                const auto first = buffer_.begin();
                co_return port::channel_read::Data{std::vector<std::uint8_t>(first, first + static_cast<std::ptrdiff_t>(n))};
            }
            if (ec == asio::error::interrupted) continue;
            // A PTY master's reads fail with EIO once every slave descriptor is closed.
            if (!ec || ec == asio::error::eof || ec.value() == EIO) break;
            co_return std::unexpected(port::Error::io(ec.message()));
        }
        asio::steady_timer wait(fd_.get_executor());
        while (!child_->try_reap()) {
            wait.expires_after(std::chrono::milliseconds(10));
            co_await wait.async_wait(use_awaitable);
        }
        co_return port::channel_read::Closed{child_->exit_code};
    }

  private:
    asio::posix::stream_descriptor fd_;
    std::shared_ptr<Child> child_;
    std::vector<std::uint8_t> buffer_;
};

// A running child's PTY; closing or destroying it hangs up.
class LocalWriter final : public port::ChannelWriter {
  public:
    LocalWriter(const asio::any_io_executor& ex, int fd, std::shared_ptr<Child> child) : fd_(ex, fd), child_(std::move(child)) {}
    ~LocalWriter() override { child_->hangup(); }

    awaitable<port::Result<>> write(std::vector<std::uint8_t> bytes) override {
        auto [ec, n] = co_await asio::async_write(fd_, asio::buffer(bytes), asio::as_tuple(use_awaitable));
        if (ec) co_return std::unexpected(port::Error::io(ec.message()));
        co_return port::Result<>{};
    }
    awaitable<port::Result<>> resize(port::GridSize s) override {
        const winsize ws{s.rows, s.cols, pixels(s.cell_width, s.cols), pixels(s.cell_height, s.rows)};
        if (::ioctl(fd_.native_handle(), TIOCSWINSZ, &ws) < 0) co_return std::unexpected(port::Error::io(std::strerror(errno)));
        co_return port::Result<>{};
    }
    awaitable<port::Result<>> close() override {
        child_->hangup();
        co_return port::Result<>{};
    }

  private:
    asio::posix::stream_descriptor fd_;
    std::shared_ptr<Child> child_;
};

#else // _WIN32

std::wstring wide(std::string_view s) {
    if (s.empty()) return {};
    const int n = MultiByteToWideChar(CP_UTF8, 0, s.data(), static_cast<int>(s.size()), nullptr, 0);
    std::wstring out(static_cast<std::size_t>(n), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.data(), static_cast<int>(s.size()), out.data(), n);
    return out;
}

std::string last_error() {
    return std::system_category().message(static_cast<int>(GetLastError()));
}

// `arg` quoted for `CommandLineToArgvW`.
std::wstring quote(const std::wstring& arg) {
    if (!arg.empty() && arg.find_first_of(L" \t\"") == std::wstring::npos) return arg;
    std::wstring out = L"\"";
    std::size_t slashes = 0;
    for (const wchar_t c : arg) {
        if (c == L'\\') {
            ++slashes;
            continue;
        }
        out.append(c == L'"' ? slashes * 2 + 1 : slashes, L'\\');
        slashes = 0;
        out.push_back(c);
    }
    out.append(slashes * 2, L'\\');
    return out + L"\"";
}

// The pseudoconsole, its pipes and the process. ConPTY keeps its output pipe open after the shell exits;
// closing the pseudoconsole flushes it and lets the reader reach EOF.
struct Console {
    HPCON console = nullptr;
    HANDLE input = INVALID_HANDLE_VALUE, output = INVALID_HANDLE_VALUE, process = nullptr;
    std::mutex mutex;
    std::optional<std::uint32_t> exit_code;
    // Blocking pipe I/O and the process wait run here, off the executor.
    asio::thread_pool io{2};

    void close_console() {
        std::lock_guard lock(mutex);
        if (console) ClosePseudoConsole(std::exchange(console, nullptr));
    }
    void kill() {
        std::lock_guard lock(mutex);
        if (process && WaitForSingleObject(process, 0) == WAIT_TIMEOUT) TerminateProcess(process, 1);
    }
    ~Console() {
        kill();
        close_console();
        io.join();
        if (input != INVALID_HANDLE_VALUE) CloseHandle(input);
        if (output != INVALID_HANDLE_VALUE) CloseHandle(output);
        if (process) CloseHandle(process);
    }
};

class ConsoleReader final : public port::ChannelReader {
  public:
    explicit ConsoleReader(std::shared_ptr<Console> c) : console_(std::move(c)), buffer_(READ_CHUNK) {}
    awaitable<port::Result<port::ChannelRead>> read() override {
        auto c = console_;
        auto* buffer = &buffer_;
        co_return co_await asio::co_spawn(
            c->io,
            [c, buffer]() -> awaitable<port::Result<port::ChannelRead>> {
                DWORD n = 0;
                if (ReadFile(c->output, buffer->data(), static_cast<DWORD>(buffer->size()), &n, nullptr) && n > 0)
                    co_return port::channel_read::Data{std::vector<std::uint8_t>(buffer->begin(), buffer->begin() + n)};
                const DWORD error = GetLastError();
                if (n == 0 || error == ERROR_BROKEN_PIPE) {
                    WaitForSingleObject(c->process, INFINITE);
                    std::lock_guard lock(c->mutex);
                    co_return port::channel_read::Closed{c->exit_code};
                }
                co_return std::unexpected(port::Error::io(last_error()));
            },
            use_awaitable);
    }

  private:
    std::shared_ptr<Console> console_;
    std::vector<std::uint8_t> buffer_;
};

class ConsoleWriter final : public port::ChannelWriter {
  public:
    explicit ConsoleWriter(std::shared_ptr<Console> c) : console_(std::move(c)) {}
    // ClosePseudoConsole can wait for its output to drain, which needs the reader this caller may be
    // responsible for polling, so the pseudoconsole closes on the I/O thread.
    ~ConsoleWriter() override {
        console_->kill();
        asio::post(console_->io, [c = console_] { c->close_console(); });
    }
    awaitable<port::Result<>> write(std::vector<std::uint8_t> bytes) override {
        auto c = console_;
        co_return co_await asio::co_spawn(
            c->io,
            [c, bytes = std::move(bytes)]() -> awaitable<port::Result<>> {
                DWORD written = 0;
                for (std::size_t at = 0; at < bytes.size(); at += written)
                    if (!WriteFile(c->input, bytes.data() + at, static_cast<DWORD>(bytes.size() - at), &written, nullptr))
                        co_return std::unexpected(port::Error::io(last_error()));
                co_return port::Result<>{};
            },
            use_awaitable);
    }
    awaitable<port::Result<>> resize(port::GridSize s) override {
        std::lock_guard lock(console_->mutex);
        if (!console_->console) co_return std::unexpected(port::Error::closed());
        if (FAILED(ResizePseudoConsole(console_->console, COORD{static_cast<SHORT>(s.cols), static_cast<SHORT>(s.rows)})))
            co_return std::unexpected(port::Error::io(last_error()));
        co_return port::Result<>{};
    }
    awaitable<port::Result<>> close() override {
        console_->kill();
        co_return port::Result<>{};
    }

  private:
    std::shared_ptr<Console> console_;
};

#endif

} // namespace

port::Result<port::PtyChannel> spawn(const LocalShell& shell, port::GridSize size, const asio::any_io_executor& executor) {
    std::error_code fs_error;
    if (!std::filesystem::is_directory(shell.cwd, fs_error)) return std::unexpected(port::Error::not_a_directory(shell.cwd));
    const auto env = environment(shell.env);
#if !defined(_WIN32)
    // Everything the child needs is built before the fork: only async-signal-safe calls follow it.
    std::string program;
    std::vector<std::string> args;
    if (shell.program) {
        program = shell.program->string();
        args.push_back(program);
        args.insert(args.end(), shell.args.begin(), shell.args.end());
    } else {
        const char* login = std::getenv("SHELL");
        if (!login || !*login) {
            const passwd* pw = ::getpwuid(::getuid());
            login = pw && pw->pw_shell && *pw->pw_shell ? pw->pw_shell : "/bin/sh";
        }
        program = login;
        // A leading `-` makes it a login shell.
        args.push_back("-" + std::filesystem::path(program).filename().string());
    }
    std::vector<char*> argv, envp;
    for (auto& a : args) argv.push_back(a.data());
    argv.push_back(nullptr);
    auto env_storage = env;
    for (auto& e : env_storage) envp.push_back(e.data());
    envp.push_back(nullptr);
    const std::string cwd = shell.cwd.string();
    const bool search = shell.program.has_value();

    int report[2];
    if (::pipe2(report, O_CLOEXEC) < 0) return std::unexpected(port::Error::spawn(std::strerror(errno)));
    winsize ws{size.rows, size.cols, pixels(size.cell_width, size.cols), pixels(size.cell_height, size.rows)};
    int master = -1;
    const pid_t pid = ::forkpty(&master, nullptr, nullptr, &ws);
    if (pid < 0) {
        const int e = errno;
        ::close(report[0]);
        ::close(report[1]);
        return std::unexpected(port::Error::spawn(std::strerror(e)));
    }
    if (pid == 0) {
        ::close(report[0]);
        for (const int sig : {SIGPIPE, SIGINT, SIGQUIT, SIGTERM, SIGHUP, SIGCHLD}) ::signal(sig, SIG_DFL);
        sigset_t none;
        sigemptyset(&none);
        ::sigprocmask(SIG_SETMASK, &none, nullptr);
        if (::chdir(cwd.c_str()) == 0) {
            if (search) ::execvpe(program.c_str(), argv.data(), envp.data());
            else ::execve(program.c_str(), argv.data(), envp.data());
        }
        const int e = errno;
        (void)!::write(report[1], &e, sizeof e);
        ::_exit(127);
    }
    ::close(report[1]);
    int exec_error = 0;
    ssize_t n = 0;
    do n = ::read(report[0], &exec_error, sizeof exec_error);
    while (n < 0 && errno == EINTR);
    ::close(report[0]);
    if (n == static_cast<ssize_t>(sizeof exec_error)) {
        int status = 0;
        while (::waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
        ::close(master);
        return std::unexpected(port::Error::spawn(program + ": " + std::strerror(exec_error)));
    }
    ::fcntl(master, F_SETFD, FD_CLOEXEC);
    const int write_fd = ::fcntl(master, F_DUPFD_CLOEXEC, 0);
    auto child = std::make_shared<Child>(pid);
    if (write_fd < 0) {
        ::close(master);
        return std::unexpected(port::Error::spawn(std::strerror(errno)));
    }
    return port::PtyChannel{std::make_unique<LocalReader>(executor, master, child), std::make_unique<LocalWriter>(executor, write_fd, child)};
#else
    (void)executor;
    auto c = std::make_shared<Console>();
    HANDLE in_read = nullptr, out_write = nullptr;
    if (!CreatePipe(&in_read, &c->input, nullptr, 0) || !CreatePipe(&c->output, &out_write, nullptr, 0))
        return std::unexpected(port::Error::spawn(last_error()));
    const HRESULT hr = CreatePseudoConsole(COORD{static_cast<SHORT>(size.cols), static_cast<SHORT>(size.rows)}, in_read, out_write, 0, &c->console);
    CloseHandle(in_read);
    CloseHandle(out_write);
    if (FAILED(hr)) return std::unexpected(port::Error::spawn(std::system_category().message(hr)));

    std::wstring command;
    if (shell.program) {
        command = quote(shell.program->wstring());
        for (const auto& a : shell.args) command += L" " + quote(wide(a));
    } else {
        const char* comspec = std::getenv("COMSPEC");
        command = quote(wide(comspec && *comspec ? comspec : "cmd.exe"));
    }
    std::wstring block;
    for (const auto& e : env) block += wide(e) + L'\0';
    block += L'\0';

    SIZE_T bytes = 0;
    InitializeProcThreadAttributeList(nullptr, 1, 0, &bytes);
    std::vector<std::byte> attributes(bytes);
    STARTUPINFOEXW info{};
    info.StartupInfo.cb = sizeof info;
    info.lpAttributeList = reinterpret_cast<LPPROC_THREAD_ATTRIBUTE_LIST>(attributes.data());
    if (!InitializeProcThreadAttributeList(info.lpAttributeList, 1, 0, &bytes) ||
        !UpdateProcThreadAttribute(info.lpAttributeList, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, c->console, sizeof c->console, nullptr, nullptr))
        return std::unexpected(port::Error::spawn(last_error()));
    PROCESS_INFORMATION process{};
    const std::wstring cwd = shell.cwd.wstring();
    const BOOL ok = CreateProcessW(nullptr, command.data(), nullptr, nullptr, FALSE, EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                                   block.data(), cwd.c_str(), &info.StartupInfo, &process);
    DeleteProcThreadAttributeList(info.lpAttributeList);
    if (!ok) return std::unexpected(port::Error::spawn(last_error()));
    CloseHandle(process.hThread);
    c->process = process.hProcess;
    // The wait: records the exit status, then closes the pseudoconsole so the reader sees EOF.
    asio::post(c->io, [weak = std::weak_ptr<Console>(c)] {
        auto c = weak.lock();
        if (!c) return;
        WaitForSingleObject(c->process, INFINITE);
        DWORD code = 0;
        if (GetExitCodeProcess(c->process, &code)) {
            std::lock_guard lock(c->mutex);
            c->exit_code = code;
        }
        c->close_console();
    });
    return port::PtyChannel{std::make_unique<ConsoleReader>(c), std::make_unique<ConsoleWriter>(c)};
#endif
}

} // namespace loa::ui::terminal
