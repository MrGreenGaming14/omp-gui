// The tools area (port of `loa-app/src/areas/tools/tests.rs` and the unit tests of
// `areas/tools/remote.rs`), plus the formats it keeps on disk: the remote audit log's SQLite
// schema and the known-hosts file the remote service writes.

#include <algorithm>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

#include <doctest/doctest.h>
#include <sqlite3.h>

#include "areas/tools/listing.hpp"
#include "areas/tools/remote.hpp"
#include "areas/tools/tools.hpp"
#include "loa/app/app.hpp"
#include "loa/app/routes.hpp"
#include "loa/core/schema.hpp"
#include "loa/projects/tools.hpp"
#include "loa/remote/audit.hpp"
#include "loa/remote/known_hosts.hpp"
#include "loa/remote/tools.hpp"
#include "loa/runtime/tempfile.hpp"
#include "support/test_app.hpp"

using namespace loa::app;
using loa::app::testing::TestApp;
namespace tools = loa::app::areas::tools;
namespace remote = loa::remote;
namespace core = loa::core;
namespace vp = loa::view::panel;

namespace {

std::string read_file(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::stringstream out;
    out << in.rdbuf();
    return out.str();
}

} // namespace

TEST_CASE("the tools panel opens from the palette and sessions get the app tools") {
    TestApp test;
    test.palette("MCP servers");
    const auto& tabs = test.app->ctx.shell.tabs();
    CHECK(std::ranges::any_of(tabs, [](const auto& tab) { return tab.id == tools::TAB; }));
    test.command("sidebar.tools");
    CHECK(std::ranges::any_of(test.app->ctx.app_tools, [](const auto& tool) { return tool.name == loa::projects::PROJECT_SEARCH; }));
    test.session("hello");
    auto configs = test.harness.seen().configs;
    REQUIRE_FALSE(configs.empty());
    CHECK_MESSAGE(std::ranges::any_of(configs.front().host_tools,
                                      [](const auto& tool) { return tool.name == loa::projects::PROJECT_SEARCH; }),
                  "app tools reach sessions as host tools");
    test.prompt("tool " + std::string(loa::projects::PROJECT_SEARCH) + " {}");
    test.until("the host tool answered", [](const App& app) {
        const auto* session = app.ctx.runtime.sessions().active();
        return session && !session->is_busy();
    });
    CHECK_FALSE(test.harness.seen().prompts.empty());
    test.command("tools.shell.nowhere");
    CHECK_FALSE(test.notice().starts_with("Unknown command"));
}

TEST_CASE("harnesses with MCP reach the remote server over loopback HTTP, not as host tools") {
    TestApp test;
    // The startup tasks are dropped in tests: start the loopback server as `State::make` does.
    test.run(Task::job([set = *test.area<tools::State>().remote().tools](Task::Emit send, loa::runtime::Executor& executor) mutable {
        auto started = remote::RemoteHttp::start(std::move(set), executor);
        std::expected<std::shared_ptr<remote::RemoteHttp>, std::string> result = std::unexpected(std::string("not started"));
        if (started) result = std::make_shared<remote::RemoteHttp>(std::move(*started));
        else result = std::unexpected(started.error());
        send(tools::State::message(tools::msg::RemoteServing{std::move(result)}));
    }));
    REQUIRE_MESSAGE(test.area<tools::State>().remote().http != nullptr, test.notice());
    test.session("hello");
    auto configs = test.harness.seen().configs;
    REQUIRE_FALSE(configs.empty());
    const auto& config = configs.front();
    CHECK(std::ranges::any_of(config.mcp_servers, [](const auto& spec) { return spec.name == remote::SERVER_NAME; }));
    CHECK_FALSE(std::ranges::any_of(config.host_tools, [](const auto& tool) { return tool.name == remote::REMOTE_BASH; }));
}

