// Port of `crates/loa-ui-panel/src/tools/tests.rs` (state half).

#include <doctest/doctest.h>

#include "loa/ui/panel/tools.hpp"

using namespace loa::ui::panel::tools;
namespace ti = loa::view::panel::tools_intent;
using loa::contracts::McpHttp;
using loa::contracts::McpStdio;

namespace {

ToolInfo tool(std::string name) { return {name, name + " tool", {}}; }
ProjectRef project(std::string id) { return {id, "Project " + id}; }
RemoteHost host(std::string alias) {
    HostLimits l;
    l.allowed_paths = {"/srv"};
    l.read_only = true;
    return {alias, alias + ".lan", std::nullopt, std::nullopt, std::nullopt, l};
}
template <typename F> F filled(F form, std::initializer_list<std::pair<std::string_view, std::string_view>> values) {
    for (auto [k, v] : values) REQUIRE(form.set(k, v));
    return form;
}
std::vector<Event> send(State& s, std::vector<Message> messages) {
    std::vector<Event> out;
    for (auto& m : messages)
        if (auto e = s.update(std::move(m)).second) out.push_back(std::move(*e));
    return out;
}
Errors errors(std::initializer_list<std::pair<FieldId, std::string_view>> list) {
    Errors out;
    for (auto [k, e] : list) out.emplace(k, std::string(e));
    return out;
}
McpServer server(std::string name, ServerKind kind, ServerStatus status, std::initializer_list<const char*> tools) {
    McpServer s{name, McpStdio{name, {}, {}}, std::move(status), kind, {}};
    for (const char* t : tools) s.tools.push_back(tool(t));
    return s;
}
State panel() {
    State s;
    s.set_builtin({"omp", {tool("bash"), tool("read")}, {tool("project_search"), tool("sandbox_task")}, std::nullopt});
    s.set_servers({server("remote", ServerKind::AppHosted, server_status::Running{}, {"remote_bash", "remote_file"}),
                   server("browser", ServerKind::Surface, server_status::Stopped{}, {"surface_click", "read"}),
                   server("git", ServerKind::External, server_status::Error{"exited"}, {"git_log"})});
    s.set_projects({project("a"), project("b")});
    s.set_hosts({host("build"), host("db")});
    return s;
}
msg::FormEdit edit(FormId f, std::string k, std::string v) { return {f, std::move(k), std::move(v)}; }

} // namespace

