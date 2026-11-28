// The tools' schemas and argument errors, and (port of remote.rs) tool calls served as host
// tools and over loopback streamable HTTP, and the prompt queue in front of a real server.

#include <doctest/doctest.h>

#include <asio.hpp>
#include <httplib.h>

#include <chrono>
#include <fstream>
#include <future>

#include "loa/remote/http.hpp"
#include "loa/remote/tools.hpp"
#include "loa/runtime/executor.hpp"
#include "sshd.hpp"

using namespace loa::remote;
using namespace loa::remote::test;
namespace json = loa::runtime::json;

namespace {

std::expected<std::string, std::string> call(loa::runtime::Executor& exec, const loa::contracts::ToolSet& tools, std::string session,
                                             std::string tool, std::string args) {
    return asio::co_spawn(exec.context(), tools.call(std::move(session), std::move(tool), std::move(args)), asio::use_future).get();
}

std::expected<FileRequest, std::string> request(const char* args) {
    auto parsed = json::parse(args);
    REQUIRE(parsed);
    auto decoded = FileArgs::from_json(*parsed);
    if (!decoded) return std::unexpected(decoded.error());
    return std::move(*decoded).into_request();
}

} // namespace

TEST_CASE("tool schemas match schemars") {
    CHECK(BashArgs::schema() == slurp(std::filesystem::path(LOA_GOLDEN_DIR) / "remote/remote_bash.schema.json"));
    CHECK(FileArgs::schema() == slurp(std::filesystem::path(LOA_GOLDEN_DIR) / "remote/remote_file.schema.json"));
    const auto tools = tool_set(nullptr);
    std::vector<std::string> names;
    for (const auto& spec : tools.specs()) names.push_back(spec.name);
    CHECK(names == std::vector<std::string>{"remote_bash", "remote_file"});
    CHECK(tools.get("remote_bash")->spec().description ==
          "Run a shell command on a configured SSH host. Returns stdout, stderr and the exit status. Subject to the host's limits and the "
          "user's approval.");
    CHECK(tools.get("remote_file")->spec().description ==
          "Read, list, write or edit a file on a configured SSH host. `edit` replaces one exact, unique occurrence of `old` with `new`. "
          "Writes are atomic where the server allows.");
    CHECK(tools.get("remote_file")->annotations() == loa::contracts::ToolAnnotations{std::nullopt, false, true});
}

TEST_CASE("file arguments become requests with the Rust errors") {
    CHECK(request(R"({"host":"b","op":"write","path":"/x"})").error() == "`content` is required for op `write`");
    CHECK(request(R"({"host":"b","op":"edit","path":"/x","new":"n"})").error() == "`old` is required for op `edit`");
    CHECK(request(R"({"host":"b","op":"edit","path":"/x","old":"o"})").error() == "`new` is required for op `edit`");
    CHECK(request(R"({"host":"b","op":"write","path":"/x","content":"a*b=","encoding":"base64"})").error() ==
          "`content` is not valid base64: Invalid symbol 42, offset 1.");
    CHECK(request(R"({"host":"b","op":"write","path":"/x","content":"aGk","encoding":"base64"})").error() ==
          "`content` is not valid base64: Invalid padding");
    CHECK(request(R"({"host":"b","op":"write","path":"/x","content":"aGl=","encoding":"base64"})").error() ==
          "`content` is not valid base64: Invalid last symbol 108, offset 2.");
    CHECK(request(R"({"host":"b","op":"write","path":"/x","content":"aGlhZ","encoding":"base64"})").error() ==
          "`content` is not valid base64: Invalid input length: 5");
    CHECK(request(R"({"host":"b","op":"move","path":"/x"})").error() == "unknown variant `move`, expected one of `read`, `write`, `list`, `edit`");
    CHECK(request(R"({"host":"b","op":"write","path":"/x","content":"aGk=","encoding":"base64"})")->op == FileOp{WriteOp{"hi"}});
    CHECK(request(R"({"host":"b","op":"write","path":"/x","content":"hi","encoding":null})")->op == FileOp{WriteOp{"hi"}});
    CHECK(request(R"({"host":"b","op":"edit","path":"/x","old":"o","new":"n"})")->op == FileOp{EditOp{"o", "n"}});
    CHECK(request(R"({"host":"b","op":"list","path":"/x"})")->op == FileOp{ListOp{}});
}

