#include "loa/harness/kit/rpc.hpp"

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>

#include <atomic>
#include <csignal>
#include <deque>
#include <format>
#include <mutex>
#include <unordered_map>

#include "loa/harness/kit/proc.hpp"
#include "loa/runtime/log.hpp"
#include "sync.hpp"

namespace loa::harness::kit {

namespace json = loa::runtime::json;
namespace log = loa::runtime::log;
using detail::Signal;
using detail::WaitList;

// ---- RpcError ------------------------------------------------------------------------

std::string duration_debug(std::chrono::nanoseconds d) {
    const auto total = static_cast<std::uint64_t>(d.count() < 0 ? 0 : d.count());
    auto decimal = [](std::uint64_t integer, std::uint64_t frac, int width, std::string_view unit) {
        std::string out = std::to_string(integer);
        if (frac > 0) {
            std::string digits = std::to_string(frac);
            digits.insert(0, static_cast<std::size_t>(width) - digits.size(), '0');
            while (digits.ends_with('0')) digits.pop_back();
            out += "." + digits;
        }
        out += unit;
        return out;
    };
    const std::uint64_t secs = total / 1'000'000'000, nanos = total % 1'000'000'000;
    if (secs > 0) return decimal(secs, nanos, 9, "s");
    if (nanos >= 1'000'000) return decimal(nanos / 1'000'000, nanos % 1'000'000, 6, "ms");
    if (nanos >= 1'000) return decimal(nanos / 1'000, nanos % 1'000, 3, "\u00b5s");
    return std::to_string(nanos) + "ns";
}

std::optional<std::string_view> RpcError::code() const noexcept {
    if (const auto* command = get<Command>(); command && command->code) return std::string_view(*command->code);
    return std::nullopt;
}

std::string RpcError::message() const {
    struct Visitor {
        std::string operator()(const NotFound& e) const { return std::format("the executable `{}` wasn't found", e.program); }
        std::string operator()(const Io& e) const { return "harness i/o failed: " + e.error.to_string(); }
        std::string operator()(const Exited& e) const {
            std::string out = "the harness exited (" + (e.status ? e.status->to_string() : std::string("unknown status")) + ")";
            if (!e.stderr.empty()) out += ": " + e.stderr;
            return out;
        }
        std::string operator()(const Version& e) const {
            std::string offered = "[";
            for (std::size_t i = 0; i < e.offered.size(); ++i) offered += (i ? ", " : "") + std::to_string(e.offered[i]);
            offered += "]";
            return std::format("the harness offers RPC protocol versions {}, this client supports {}..={}", offered, e.min, e.max);
        }
        std::string operator()(const Protocol& e) const { return "RPC protocol error: " + e.detail; }
        std::string operator()(const Command& e) const { return std::format("the harness rejected `{}`: {}", e.command, e.message); }
        std::string operator()(const StartTimeout& e) const {
            return "the harness didn't become ready within " + duration_debug(e.after);
        }
        std::string operator()(const Closed&) const { return "the RPC client is closed"; }
        std::string operator()(const Json& e) const { return "malformed JSON: " + e.detail; }
    };
    return std::visit(Visitor{}, kind_);
}

loa::contracts::AdapterError RpcError::to_adapter_error() const {
    using loa::contracts::AdapterError;
    if (const auto* e = get<NotFound>()) return AdapterError::NotInstalled{e->program};
    if (const auto* e = get<Io>()) return AdapterError::Io{e->error};
    if (get<Closed>()) return AdapterError::Closed{};
    return AdapterError::Protocol{message()};
}

std::string id_key(const Json& id) {
    if (const auto* s = id.as_string()) return *s;
    return json::dump(id);
}

Json decode(Json data) {
    if (data.is_null()) return json::Object{};
    return data;
}

Awaitable<void> Inbound::on_deferred(Json, std::string, Json) const { co_return; }
Awaitable<void> Inbound::on_response(std::string, const RpcResult<Json>&) const { co_return; }

// ---- Core ----------------------------------------------------------------------------

namespace detail {

struct Exit {
    std::optional<ExitStatus> status;
    std::string stderr;
    [[nodiscard]] RpcError error() const { return RpcError::Exited{status, stderr}; }
};

struct Pending {
    std::shared_ptr<Signal> signal;
    std::optional<RpcResult<Json>> value;
};

// An inbound request served on its own strand, cancellable through `cancel`.
struct Served {
    explicit Served(const asio::any_io_executor& ex) : strand(asio::make_strand(ex)) {}
    asio::strand<asio::any_io_executor> strand;
    asio::cancellation_signal cancel;

    static void abort(const std::shared_ptr<Served>& task) {
        asio::post(task->strand, [task] { task->cancel.emit(asio::cancellation_type::terminal); });
    }
};

// What the handle, the read loop and inbound tasks share.
struct RpcCore {
    RpcCore(asio::any_io_executor ex, std::shared_ptr<const Framing> framing, int pid)
        : ex(std::move(ex)), framing(std::move(framing)), pid(pid), out_signal(std::make_shared<Signal>(this->ex, 1)),
          kill_signal(std::make_shared<Signal>(this->ex, 1)) {}

    asio::any_io_executor ex;
    std::shared_ptr<const Framing> framing;
    int pid;
    std::atomic<std::uint64_t> next{1};
    std::atomic<bool> kill_sent{false};
    std::shared_ptr<Signal> out_signal;
    std::shared_ptr<Signal> kill_signal;
    WaitList exit_waiters;

    std::mutex mutex;
    bool out_open = true;
    std::deque<std::string> out;
    std::unordered_map<std::string, std::shared_ptr<Pending>> pending;
    std::unordered_map<std::string, std::shared_ptr<Served>> tasks;
    std::optional<Exit> exited;

    // Per-frame traces: `LOA_LOG=loa_harness::frames=trace`.
    static constexpr std::string_view FRAMES = "loa_harness::frames";

    RpcError gone_locked() const { return exited ? exited->error() : RpcError(RpcError::Closed{}); }

    RpcResult<void> send(const Json& frame) {
        std::string line = json::dump(frame);
        if (log::enabled(log::Level::trace, FRAMES)) log::trace(FRAMES, "to harness: " + line);
        line.push_back('\n');
        {
            std::scoped_lock lock(mutex);
            if (!out_open) return std::unexpected(gone_locked());
            out.push_back(std::move(line));
        }
        out_signal->try_send(asio::error_code{});
        return {};
    }

    void close_out() {
        {
            std::scoped_lock lock(mutex);
            out_open = false;
        }
        out_signal->try_send(asio::error_code{});
    }

    void kill() {
        if (!kill_sent.exchange(true)) kill_signal->try_send(asio::error_code{});
    }

    // Records the exit and fails everything still waiting.
    void record_exit(const Exit& exit) {
        std::unordered_map<std::string, std::shared_ptr<Pending>> waiting;
        std::unordered_map<std::string, std::shared_ptr<Served>> running;
        {
            std::scoped_lock lock(mutex);
            exited = exit;
            waiting.swap(pending);
            running.swap(tasks);
            out_open = false;
            for (auto& [_, p] : waiting) p->value = std::unexpected(exit.error());
        }
        for (auto& [_, p] : waiting) p->signal->try_send(asio::error_code{});
        for (auto& [_, task] : running) Served::abort(task);
        out_signal->try_send(asio::error_code{});
        exit_waiters.notify_all();
    }

    // Waits until the exit is recorded, or `deadline` passes (false then).
    asio::awaitable<bool> wait_exit(std::optional<std::chrono::steady_clock::time_point> deadline) {
        using namespace asio::experimental::awaitable_operators;
        auto executor = co_await asio::this_coro::executor;
        asio::steady_timer timer(executor);
        if (deadline) timer.expires_at(*deadline);
        for (;;) {
            std::shared_ptr<Signal> signal;
            {
                std::scoped_lock lock(mutex);
                if (exited) co_return true;
                signal = exit_waiters.prepare(executor);
            }
            if (!deadline) {
                co_await WaitList::wait(signal);
                continue;
            }
            auto woke = co_await (WaitList::wait(signal) || timer.async_wait(asio::as_tuple(asio::use_awaitable)));
            if (woke.index() == 1) {
                std::scoped_lock lock(mutex);
                co_return exited.has_value();
            }
        }
    }
};

} // namespace detail

namespace {

using detail::RpcCore;
using detail::Served;

std::string_view trim_ascii(std::string_view s) {
    constexpr std::string_view ws = " \t\n\r\f\v";
    const auto start = s.find_first_not_of(ws);
    if (start == std::string_view::npos) return {};
    return s.substr(start, s.find_last_not_of(ws) - start + 1);
}

void ignore_sigpipe() {
    static std::once_flag once;
    // Rust ignores SIGPIPE at startup; a harness that exits mid-write must surface as EPIPE.
    std::call_once(once, [] { std::signal(SIGPIPE, SIG_IGN); });
}

asio::awaitable<void> serve_one(std::shared_ptr<RpcCore> core, std::shared_ptr<Served> task, Json id, std::string method,
                                Json params, std::shared_ptr<const Inbound> inbound, std::string key) {
    // Off the reader's turn: the read loop goes on while this request is served.
    co_await asio::post(co_await asio::this_coro::executor, asio::use_awaitable);
    RpcResult<Json> result = std::unexpected(RpcError(RpcError::Closed{}));
    std::optional<std::string> failure;
    try {
        result = co_await inbound->on_request(method, std::move(params));
    } catch (const std::exception& e) {
        failure = e.what();
    }
    auto state = co_await asio::this_coro::cancellation_state;
    if (state.cancelled() != asio::cancellation_type::none) co_return;
    if (failure) result = std::unexpected(RpcError(RpcError::Protocol{*failure}));
    {
        std::scoped_lock lock(core->mutex);
        if (auto it = core->tasks.find(key); it != core->tasks.end() && it->second == task) core->tasks.erase(it);
    }
    if (auto sent = core->send(core->framing->response(std::move(id), method, std::move(result))); !sent)
        log::warn("loa_harness", std::format("couldn't answer the harness ({}): {}", method, sent.error().message()));
}

// Answers one inbound request on its own task, abortable by `incoming::Cancel`.
void serve(const std::shared_ptr<RpcCore>& core, Json id, std::string method, Json params,
           std::shared_ptr<const Inbound> inbound) {
    std::string key = id_key(id);
    auto task = std::make_shared<Served>(core->ex);
    // Inserted before the spawn so the task's own removal can't run first. The lock is
    // released before spawning: a strand dispatch may run the task inline on this thread.
    {
        std::scoped_lock lock(core->mutex);
        core->tasks.insert_or_assign(key, task);
    }
    asio::co_spawn(task->strand,
                   serve_one(core, task, std::move(id), std::move(method), std::move(params), std::move(inbound), key),
                   asio::bind_cancellation_slot(task->cancel.slot(), asio::detached));
}

asio::awaitable<void> dispatch(std::shared_ptr<RpcCore> core, std::string_view raw,
                               const std::shared_ptr<const Inbound>& inbound) {
    const std::string_view line = trim_ascii(raw);
    if (line.empty()) co_return;
    if (log::enabled(log::Level::trace, RpcCore::FRAMES)) log::trace(RpcCore::FRAMES, "from harness: " + std::string(line));
    auto msg = json::parse(line);
    if (!msg) {
        log::warn("loa_harness", "the harness wrote a line that isn't JSON: " + msg.error().to_string());
        co_return;
    }
    auto classified = core->framing->classify(std::move(*msg));
    if (!classified) co_return;
    if (auto* response = std::get_if<incoming::Response>(&*classified)) {
        co_await inbound->on_response(response->id, response->result);
        std::shared_ptr<detail::Pending> pending;
        {
            std::scoped_lock lock(core->mutex);
            if (auto it = core->pending.find(response->id); it != core->pending.end()) {
                pending = std::move(it->second);
                core->pending.erase(it);
                pending->value = std::move(response->result);
            }
        }
        if (pending) pending->signal->try_send(asio::error_code{});
        else log::debug("loa_harness", "a response nobody waits for: " + response->id);
    } else if (auto* note = std::get_if<incoming::Notification>(&*classified)) {
        co_await inbound->on_notification(std::move(note->method), std::move(note->params));
    } else if (auto* request = std::get_if<incoming::Request>(&*classified)) {
        if (inbound->defers_requests())
            co_await inbound->on_deferred(std::move(request->id), std::move(request->method), std::move(request->params));
        else
            serve(core, std::move(request->id), std::move(request->method), std::move(request->params), inbound);
    } else if (auto* cancel = std::get_if<incoming::Cancel>(&*classified)) {
        std::shared_ptr<Served> task;
        {
            std::scoped_lock lock(core->mutex);
            if (auto it = core->tasks.find(id_key(cancel->id)); it != core->tasks.end()) {
                task = std::move(it->second);
                core->tasks.erase(it);
            }
        }
        if (task) Served::abort(task);
    }
}

asio::awaitable<void> write_loop(std::shared_ptr<RpcCore> core, Fd stdin_fd) {
    asio::posix::stream_descriptor pipe(co_await asio::this_coro::executor, stdin_fd.release());
    for (;;) {
        std::optional<std::string> line;
        bool open = false;
        {
            std::scoped_lock lock(core->mutex);
            if (!core->out.empty()) {
                line = std::move(core->out.front());
                core->out.pop_front();
            }
            open = core->out_open;
        }
        if (!line) {
            if (!open) break;
            (void)co_await core->out_signal->async_receive(asio::as_tuple(asio::use_awaitable));
            continue;
        }
        auto [ec, n] = co_await asio::async_write(pipe, asio::buffer(*line), asio::as_tuple(asio::use_awaitable));
        if (ec) {
            std::scoped_lock lock(core->mutex);
            core->out_open = false;
            core->out.clear();
            break;
        }
    }
    // Closing stdin tells the harness no more input comes.
}

asio::awaitable<void> read_loop(std::shared_ptr<RpcCore> core, ChildProcess child, Fd stdout_fd,
                                loa::runtime::exec::StderrTail stderr, std::shared_ptr<const Inbound> inbound) {
    using namespace asio::experimental::awaitable_operators;
    auto executor = co_await asio::this_coro::executor;
    asio::posix::stream_descriptor pipe(executor, stdout_fd.release());
    std::string buffer;
    buffer.reserve(4096);
    std::array<char, 8192> chunk{};
    bool killed = false;
    auto dispatch_line = [&](std::string line) -> asio::awaitable<void> {
        try {
            co_await dispatch(core, line, inbound);
        } catch (const std::exception& e) {
            log::warn("loa_harness", std::string("handling a harness frame failed: ") + e.what());
        }
    };
    for (;;) {
        if (auto nl = buffer.find('\n'); nl != std::string::npos) {
            std::string line = buffer.substr(0, nl + 1);
            buffer.erase(0, nl + 1);
            co_await dispatch_line(std::move(line));
            continue;
        }
        auto read = co_await (pipe.async_read_some(asio::buffer(chunk), asio::as_tuple(asio::use_awaitable)) ||
                              core->kill_signal->async_receive(asio::as_tuple(asio::use_awaitable)));
        if (read.index() == 1) {
            killed = true;
            break;
        }
        auto [ec, n] = std::get<0>(read);
        if (ec || n == 0) {
            // `read_until` hands over a last line without its newline.
            if (!buffer.empty()) co_await dispatch_line(std::exchange(buffer, {}));
            break;
        }
        buffer.append(chunk.data(), n);
    }
    pipe.close();

    std::optional<ExitStatus> status;
    asio::steady_timer timer(executor);
    if (!killed) {
        // Wait for the exit, or for `kill`.
        for (;;) {
            if ((status = child.try_wait())) break;
            if (child.pid() <= 0 || core->kill_sent.load()) break;
            timer.expires_after(std::chrono::milliseconds(10));
            (void)co_await timer.async_wait(asio::as_tuple(asio::use_awaitable));
        }
    }
    if (!status) {
        child.kill();
        status = child.wait();
    }
    // stderr closes with the process; a grandchild holding it open must not keep the exit
    // report back forever.
    for (int i = 0; i < 200 && !stderr.wait_closed(std::chrono::milliseconds(0)); ++i) {
        timer.expires_after(std::chrono::milliseconds(10));
        (void)co_await timer.async_wait(asio::as_tuple(asio::use_awaitable));
    }
    detail::Exit exit{status, std::string(trim_ascii(stderr.joined()))};
    core->record_exit(exit);
    try {
        co_await inbound->on_closed(exit.status, exit.stderr);
    } catch (const std::exception& e) {
        log::warn("loa_harness", std::string("on_closed failed: ") + e.what());
    }
}

} // namespace

// ---- Writer / StdioRpcBase -----------------------------------------------------------

RpcResult<void> Writer::send(const Json& frame) const {
    auto core = core_.lock();
    if (!core) return std::unexpected(RpcError(RpcError::Closed{}));
    return core->send(frame);
}

StdioRpcBase& StdioRpcBase::operator=(StdioRpcBase&& other) noexcept {
    if (this != &other) {
        close_out();
        core_ = std::move(other.core_);
    }
    return *this;
}

StdioRpcBase::~StdioRpcBase() { close_out(); }

void StdioRpcBase::close_out() const noexcept {
    if (core_) core_->close_out();
}

RpcResult<std::shared_ptr<detail::RpcCore>> StdioRpcBase::spawn_core(const asio::any_io_executor& executor,
                                                                     const loa::runtime::exec::Command& cmd,
                                                                     std::shared_ptr<const Framing> framing,
                                                                     std::shared_ptr<const Inbound> inbound) {
    ignore_sigpipe();
    auto piped = spawn_piped(cmd);
    if (!piped) return std::unexpected(std::move(piped).error());
    auto core = std::make_shared<RpcCore>(executor, std::move(framing), piped->child.pid());
    asio::co_spawn(executor, write_loop(core, std::move(piped->stdin_fd)), asio::detached);
    asio::co_spawn(executor,
                   read_loop(core, std::move(piped->child), std::move(piped->stdout_fd), std::move(piped->stderr),
                             std::move(inbound)),
                   asio::detached);
    return core;
}

std::uint64_t StdioRpcBase::next_id() const { return core_->next.fetch_add(1, std::memory_order_relaxed); }

Awaitable<RpcResult<Json>> StdioRpcBase::request_as(Json id, std::string method, Json params) const {
    auto core = core_;
    const std::string key = id_key(id);
    auto pending = std::make_shared<detail::Pending>();
    pending->signal = std::make_shared<Signal>(co_await asio::this_coro::executor, 1);
    std::optional<RpcError> early;
    {
        std::scoped_lock lock(core->mutex);
        if (core->exited) early = core->exited->error();
        else core->pending.insert_or_assign(key, pending);
    }
    if (early) co_return std::unexpected(std::move(*early));
    if (auto sent = core->send(core->framing->request(id, method, std::move(params))); !sent) {
        std::scoped_lock lock(core->mutex);
        if (auto it = core->pending.find(key); it != core->pending.end() && it->second == pending) core->pending.erase(it);
        co_return std::unexpected(std::move(sent).error());
    }
    (void)co_await pending->signal->async_receive(asio::as_tuple(asio::use_awaitable));
    std::optional<RpcResult<Json>> value;
    RpcError gone = RpcError::Closed{};
    {
        std::scoped_lock lock(core->mutex);
        value = std::move(pending->value);
        gone = core->gone_locked();
    }
    if (value) co_return std::move(*value);
    co_return std::unexpected(std::move(gone));
}

Awaitable<RpcResult<Json>> StdioRpcBase::request(std::string method, Json params) const {
    auto result = co_await request_as(Json(next_id()), std::move(method), std::move(params));
    if (!result) co_return result;
    co_return decode(std::move(*result));
}

RpcResult<void> StdioRpcBase::notify(std::string_view method, Json params) const {
    return core_->send(core_->framing->notification(method, std::move(params)));
}

RpcResult<void> StdioRpcBase::respond(Json id, std::string_view method, RpcResult<Json> result) const {
    return core_->send(core_->framing->response(std::move(id), method, std::move(result)));
}

RpcResult<void> StdioRpcBase::send(const Json& frame) const { return core_->send(frame); }

Writer StdioRpcBase::writer() const { return Writer(core_); }

bool StdioRpcBase::has_exited() const {
    std::scoped_lock lock(core_->mutex);
    return core_->exited.has_value();
}

int StdioRpcBase::pid() const { return core_->pid; }

Awaitable<RpcError> StdioRpcBase::closed() const {
    auto core = core_;
    co_await core->wait_exit(std::nullopt);
    std::scoped_lock lock(core->mutex);
    co_return core->gone_locked();
}

void StdioRpcBase::kill() const { core_->kill(); }

Awaitable<std::optional<ExitStatus>> StdioRpcBase::shutdown(std::chrono::milliseconds grace) const {
    auto core = core_;
    core->close_out();
    if (!co_await core->wait_exit(std::chrono::steady_clock::now() + grace)) core->kill();
    co_await core->wait_exit(std::nullopt);
    std::optional<ExitStatus> status;
    {
        std::scoped_lock lock(core->mutex);
        status = core->exited->status;
    }
    co_return status;
}

} // namespace loa::harness::kit