TEST_CASE("forms_validate_per_field") {
    const std::vector<std::string> git{"git"};
    auto srv = [](std::string_view name, TransportKind kind, std::string_view target) {
        AddServerForm f;
        f.kind = kind;
        return filled(f, {{"name", name}, {"command", target}, {"url", target}});
    };
    using enum TransportKind;
    const std::vector<std::pair<AddServerForm, Errors>> bad{
        {srv("fs", Stdio, "   "), errors({{"command", EMPTY}})},
        {srv("fs", Stdio, "npx 'oops"), errors({{"command", UNBALANCED}})},
        {srv("fs", Stdio, "A=1 B=2"), errors({{"command", NO_COMMAND}})},
        {srv("my server", Stdio, "x"), errors({{"name", BAD_NAME}})},
        {srv("", Stdio, "'x"), errors({{"name", EMPTY}, {"command", UNBALANCED}})},
        {srv("git", Http, "localhost:3000"), errors({{"name", DUPLICATE_SERVER}, {"url", BAD_SCHEME}})},
        {srv("w", Http, "https:///mcp"), errors({{"url", NO_HOST}})},
        {srv("w", Http, "https://host:99999/"), errors({{"url", BAD_PORT}})},
        {srv("w", Http, "https://ho st/"), errors({{"url", WHITESPACE}})},
    };
    for (const auto& [f, want] : bad) {
        auto r = f.validate(git);
        REQUIRE_FALSE(r);
        CHECK(r.error() == want);
    }
    for (auto url : {" https://mcp.example.com:8443/sse ", "HTTP://[::1]:3000/mcp", "http://user@localhost/mcp"}) {
        INFO(url);
        CHECK(srv("w", Http, url).validate({}));
    }
    auto ok = srv("fs", Stdio, R"(API_KEY=a=b npx -y "@scope/server fs" a\ b)").validate({});
    REQUIRE(ok);
    const auto& stdio = std::get<McpStdio>(ok->transport);
    CHECK(stdio.command == "npx");
    CHECK(stdio.args == std::vector<std::string>{"-y", "@scope/server fs", "a b"});
    CHECK(stdio.env.at("API_KEY") == "a=b");

    auto host_form = [](std::string_view a, std::string_view h, std::string_view u, std::string_view p) {
        return filled(AddHostForm{}, {{"alias", a}, {"hostname", h}, {"user", u}, {"port", p}});
    };
    const std::vector<std::string> build{"build"};
    auto good = host_form("gpu", "gpu.lan", "ci", "2222").validate(build);
    REQUIRE(good);
    CHECK(good->port == 2222);
    CHECK(good->user == "ci");
    CHECK(good->limits.read_only);
    CHECK_FALSE(good->limits.commands_allowed);
    auto badh = host_form("build", "", "a b", "0").validate(build);
    REQUIRE_FALSE(badh);
    CHECK(badh.error() == errors({{"alias", DUPLICATE_HOST}, {"hostname", EMPTY}, {"user", WHITESPACE}, {"port", BAD_PORT}}));

    auto draft = LimitsDraft::of(host("h"));
    for (auto [path, error] : {std::pair<const char*, std::optional<std::string_view>>{"var/log", RELATIVE_PATH},
                               {"/srv", DUPLICATE_PATH}, {" ~/work ", std::nullopt}}) {
        draft.new_path = path;
        draft.add_path();
        CHECK(draft.path_error == error);
    }
    CHECK(draft.limits.allowed_paths == std::vector<std::string>{"/srv", "~/work"});
}

TEST_CASE("messages_emit_events_and_scope_toggles") {
    auto s = panel();
    const auto bash = ToolId::builtin("bash");
    const std::vector remote{ToolId::mcp("remote", "remote_bash"), ToolId::mcp("remote", "remote_file")};
    HostLimits saved;
    saved.allowed_paths = {"/var"};
    saved.read_only = true;
    saved.commands_allowed = true;
    const std::vector<std::pair<std::vector<Message>, std::vector<Event>>> cases{
        {{msg::ToolToggled{bash, false}}, {}},
        {{msg::ProjectSelected{project("a")}, msg::ToolToggled{bash, false}},
         {ti::ProjectSelected{"a"}, ti::SetToolsEnabled{"a", {bash}, false}}},
        {{msg::ServerToggled{"remote"}}, {ti::SetToolsEnabled{"a", remote, false}}},
        {{msg::Server{"remote", ServerAction::Remove}, msg::Server{"git", ServerAction::Remove}}, {ti::Server{"git", ServerAction::Remove}}},
        {{msg::FormOpen{FormId::Server}, edit(FormId::Server, "name", "git"), msg::TransportPicked{TransportKind::Http},
          edit(FormId::Server, "url", "x"), msg::FormSubmit{FormId::Server}},
         {}},
        {{edit(FormId::Server, "name", "docs"), edit(FormId::Server, "url", "http://h/mcp"), msg::FormSubmit{FormId::Server}},
         {ti::AddServer{{"docs", McpHttp{"http://h/mcp", {}}}}}},
        {{msg::FormOpen{FormId::Host}, edit(FormId::Host, "alias", "build"), edit(FormId::Host, "hostname", "h"), msg::FormSubmit{FormId::Host},
          msg::FormCancel{FormId::Host}},
         {}},
        {{msg::EditLimits{"build"}, msg::LimitSet{Limit::Commands, true}, edit(FormId::Limits, "path", "/var"), msg::LimitsAddPath{},
          msg::LimitsRemovePath{0}, msg::FormSubmit{FormId::Limits}},
         {ti::SaveHostLimits{"build", saved}}},
        {{msg::ImportScan{}, msg::ImportSubmit{}, msg::OpenTerminal{"db"}, msg::ShowAudit{"db"}, msg::RemoveHost{"db"}},
         {ti::ScanImports{}, ti::OpenTerminal{"db"}, ti::LoadAudit{"db"}, ti::RemoveHost{"db"}}},
    };
    for (const auto& [messages, want] : cases) CHECK(send(s, messages) == want);
    CHECK_FALSE(s.add_host());
    CHECK_FALSE(s.add_server());
    send(s, {msg::ProjectSelected{project("b")}});
    CHECK(s.disabled_tools() == nullptr); // project b keeps its own toggles
    auto candidate = [](ImportSource source, std::string name) {
        return ImportCandidate{source, "mcp.json", {name, McpStdio{name, {}, {}}}};
    };
    s.set_import_candidates({candidate(ImportSource::Claude, "git"), candidate(ImportSource::Cursor, "fetch")});
    CHECK(send(s, {msg::ImportSubmit{}}) == std::vector<Event>{ti::ImportServers{{candidate(ImportSource::Cursor, "fetch")}}});
    s.set_import_outcome(ImportSource::Cursor, "fetch", import_outcome::Imported{});
    CHECK(send(s, {msg::ImportSubmit{}}).empty()); // imported entries aren't imported twice
}