TEST_CASE("tools serve host tool calls and http sessions with the same audit") {
    if (!have_sshd()) {
        MESSAGE("no sshd found (set LOA_SSHD); skipped");
        return;
    }
    loa::runtime::Executor exec(2);
    {
        auto limits = open_limits();
        limits.commands = false;
        const auto rig = setup(limits).build();
        const auto tools = tool_set(rig->service);
        const auto a_txt = (std::filesystem::canonical(rig->dir.path()) / "a.txt").string();
        const std::vector<std::tuple<std::string, std::string, bool, std::string>> calls{
            {"remote_file", R"({"host":"box","op":"write","path":")" + a_txt + R"(","content":"aGk=","encoding":"base64"})", true, "\"op\": \"write\""},
            {"remote_file", R"({"host":"box","op":"read","path":")" + a_txt + R"("})", true, "\"content\": \"hi\""},
            {"remote_file", R"({"host":"box","op":"edit","path":")" + a_txt + R"(","old":"h"})", false, "`new` is required for op `edit`"},
            {"remote_bash", R"({"host":"box","command":"uptime"})", false, "commands are not allowed"},
            {"remote_bash", "[1]", false, "invalid arguments"},
        };
        for (const auto& [tool, args, ok, want] : calls) {
            CAPTURE(args);
            const auto got = call(exec, tools, "omp-session", tool, args);
            CHECK(got.has_value() == ok);
            CHECK((got ? *got : got.error()).find(want) != std::string::npos);
        }
        AuditQuery q;
        q.session_id = "omp-session";
        const auto rows = rig->audit->query(q);
        REQUIRE(rows);
        std::vector<std::pair<Action, Outcome>> kinds;
        for (const auto& r : *rows) kinds.emplace_back(r.action, r.outcome);
        CHECK(kinds == std::vector<std::pair<Action, Outcome>>{
                           {Action::command, Outcome::refused}, {Action::read, Outcome::ok}, {Action::write, Outcome::ok}});
    }

    const auto rig = setup(open_limits()).build();
    auto http = RemoteHttp::start(tool_set(rig->service), exec);
    REQUIRE(http);
    const auto spec = http->session_spec("claude-session");
    CHECK(spec.name == "remote");
    const auto& transport = std::get<loa::contracts::McpHttp>(spec.transport);
    CHECK(transport.url == http->url());
    const auto port = std::stoi(transport.url.substr(transport.url.rfind(':') + 1));
    httplib::Client client("127.0.0.1", port);
    client.set_read_timeout(std::chrono::seconds(20));
    httplib::Headers auth{{"Authorization", transport.headers.at("Authorization")}, {"Accept", "application/json, text/event-stream"}};
    const auto init = R"({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}})";
    auto opened = client.Post("/mcp", auth, init, "application/json");
    REQUIRE(opened);
    CHECK(opened->status == 200);
    CHECK(opened->body.find("Work on SSH hosts that do not run the app.") != std::string::npos);
    const auto bash_call = R"({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"remote_bash","arguments":{"host":"box","command":"echo over http"}}})";
    auto called = client.Post("/mcp", auth, bash_call, "application/json");
    REQUIRE(called);
    CHECK(called->status == 200);
    CHECK(called->body.find("over http") != std::string::npos);
    CHECK(called->body.find("\"isError\":false") != std::string::npos);
    CHECK_MESSAGE(rig->last().session_id == "claude-session", "the token names the session");
    http->forget("claude-session");
    auto forgotten = client.Post("/mcp", auth, init, "application/json");
    REQUIRE(forgotten);
    CHECK(forgotten->status == 401);
    httplib::Headers stranger{{"Authorization", "Bearer nope"}};
    auto refused = client.Post("/mcp", stranger, init, "application/json");
    REQUIRE(refused);
    CHECK(refused->status == 401);
    http->shutdown();
}

TEST_CASE("the prompt queue asks for the approval, then the new host key") {
    if (!have_sshd()) {
        MESSAGE("no sshd found (set LOA_SSHD); skipped");
        return;
    }
    loa::runtime::Executor exec(2);
    HostLimits limits;
    limits.allowed_paths = {"/"};
    auto s = setup(limits);
    s.known = "";
    const auto rig = s.build();
    auto prompts = std::make_shared<PromptQueue>();
    RemoteServiceConfig config{{rig->known_hosts}, rig->known_hosts, ConnectorConfig::with_agent(rig->agent->socket)};
    auto service = std::make_shared<RemoteService>(rig->hosts, rig->audit, prompts, prompts, std::move(config));
    const auto tools = tool_set(service);
    auto pending_call = std::async(std::launch::async, [&] { return call(exec, tools, "s1", "remote_bash", R"({"host":"box","command":"echo approved"})"); });
    const auto next = [&] {
        auto seen = prompts->generation();
        for (int i = 0; i < 100; ++i) {
            auto pending = prompts->pending();
            if (!pending.empty()) return pending.front();
            seen = prompts->wait_change(seen, std::chrono::milliseconds(100));
        }
        FAIL("no question arrived");
        return PromptView{};
    };
    const auto approval = next();
    const auto* request = std::get_if<ApprovalRequest>(&approval.prompt);
    REQUIRE(request);
    CHECK(request->target == "echo approved");
    CHECK(request->session_id == "s1");
    CHECK_FALSE(prompts->answer(approval.id, true));
    CHECK(prompts->answer(approval.id, Approval::allow_once()));
    const auto key = next();
    const auto* host_key = std::get_if<HostKeyPrompt>(&key.prompt);
    REQUIRE(host_key);
    CHECK(host_key->port == rig->server->port);
    CHECK(prompts->answer(key.id, true));
    const auto result = pending_call.get();
    REQUIRE(result);
    CHECK(result->find("approved") != std::string::npos);
    CHECK(slurp(rig->known_hosts).find("[127.0.0.1]:" + std::to_string(rig->server->port)) != std::string::npos);
    CHECK(prompts->pending().empty());
}
