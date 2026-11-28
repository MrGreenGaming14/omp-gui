// Thin exec stub that runs inside a sandbox (Rust `loa-sandbox-stub`).
//
// Speaks `loa::sandbox::proto` on stdin and stdout and runs commands in its root directory. It
// holds no secrets and opens no connections; the environment it was started with is all its
// commands see.
//
// Usage: `loa-sandbox-stub [ROOT]`. `ROOT` defaults to the current directory.

#include <signal.h>
#include <unistd.h>

#include <chrono>
#include <condition_variable>
#include <filesystem>
#include <map>
#include <memory>
#include <mutex>
#include <thread>

#include "../src/process.hpp"
#include "loa/sandbox/proto.hpp"

namespace proto = loa::sandbox::proto;
namespace detail = loa::sandbox::detail;

namespace {

// Output chunk size for command stdout and stderr.
constexpr std::size_t CHUNK = 32 * 1024;

// Frames to the host. One lock per frame keeps frames whole.
class Out {
  public:
    void send(const proto::Response& response) {
        const auto frame = proto::frame(response);
        std::scoped_lock lock(mutex_);
        // If the host is gone there is nobody to tell; the read loop sees the closed stdin and
        // ends the stub.
        (void)writer_.write_all(frame);
    }
    std::mutex& mutex() { return mutex_; }
    loa::sandbox::FdWriter& writer() { return writer_; }

  private:
    std::mutex mutex_;
    loa::sandbox::FdWriter writer_{1};
};

// Process groups of running commands, by request id.
struct Running {
    std::mutex mutex;
    std::map<std::uint32_t, int> groups;
};

void kill_group(int group) { ::kill(-group, SIGKILL); }

std::jthread pump(int fd, std::shared_ptr<Out> out, std::uint32_t id, bool stderr) {
    return std::jthread([fd, out = std::move(out), id, stderr] {
        loa::sandbox::FdReader pipe(fd);
        std::string buf(CHUNK, '\0');
        for (;;) {
            auto n = pipe.read(buf);
            if (!n || *n == 0) return;
            if (stderr) out->send(proto::response::Stderr{id, buf.substr(0, *n)});
            else out->send(proto::response::Stdout{id, buf.substr(0, *n)});
        }
    });
}

class Stub {
  public:
    Stub(std::filesystem::path root, std::shared_ptr<Out> out) : root_(std::move(root)), out_(std::move(out)) {}

    // False when the stub should end.
    bool handle(proto::Request request) {
        auto* exec = std::get_if<proto::request::Exec>(&request);
        if (!exec) return false;
        if (auto started = start(exec->id, std::move(exec->exec)); !started) {
            out_->send(proto::response::Error{exec->id, "spawn: " + started.error().message()});
        }
        return true;
    }

    void kill_all() {
        std::scoped_lock lock(running_->mutex);
        for (const auto& [_, group] : running_->groups) kill_group(group);
    }

  private:
    // Starts the command with a pump per output pipe and a thread that waits for it (under a
    // watchdog when it has a timeout), then reports its exit.
    loa::sandbox::Result<> start(std::uint32_t id, proto::ExecRequest exec) {
        if (exec.argv.empty()) return std::unexpected(loa::Error(loa::ErrorCode::invalid_argument, "empty argv"));
        detail::Spawn spec;
        spec.program = exec.argv.front();
        spec.args.assign(exec.argv.begin() + 1, exec.argv.end());
        spec.cwd = exec.cwd ? root_ / *exec.cwd : root_;
        spec.env = std::move(exec.env);
        spec.in = exec.stdin_data ? detail::Stdio::piped : detail::Stdio::null;
        spec.out = detail::Stdio::piped;
        spec.err = detail::Stdio::piped;
        spec.new_group = true;
        spec.kill_on_drop = false;
        auto spawned = detail::Process::spawn(spec);
        if (!spawned) return std::unexpected(spawned.error());
        auto proc = std::make_shared<detail::Process>(std::move(*spawned));
        // The child leads its own group, so its pid is the group id.
        const int group = proc->pid();
        {
            std::scoped_lock lock(running_->mutex);
            running_->groups[id] = group;
        }
        if (exec.stdin_data) {
            // A command may exit without reading its stdin; that is not an error.
            std::thread([fd = proc->take_stdin(), data = std::move(*exec.stdin_data)] {
                loa::sandbox::FdWriter pipe(fd);
                (void)pipe.write_all(data);
            }).detach();
        }
        auto stdout_pump = pump(proc->take_stdout(), out_, id, false);
        auto stderr_pump = pump(proc->take_stderr(), out_, id, true);
        std::thread([proc, group, id, timeout = exec.timeout_ms, out = out_, running = running_,
                     pumps = std::array{std::move(stdout_pump), std::move(stderr_pump)}]() mutable {
            // Waits for the command, killing its tree once the timeout passes.
            std::mutex m;
            std::condition_variable wake;
            bool finished = false, timed_out = false;
            std::jthread timer;
            if (timeout) {
                timer = std::jthread([&, deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(*timeout)] {
                    std::unique_lock lock(m);
                    if (!wake.wait_until(lock, deadline, [&] { return finished; })) {
                        kill_group(group);
                        timed_out = true;
                    }
                });
            }
            auto status = proc->wait();
            {
                std::scoped_lock lock(m);
                finished = true;
            }
            wake.notify_all();
            if (timer.joinable()) timer.join();
            // Output that is still buffered goes out before the exit.
            for (auto& p : pumps) p.join();
            {
                std::scoped_lock lock(running->mutex);
                running->groups.erase(id);
            }
            if (status) {
                out->send(proto::response::Exit{id, proto::ExitStatus{detail::exit_code(*status), detail::exit_signal(*status), timed_out}});
            } else {
                out->send(proto::response::Error{id, "wait: " + status.error().message()});
            }
        }).detach();
        return {};
    }

    std::filesystem::path root_;
    std::shared_ptr<Out> out_;
    std::shared_ptr<Running> running_ = std::make_shared<Running>();
};

} // namespace

int main(int argc, char** argv) {
    std::filesystem::path root;
    if (argc > 1) {
        root = argv[1];
    } else {
        std::error_code ec;
        root = std::filesystem::current_path(ec);
        if (ec) root = "/";
    }
    auto out = std::make_shared<Out>();
    {
        std::scoped_lock lock(out->mutex());
        if (!proto::write_hello(out->writer())) _exit(1);
    }
    Stub stub(root, out);
    loa::sandbox::FdReader in(0);
    std::string buf;
    // A frame we can't read means the stream is out of step; there is no safe way to continue.
    for (;;) {
        auto more = proto::read_frame(in, buf);
        if (!more || !*more) break;
        auto request = proto::decode_request(buf);
        if (!request || !stub.handle(std::move(*request))) break;
    }
    stub.kill_all();
    _exit(0);
}