TEST_CASE("the tools area lists the app hosted servers and the hosts it reads") {
    TestApp test;
    test.command("tools.hosts");
    const auto& area = test.area<tools::State>();
    CHECK(area.panel().section() == loa::ui::panel::tools::Section::Hosts);
    const auto& servers = area.panel().servers();
    REQUIRE(servers.size() >= 2);
    CHECK(servers[0].name == remote::SERVER_NAME);
    CHECK(servers[0].kind == vp::ServerKind::AppHosted);
    CHECK(servers[1].name == "omp");
    // A host in the central config is listed once the hosts reload, and offered a shell.
    auto config = test.app->ctx.services.storage.config.edit([](loa::settings::app::Config& c) {
        loa::settings::app::RemoteHost entry;
        entry.host = "box.example";
        c.remote_hosts.insert_or_assign("box", entry);
    });
    REQUIRE(config);
    test.area<tools::State>().reload_hosts(test.app->ctx);
    const auto& hosts = test.area<tools::State>().panel().hosts();
    REQUIRE(hosts.size() == 1);
    CHECK(hosts.front().alias == "box");
    const auto titles = test.palette_titles();
    CHECK(std::ranges::find(titles, "Open a shell on box") != titles.end());
}

TEST_CASE("audit times read as UTC dates") {
    using namespace std::chrono;
    CHECK(tools::utc(system_clock::time_point{}) == "1970-01-01 00:00:00 UTC");
    CHECK(tools::utc(system_clock::time_point{seconds{1'763'548'245}}) == "2025-11-19 10:30:45 UTC");
}

TEST_CASE("remote approval advertises the canonical session option") {
    remote::Prompt prompt = remote::ApprovalRequest{"s1", "box", remote::Action::command, "uptime", ""};
    auto [kind, options] = tools::decision_options(prompt);
    CHECK(kind == core::DecisionKind::Approval);
    const auto items = options.str_items();
    CHECK((items == std::vector<std::string_view>{core::answer::ALLOW_ONCE, core::answer::ALLOW_SESSION, core::answer::DENY}));
    CHECK(tools::title(prompt) == "Run a command on box?");
    CHECK(tools::body(prompt) == "uptime");
}

TEST_CASE("host key prompts read as questions with their fingerprint") {
    remote::HostKeyPrompt key{"box", "box.example", 2222, "ssh-ed25519", "SHA256:abc", {}};
    remote::Prompt prompt = key;
    auto [kind, options] = tools::decision_options(prompt);
    CHECK(kind == core::DecisionKind::Question);
    CHECK((options.str_items() == std::vector<std::string_view>{tools::TRUST, tools::REJECT}));
    CHECK(tools::title(prompt) == "Trust the host key of box?");
    CHECK(tools::body(prompt) == "[box.example]:2222 presented ssh-ed25519 key\nSHA256:abc\nNo key is on file for it yet.");
}

TEST_CASE("tool descriptions shorten to their first line") {
    CHECK(tools::listing::short_description("\n  first line  \nsecond") == "first line");
    const std::string long_line(200, 'a');
    CHECK(tools::listing::short_description(long_line) == std::string(160, 'a') + "\u2026");
    CHECK(tools::listing::short_description("") == "");
}

TEST_CASE("peers may use the tools their live grants allow") {
    auto scope = [](loa::trust::Resource resource, loa::trust::Action action) {
        auto made = loa::trust::Scope::make(std::move(resource), {action});
        REQUIRE(made);
        return *made;
    };
    loa::trust::IssuedRecord record;
    auto key = loa::trust::NodeIdentity::generate();
    REQUIRE(key);
    record.holder = key->public_key();
    record.scopes = std::vector{scope(loa::trust::resource::Session{"s"}, loa::trust::Action::drive),
                                scope(loa::trust::resource::Ssh{"box"}, loa::trust::Action::read)};
    std::vector<vp::McpServer> servers{
        tools::listing::app_server(remote::SERVER_NAME, loa::contracts::McpHttp{}, vp::server_status::Running{},
                                   {{std::string(remote::REMOTE_BASH), "", {}}, {std::string(remote::REMOTE_FILE), "", {}}}),
        tools::listing::app_server("other", loa::contracts::McpHttp{}, vp::server_status::Running{}, {{"t", "", {}}})};
    const std::vector<std::string> hosts{"box"};
    auto peers = tools::listing::peer_tools(std::vector{record}, servers, hosts, 0);
    REQUIRE(peers.size() == 1);
    CHECK(peers.front().name.size() == 8);
    CHECK(peers.front().allows(vp::ToolId::builtin("bash")));
    CHECK(peers.front().allows(vp::ToolId::mcp(std::string(remote::SERVER_NAME), std::string(remote::REMOTE_FILE))));
    CHECK_FALSE(peers.front().allows(vp::ToolId::mcp(std::string(remote::SERVER_NAME), std::string(remote::REMOTE_BASH))));
    CHECK(peers.front().allows(vp::ToolId::mcp("other", "t")));
    record.revoked_ms = 1;
    CHECK(tools::listing::peer_tools(std::vector{record}, servers, hosts, 0).empty());
}

TEST_CASE("the remote audit log keeps the Rust schema byte for byte") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto [made, warning] = tools::Remote::make(dir->path(), remote::RemoteServiceConfig::from_home(dir->path()));
    CHECK_FALSE(warning);
    const auto golden = read_file(std::filesystem::path(LOA_GOLDEN_DIR) / "remote" / "remote_audit.sql");
    REQUIRE_FALSE(golden.empty());
    CHECK(std::string(remote::AUDIT_SCHEMA) == golden);
    REQUIRE(made.service->audit()->record({std::chrono::system_clock::time_point{std::chrono::seconds{1'763'548'245}}, "box", "s1",
                                           remote::Action::command, "uptime", remote::Outcome::ok, ""}));
    sqlite3* db = nullptr;
    REQUIRE(sqlite3_open_v2((dir->path() / tools::AUDIT_FILE).c_str(), &db, SQLITE_OPEN_READONLY, nullptr) == SQLITE_OK);
    sqlite3_stmt* stmt = nullptr;
    REQUIRE(sqlite3_prepare_v2(db, "SELECT name FROM sqlite_master WHERE type IN ('table','index') AND name LIKE 'remote_audit%' ORDER BY name",
                               -1, &stmt, nullptr) == SQLITE_OK);
    std::vector<std::string> names;
    while (sqlite3_step(stmt) == SQLITE_ROW) names.emplace_back(reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0)));
    sqlite3_finalize(stmt);
    sqlite3_close(db);
    CHECK(names == std::vector<std::string>{"remote_audit", "remote_audit_host", "remote_audit_session"});
    auto rows = tools::Remote::audit(made.service->audit(), "box");
    REQUIRE(rows);
    REQUIRE(rows->size() == 1);
    CHECK(rows->front().when == "2025-11-19 10:30:45 UTC");
    CHECK(rows->front().action == vp::AuditAction::Command);
    CHECK(rows->front().outcome == "ok");
}

TEST_CASE("the known hosts file round-trips byte for byte") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto file = dir->path() / "known_hosts";
    const std::string existing =
        "# comment kept\n"
        "box.example ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl\n";
    {
        std::ofstream out(file, std::ios::binary);
        out << existing;
    }
    remote::KnownHosts known({file}, file);
    auto key = remote::PublicKey::from_openssh("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl");
    REQUIRE(key);
    auto status = known.check("box.example", 22, *key);
    REQUIRE(status);
    CHECK(status->kind == remote::HostKeyStatus::Kind::trusted);
    REQUIRE(known.add("other.example", 2222, *key));
    const auto written = read_file(file);
    CHECK(written ==
          existing + "[other.example]:2222 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl\n");
}

TEST_CASE("a remote card answered after its question went away is cancelled") {
    TestApp test;
    test.session("hello");
    const auto id = test.active();
    REQUIRE(id);
    auto& area = test.area<tools::State>();
    auto result = area.remote().answer_card(loa::bus::SessionId(*id), core::NodeId{999}, loa::contracts::Answer{{"Trust"}, std::nullopt, "me"}, test.app->ctx);
    REQUIRE_FALSE(result);
    CHECK(result.error() == "This remote host question is no longer waiting");
}
