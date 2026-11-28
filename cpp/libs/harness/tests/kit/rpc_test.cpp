#include <doctest/doctest.h>

#include <atomic>
#include <future>

#include "loa/harness/kit/jsonrpc.hpp"
#include "loa/runtime/tempfile.hpp"
#include "support.hpp"

using namespace loa::harness::kit;
namespace json = loa::runtime::json;
namespace exec = loa::runtime::exec;
using loa::harness::test::block_on;
using loa::harness::test::read_file;

namespace {

// JSON-RPC 2.0 plus `{"cancel": id}`, which withdraws an inbound request.
class CancelFraming final : public Framing {
  public:
    Json request(const Json& id, std::string_view method, Json params) const override { return inner_.request(id, method, std::move(params)); }
    Json notification(std::string_view method, Json params) const override { return inner_.notification(method, std::move(params)); }
    Json response(Json id, std::string_view method, RpcResult<Json> result) const override {
        return inner_.response(std::move(id), method, std::move(result));
    }
    std::optional<Incoming> classify(Json msg) const override {
        if (const Json* id = msg.find("cancel")) return incoming::Cancel{*id};
        return inner_.classify(std::move(msg));
    }

  private:
    JsonRpc2 inner_;
};

struct Recorder final : Inbound {
    mutable std::mutex mutex;
    mutable std::vector<std::string> events;
    mutable std::atomic<bool> slow_cancelled{false};
    mutable std::promise<void> closed_promise;
    bool defer = false;

    void push(std::string s) const {
        std::scoped_lock lock(mutex);
        events.push_back(std::move(s));
    }
    std::vector<std::string> snapshot() const {
        std::scoped_lock lock(mutex);
        return events;
    }

    Awaitable<RpcResult<Json>> on_request(std::string method, Json params) const override {
        push("request:" + method);
        if (method == "slow") {
            asio::steady_timer timer(co_await asio::this_coro::executor, std::chrono::seconds(30));
            try {
                co_await timer.async_wait(asio::use_awaitable);
            } catch (const std::exception&) {
                slow_cancelled = true;
                throw;
            }
        }
        co_return json::Value(json::Object{{"ok", true}});
    }
    Awaitable<void> on_notification(std::string method, Json params) const override {
        push("note:" + method + ":" + json::dump(params));
        co_return;
    }
    Awaitable<void> on_closed(std::optional<ExitStatus> status, std::string stderr) const override {
        push("closed:" + (status ? status->to_string() : std::string("?")) + ":" + stderr);
        closed_promise.set_value();
        co_return;
    }
    bool defers_requests() const override { return defer; }
    Awaitable<void> on_deferred(Json id, std::string method, Json) const override {
        push("deferred:" + id_key(id) + ":" + method);
        co_return;
    }
    Awaitable<void> on_response(std::string id, const RpcResult<Json>& result) const override {
        push("response:" + id + (result ? "" : ":error"));
        co_return;
    }
};

exec::Command bash(const std::string& script, const std::filesystem::path& dir) {
    return exec::Command{"/bin/bash", {"-c", script}, {{"OUT", dir.string()}}, dir};
}

} // namespace

TEST_CASE("kit: StdioRpc matches responses by id and maps error responses to Command") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto inbound = std::make_shared<Recorder>();
    const std::string script = R"(read -r a; read -r b
printf '%s\n%s\n' "$a" "$b" > "$OUT/in"
printf '%s\n' '{"jsonrpc":"2.0","method":"note","params":{"n":1}}' '{"id":2,"result":{"x":2}}' '{"id":1,"error":{"code":-32000,"message":"nope"}}' '{"id":99,"result":null}' '' 'not json'
read -r c; printf '%s\n' "$c" >> "$OUT/in"
read -r d
exit 0)";
    auto rpc = StdioRpc<JsonRpc2>::spawn(exec.context().get_executor(), bash(script, dir.path()), JsonRpc2(true), inbound);
    REQUIRE(rpc);
    CHECK(rpc->pid() > 0);
    CHECK(rpc->next_id() == 1);
    CHECK(rpc->next_id() == 2);

    // Both requests are in flight before either response arrives.
    auto first_f = asio::co_spawn(exec.context(), rpc->request_as(Json(1), "first", Json()), asio::use_future);
    auto second_f = asio::co_spawn(exec.context(), rpc->request_as(Json(2), "second", json::Object{{"p", 2}}), asio::use_future);
    std::pair<RpcResult<Json>, RpcResult<Json>> both{first_f.get(), second_f.get()};
    REQUIRE_FALSE(both.first);
    REQUIRE(both.first.error().get<RpcError::Command>());
    CHECK(both.first.error().code() == "-32000");
    CHECK(both.first.error().message() == "the harness rejected `request`: nope");
    REQUIRE(both.second);
    CHECK(json::dump(*both.second) == R"({"x":2})");

    REQUIRE(rpc->notify("bye", Json()));
    auto status = block_on(exec, rpc->shutdown(std::chrono::seconds(10)));
    REQUIRE(status);
    CHECK(status->code() == 0);
    CHECK(rpc->has_exited());

    std::string in = read_file(dir.path() / "in");
    CHECK((in == "{\"id\":1,\"method\":\"first\",\"params\":null,\"jsonrpc\":\"2.0\"}\n"
                 "{\"id\":2,\"method\":\"second\",\"params\":{\"p\":2},\"jsonrpc\":\"2.0\"}\n"
                 "{\"method\":\"bye\",\"jsonrpc\":\"2.0\"}\n" ||
           in == "{\"id\":2,\"method\":\"second\",\"params\":{\"p\":2},\"jsonrpc\":\"2.0\"}\n"
                 "{\"id\":1,\"method\":\"first\",\"params\":null,\"jsonrpc\":\"2.0\"}\n"
                 "{\"method\":\"bye\",\"jsonrpc\":\"2.0\"}\n"));
    inbound->closed_promise.get_future().wait();
    auto events = inbound->snapshot();
    CHECK(events == std::vector<std::string>{"note:note:{\"n\":1}", "response:2", "response:1:error", "response:99",
                                             "closed:exit status: 0:"});

    auto late = block_on(exec, rpc->request("late", Json()));
    REQUIRE_FALSE(late);
    CHECK(late.error().get<RpcError::Exited>());
    CHECK_FALSE(rpc->notify("late", Json()));
}

