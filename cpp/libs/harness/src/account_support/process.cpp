#include "loa/harness/account_support/process.hpp"

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>

#include <array>
#include <cerrno>
#include <chrono>
#include <fcntl.h>
#include <format>
#include <map>
#include <spawn.h>
#include <system_error>
#include <unistd.h>
#include <vector>

#include "../kit/utf8.hpp"
#include "loa/harness/kit/proc.hpp"
#include "loa/runtime/exec.hpp"

extern char** environ;

namespace loa::harness::account_support {

namespace exec = loa::runtime::exec;
using kit::ChildProcess;
using kit::Fd;

Awaitable<std::expected<std::string, loa::Error>> ProcessReader::read_to_end() {
    std::string out;
    std::array<char, 4096> buf{};
    for (;;) {
        auto n = co_await read_some(buf);
        if (!n) co_return std::unexpected(n.error());
        if (*n == 0) co_return out;
        out.append(buf.data(), *n);
    }
}

namespace {

// Rust's `io::Error` display for OS errors.
std::string os_error(std::error_code err) { return std::format("{} (os error {})", err.message(), err.value()); }

Error process_error(const std::string& program, std::error_code err) { return Error::process(program, os_error(err)); }

class PipeReader final : public ProcessReader {
  public:
    PipeReader(const asio::any_io_executor& executor, Fd fd) : pipe_(executor, fd.release()) {}

    Awaitable<std::expected<std::size_t, loa::Error>> read_some(std::span<char> buf) override {
        auto [ec, n] = co_await pipe_.async_read_some(asio::buffer(buf.data(), buf.size()), asio::as_tuple(asio::use_awaitable));
        if (ec == asio::error::eof) co_return 0;
        if (ec) co_return std::unexpected(loa::Error(ErrorCode::io, os_error(ec)));
        co_return n;
    }

  private:
    asio::posix::stream_descriptor pipe_;
};

class PipeWriter final : public ProcessWriter {
  public:
    PipeWriter(const asio::any_io_executor& executor, Fd fd) : pipe_(executor, fd.release()) {}

    Awaitable<std::expected<void, loa::Error>> write_all(std::string bytes) override {
        auto [ec, n] = co_await asio::async_write(pipe_, asio::buffer(bytes), asio::as_tuple(asio::use_awaitable));
        if (ec) co_return std::unexpected(loa::Error(ErrorCode::io, os_error(ec)));
        co_return std::expected<void, loa::Error>{};
    }