TEST_CASE("catalog_filters_and_peer_matrix_follow_grants") {
    auto s = panel();
    const std::vector<std::tuple<const char*, Groups, Groups>> cases{
        {"PROJECT_", {{"project_search"}}, {}},
        {"brow", {}, {{"surface_click", "read"}}},
        {"remote_f", {}, {{"remote_file"}}},
        {"read", {{"read"}}, {{"read"}}},
    };
    for (const auto& [f, b, v] : cases) {
        INFO(f);
        send(s, {msg::FilterChanged{f}});
        CHECK(names(s.catalog()) == std::pair{b, v});
    }
    s.select_project("a");
    s.set_disabled_tools("a", {ToolId::mcp("browser", "read")});
    const auto c = s.catalog();
    CHECK(c.builtin[0].tools[0].enabled); // MCP toggles leave built in tools alone
    CHECK(c.servers[0].toggle == Toggle::Mixed);
    CHECK_FALSE(c.servers[0].tools[0].enabled);
    auto peer = [](std::string k, std::vector<ToolRule> rules) { return PeerTools{k, k, std::move(rules)}; };
    s.set_peers({peer("alice", {{tool_rule::All{}}}),
                 peer("bob", {{tool_rule::Tool{ToolId::builtin("read")}}, {tool_rule::Server{"browser"}}}),
                 peer("carol", {{tool_rule::AllBuiltin{}}})});
    send(s, {msg::FilterChanged{""}});
    const auto m = s.peer_matrix();
    auto row = [&](const ToolId& id) {
        for (const auto& r : m.rows)
            if (r.tool == id) return r.allowed;
        FAIL("no row");
        return std::vector<bool>{};
    };
    CHECK(row(ToolId::builtin("bash")) == std::vector{true, false, true});
    CHECK(row(ToolId::builtin("read")) == std::vector{true, true, true});
    CHECK(row(ToolId::mcp("browser", "read")) == std::vector{true, true, false});
    CHECK(row(ToolId::mcp("git", "git_log")) == std::vector{true, false, false});
    send(s, {msg::PeerFilter{PeerChoice::peer("bob", "bob")}});
    std::vector<std::string> tools;
    for (const auto& r : s.peer_matrix().rows) tools.push_back(r.tool.name());
    CHECK(tools == std::vector<std::string>{"read", "surface_click", "read"});
    s.set_peers({peer("carol", {})});
    CHECK(s.peer_matrix().peers[0]->name == "carol"); // a vanished peer resets the filter
}