TEST_CASE("kit: StdioRpc serves inbound requests on cancellable tasks and reports the exit") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto inbound = std::make_shared<Recorder>();
    const std::string script = R"(printf '%s\n' '{"jsonrpc":"2.0","id":"r1","method":"slow"}' '{"jsonrpc":"2.0","id":"r2","method":"fast"}'
read -r x; printf '%s\n' "$x" > "$OUT/answers"
printf '%s\n' '{"cancel":"r1"}'
if read -r -t 1 y; then printf '%s\n' "$y" >> "$OUT/answers"; fi
echo bye >&2
exit 3)";
    auto rpc = StdioRpc<CancelFraming>::spawn(exec.context().get_executor(), bash(script, dir.path()), CancelFraming(), inbound);
    REQUIRE(rpc);
    RpcError closed = block_on(exec, rpc->closed());
    REQUIRE(closed.get<RpcError::Exited>());
    CHECK(closed.get<RpcError::Exited>()->status->code() == 3);
    CHECK(closed.message() == "the harness exited (exit status: 3): bye");
    inbound->closed_promise.get_future().wait();

    CHECK(read_file(dir.path() / "answers") == "{\"id\":\"r2\",\"result\":{\"ok\":true},\"jsonrpc\":\"2.0\"}\n");
    CHECK(inbound->slow_cancelled.load());
    auto events = inbound->snapshot();
    CHECK(events.back() == "closed:exit status: 3:bye");

    auto late = block_on(exec, rpc->request_as(Json("x"), "m", Json()));
    REQUIRE_FALSE(late);
    CHECK(late.error().message() == "the harness exited (exit status: 3): bye");
}

TEST_CASE("kit: StdioRpc hands deferred requests over in order and answers them with respond") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto inbound = std::make_shared<Recorder>();
    inbound->defer = true;
    const std::string script = R"(printf '%s\n' '{"id":5,"method":"ask"}'
read -r x; printf '%s\n' "$x" > "$OUT/answers")";
    auto rpc = StdioRpc<JsonRpc2>::spawn(exec.context().get_executor(), bash(script, dir.path()), JsonRpc2(false), inbound);
    REQUIRE(rpc);
    for (int i = 0; i < 500 && inbound->snapshot().empty(); ++i) std::this_thread::sleep_for(std::chrono::milliseconds(10));
    REQUIRE(inbound->snapshot() == std::vector<std::string>{"deferred:5:ask"});
    REQUIRE(rpc->respond(Json(5), "ask", std::unexpected(RpcError(RpcError::Command{"ask", "denied", "-32001"}))));
    (void)block_on(exec, rpc->closed());
    CHECK(read_file(dir.path() / "answers") == "{\"id\":5,\"error\":{\"code\":-32001,\"message\":\"denied\"}}\n");
}

TEST_CASE("kit: StdioRpc kill and shutdown stop a harness that ignores stdin") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto inbound = std::make_shared<Recorder>();
    auto rpc = StdioRpc<JsonRpc2>::spawn(exec.context().get_executor(), bash("exec sleep 30", dir.path()), JsonRpc2(), inbound);
    REQUIRE(rpc);
    const auto start = std::chrono::steady_clock::now();
    auto status = block_on(exec, rpc->shutdown(std::chrono::milliseconds(100)));
    CHECK(std::chrono::steady_clock::now() - start < std::chrono::seconds(10));
    REQUIRE(status);
    CHECK_FALSE(status->code());

    auto other = StdioRpc<JsonRpc2>::spawn(exec.context().get_executor(), bash("exec sleep 30", dir.path()), JsonRpc2(),
                                           std::make_shared<Recorder>());
    REQUIRE(other);
    auto writer = other->writer();
    REQUIRE(writer.send(json::Object{{"raw", 1}}));
    other->kill();
    auto err = block_on(exec, other->closed());
    REQUIRE(err.get<RpcError::Exited>());
    CHECK(err.message() == "the harness exited (signal: 9 (SIGKILL))");
}

TEST_CASE("kit: StdioRpc::spawn reports a missing executable as NotFound") {
    loa::runtime::Executor exec(1);
    auto dir = loa::runtime::TempDir::create().value();
    exec::Command cmd{dir.path() / "missing", {}, {}, {}};
    auto rpc = StdioRpc<JsonRpc2>::spawn(exec.context().get_executor(), cmd, JsonRpc2(), std::make_shared<Recorder>());
    REQUIRE_FALSE(rpc);
    CHECK(rpc.error().get<RpcError::NotFound>());
}