  private:
    asio::posix::stream_descriptor pipe_;
};

// The app's environment with `overrides` on top.
std::vector<std::string> environment(const std::vector<std::pair<std::string, std::string>>& overrides) {
    std::map<std::string, std::string> vars;
    std::vector<std::string> order;
    for (char** e = environ; e && *e; ++e) {
        std::string_view kv(*e);
        const auto eq = kv.find('=');
        if (eq == std::string_view::npos) continue;
        std::string key(kv.substr(0, eq));
        if (vars.emplace(key, std::string(kv.substr(eq + 1))).second) order.push_back(std::move(key));
    }
    for (const auto& [k, v] : overrides)
        if (vars.insert_or_assign(k, v).second) order.push_back(k);
    std::vector<std::string> out;
    out.reserve(order.size());
    for (const auto& k : order) out.push_back(k + "=" + vars[k]);
    return out;
}

std::expected<std::pair<Fd, Fd>, std::error_code> make_pipe() {
    int fds[2];
    if (::pipe2(fds, O_CLOEXEC) != 0) return std::unexpected(std::error_code(errno, std::system_category()));
    return std::pair{Fd(fds[0]), Fd(fds[1])};
}

struct Spawned {
    ChildProcess child;
    Fd in;
    Fd out;
    Fd err;
};

// Starts `spec` with stdout and stderr piped; stdin is piped too, else `/dev/null`.
std::expected<Spawned, std::error_code> start(const CommandSpec& spec, bool pipe_stdin) {
    const exec::Command cmd = exec::command(spec.program, spec.args, spec.env, spec.current_dir);
    Fd null_in;
    std::pair<Fd, Fd> in;
    if (pipe_stdin) {
        auto p = make_pipe();
        if (!p) return std::unexpected(p.error());
        in = std::move(*p);
    } else {
        null_in = Fd(::open("/dev/null", O_RDONLY | O_CLOEXEC));
        if (null_in.get() < 0) return std::unexpected(std::error_code(errno, std::system_category()));
    }
    auto out = make_pipe();
    if (!out) return std::unexpected(out.error());
    auto err = make_pipe();
    if (!err) return std::unexpected(err.error());

    const std::string program = cmd.program.string();
    std::vector<std::string> args{program};
    args.insert(args.end(), cmd.args.begin(), cmd.args.end());
    std::vector<char*> argv;
    for (auto& a : args) argv.push_back(a.data());
    argv.push_back(nullptr);
    std::vector<std::string> env = environment(cmd.env);
    std::vector<char*> envp;
    for (auto& e : env) envp.push_back(e.data());
    envp.push_back(nullptr);
    const int child_in = pipe_stdin ? in.first.get() : null_in.get();

    auto spawned = exec::retry_busy([&]() -> std::expected<pid_t, std::error_code> {
        posix_spawn_file_actions_t actions;
        posix_spawn_file_actions_init(&actions);
        posix_spawn_file_actions_adddup2(&actions, child_in, 0);
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
    if (!spawned) return std::unexpected(spawned.error());
    return Spawned{ChildProcess(*spawned), std::move(in.second), std::move(out->first), std::move(err->first)};
}

// Polls for the child's exit without blocking the executor.
Awaitable<std::optional<exec::ExitStatus>> reap(ChildProcess& child) {
    asio::steady_timer timer(co_await asio::this_coro::executor);
    auto delay = std::chrono::milliseconds(1);
    for (;;) {
        if (child.pid() <= 0) co_return std::nullopt;
        if (auto status = child.try_wait()) co_return status;
        timer.expires_after(delay);
        co_await timer.async_wait(asio::use_awaitable);
        delay = std::min(delay * 2, std::chrono::milliseconds(20));
    }
}

class PipedProcess final : public RunningProcess {
  public:
    PipedProcess(const asio::any_io_executor& executor, std::string program, Spawned spawned)
        : program_(std::move(program)), child_(std::move(spawned.child)),
          stdin_(std::make_unique<PipeWriter>(executor, std::move(spawned.in))),
          stdout_(std::make_unique<PipeReader>(executor, std::move(spawned.out))),
          stderr_(std::make_unique<PipeReader>(executor, std::move(spawned.err))) {}

    Awaitable<Result<std::optional<int>>> wait() override {
        stdin_.reset();
        if (!status_) status_ = co_await reap(child_);
        if (!status_)
            co_return std::unexpected(process_error(program_, std::make_error_code(std::errc::no_child_process)));
        co_return status_->code();
    }

    Awaitable<Result<>> kill() override {
        if (!status_) {
            child_.kill();
            status_ = co_await reap(child_);
        }
        co_return Result<>{};
    }

    std::unique_ptr<ProcessWriter> take_stdin() override { return std::move(stdin_); }
    std::unique_ptr<ProcessReader> take_stdout() override { return std::move(stdout_); }
    std::unique_ptr<ProcessReader> take_stderr() override { return std::move(stderr_); }

  private:
    std::string program_;
    ChildProcess child_;
    std::optional<exec::ExitStatus> status_;
    std::unique_ptr<ProcessWriter> stdin_;
    std::unique_ptr<ProcessReader> stdout_;
    std::unique_ptr<ProcessReader> stderr_;
};

} // namespace

Awaitable<Result<ProcessOutput>> AsioProcessRunner::output(CommandSpec spec) const {
    using namespace asio::experimental::awaitable_operators;
    auto spawned = start(spec, false);
    if (!spawned) co_return std::unexpected(process_error(spec.program_name(), spawned.error()));
    auto executor = co_await asio::this_coro::executor;
    PipeReader out(executor, std::move(spawned->out));
    PipeReader err(executor, std::move(spawned->err));
    auto [stdout_text, stderr_text] = co_await (out.read_to_end() && err.read_to_end());
    auto status = co_await reap(spawned->child);
    if (!status)
        co_return std::unexpected(process_error(spec.program_name(), std::make_error_code(std::errc::no_child_process)));
    co_return ProcessOutput{status->code(), kit::detail::utf8_lossy(stdout_text.value_or("")),
                            kit::detail::utf8_lossy(stderr_text.value_or(""))};
}

Awaitable<Result<std::unique_ptr<RunningProcess>>> AsioProcessRunner::spawn(CommandSpec spec) const {
    auto spawned = start(spec, true);
    if (!spawned) co_return std::unexpected(process_error(spec.program_name(), spawned.error()));
    auto executor = co_await asio::this_coro::executor;
    co_return std::make_unique<PipedProcess>(executor, spec.program_name(), std::move(*spawned));
}

} // namespace loa::harness::account_support
