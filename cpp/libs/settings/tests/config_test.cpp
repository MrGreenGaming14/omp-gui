// Port of crates/loa-settings/tests/config.rs plus golden round trips.

#include <doctest/doctest.h>

#include <filesystem>
#include <fstream>
#include <sstream>
#include <tuple>

#include "loa/runtime/tempfile.hpp"
#include "loa/settings/app/codec.hpp"
#include "loa/settings/app/store.hpp"
#include "loa/settings/projects.hpp"

using namespace loa::settings::app;
namespace fs = std::filesystem;
using Kind = ConfigError::Kind;

namespace {

std::string slurp(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    REQUIRE(in);
    std::ostringstream s;
    s << in.rdbuf();
    return s.str();
}

void spit(const fs::path& path, std::string_view text) {
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    out << text;
}

const fs::path FIXTURES = LOA_SETTINGS_FIXTURES;
const fs::path GOLDEN = fs::path(LOA_GOLDEN_DIR) / "settings";

std::string full() { return slurp(FIXTURES / "full.toml"); }

struct Fixture {
    loa::runtime::TempDir dir;
    ConfigStore store;
};

loa::runtime::TempDir tempdir() {
    auto d = loa::runtime::TempDir::create();
    REQUIRE(d);
    return std::move(*d);
}

Fixture store_with(std::string_view text) {
    auto dir = tempdir();
    ConfigStore store = ConfigStore::in_dir(dir.path() / "nested");
    fs::create_directories(dir.path() / "nested");
    spit(store.path(), text);
    return {std::move(dir), store};
}

ConfigError load_err(std::string_view text) {
    auto f = store_with(text);
    auto r = f.store.load();
    REQUIRE_MESSAGE(!r, "config should be rejected: " << text);
    return r.error();
}

// Reads the file through a new store, so the answer comes from disk and not from the cache.
Config load_from_disk(const ConfigStore& store) {
    auto c = ConfigStore(store.path()).load();
    REQUIRE(c);
    return *c;
}

toml::Table raw(const fs::path& path) {
    auto t = parse(slurp(path));
    REQUIRE(t);
    return *t;
}

const toml::Value& at(const toml::Table& t, std::initializer_list<std::string_view> path) {
    const toml::Table* cur = &t;
    const toml::Value* v = nullptr;
    for (auto key : path) {
        REQUIRE(cur);
        auto it = cur->find(key);
        REQUIRE_MESSAGE(it != cur->end(), "missing " << key);
        v = &it->second;
        cur = v->as_table();
    }
    return *v;
}

} // namespace

TEST_CASE("full config loads into typed settings and round trips") {
    auto [dir, store] = store_with(full());
    auto loaded = store.load();
    REQUIRE(loaded);
    const Config& c = *loaded;
    CHECK(c.appearance.theme == ThemeRef{"dark"});
    CHECK(c.project_tabs);
    CHECK(c.harness.executable("omp") == fs::path("/opt/omp/bin/omp"));
    CHECK(c.network.port == 4242);
    CHECK(c.network.relay == Relay{Relay::Url{"https://relay.example.test"}});
    CHECK(!c.network.discovery.mdns);
    CHECK(c.updater.check_interval() == std::chrono::seconds(3600));
    CHECK(c.updater.pins.at("omp") == "19.0.0");
    CHECK(c.updater.github_token == "ghp_test");
    const DecisionDefault* approval = c.unattended.for_kind(loa::core::DecisionKind::Approval);
    REQUIRE(approval);
    CHECK(approval->timeout_secs == 120);
    CHECK(approval->answer == "deny");
    CHECK(c.unattended.for_kind(loa::core::DecisionKind::Question) == nullptr);
    CHECK(!c.surfaces.video);
    auto* files = std::get_if<loa::contracts::McpStdio>(&c.mcp_servers.at("files").transport);
    REQUIRE(files);
    CHECK(files->args.size() == 2);
    CHECK(files->env.at("LOG") == "debug");
    auto* search = std::get_if<loa::contracts::McpHttp>(&c.mcp_servers.at("search").transport);
    REQUIRE(search);
    CHECK(search->url.ends_with("/sse"));
    CHECK(!c.mcp_servers.at("old").enabled);
    const RemoteHost& host = c.remote_hosts.at("build");
    CHECK(host.port == std::optional<std::uint16_t>(2222));
    CHECK(!host.read_only);
    CHECK(host.commands);
    CHECK(!host.auto_approve);
    ProjectSettings project = c.project("loa");
    CHECK(project.token_budget == 12000);
    CHECK(project.disabled_tools.contains("bash"));
    CHECK(!project.mcp_tool_enabled("files", "delete"));
    CHECK(project.mcp_tool_enabled("files", "read"));
    CHECK(c.unknown.empty());

    REQUIRE(store.save(c));
    CHECK(load_from_disk(store) == c);
    CHECK_MESSAGE(raw(store.path()) == raw(FIXTURES / "full.toml"), "every key survives a round trip unchanged");
}

TEST_CASE("golden: full.toml decodes and re-encodes byte-identically to the Rust app") {
    CHECK(slurp(GOLDEN / "config.toml") == full());
    auto doc = parse(full());
    REQUIRE(doc);
    auto config = decode(*doc);
    REQUIRE(config);
    auto text = encode(*config);
    REQUIRE(text);
    CHECK(*text == slurp(GOLDEN / "config.saved.toml"));

    // The Rust output decodes to the same config and re-encodes to itself.
    auto again = decode(*parse(*text));
    REQUIRE(again);
    CHECK(*again == *config);
    CHECK(*encode(*again) == *text);
}

TEST_CASE("golden: default, theme overrides and unknown keys encode like the Rust app") {
    CHECK(*encode(Config{}) == slurp(GOLDEN / "default.saved.toml"));

    auto theme = decode(*parse("version = 1\n[theme]\nname = \"nord\"\naccent = \"#88c0d0\"\n"
                               "x = { a = [1, 2.5, \"q\\\"\\n\"], b = 1979-05-27T07:32:00.5-07:00 }\n"
                               "[network]\nrelay = \"off\"\n"));
    REQUIRE(theme);
    CHECK(theme->appearance.theme.as_str() == "nord");
    CHECK(*encode(*theme) == slurp(GOLDEN / "theme.saved.toml"));

    auto unknown = decode(*parse(slurp(GOLDEN / "unknown.toml")));
    REQUIRE(unknown);
    CHECK(*encode(*unknown) == slurp(GOLDEN / "unknown.saved.toml"));
    unknown->appearance.theme = ThemeRef{"light"};
    unknown->mcp_servers.erase("drop");
    CHECK(*encode(*unknown) == slurp(GOLDEN / "unknown.edited.toml"));
}

TEST_CASE("unknown keys are reported by dotted path") {
    auto c = decode(*parse(slurp(FIXTURES / "unknown.toml")));
    REQUIRE(c);
    std::vector<std::string> paths;
    for (const auto& k : c->unknown) paths.push_back(k.dotted());
    // serde_ignored order: document keys sorted, depth first.
    CHECK((paths == std::vector<std::string>{"future_flag", "future_section", "mcp_servers.drop.x-color",
                                            "mcp_servers.keep.x-color", "network.x-mtu",
                                            "projects.p.future_digest_mode", "unattended.plan.escalate_to"}));
    CHECK(c->unknown[1].value.as_table()->at("level") == toml::Value(3));
}

TEST_CASE("missing file and sections get the defaults") {
    auto dir = tempdir();
    ConfigStore store = ConfigStore::in_dir(dir.path());
    auto c = store.load();
    REQUIRE(c);
    CHECK(*c == Config{});
    CHECK(!fs::exists(store.path()));

    auto f = store_with("version = 1\n[remote_hosts.box]\nhost = \"box\"\n");
    auto loaded = f.store.load();
    REQUIRE(loaded);
    CHECK(loaded->network.port == 31415);
    CHECK(loaded->network.relay == Relay{});
    CHECK(loaded->harness.default_ == "omp");
    CHECK(loaded->appearance.theme.as_str() == ThemeRef::SYSTEM);
    CHECK(loaded->surfaces.video);
    CHECK(loaded->network.discovery.tailscale);
    const RemoteHost& host = loaded->remote_hosts.at("box");
    CHECK(host.read_only);
    CHECK(!host.commands);
    CHECK(!host.auto_approve);
    CHECK(loaded->project("never-configured").digest_role == "smol");
}

TEST_CASE("unknown keys survive load edit save") {
    auto [dir, store] = store_with(slurp(FIXTURES / "unknown.toml"));
    auto edited = store.edit([](Config& c) {
        c.appearance.theme = ThemeRef{"light"};
        c.mcp_servers.erase("drop");
    });
    REQUIRE(edited);
    toml::Table r = raw(store.path());
    CHECK(*at(r, {"theme"}).as_string() == "light");
    CHECK(*at(r, {"future_flag"}).as_bool());
    CHECK(*at(r, {"future_section", "level"}).as_integer() == 3);
    CHECK(at(r, {"future_section", "when"}).as_datetime());
    CHECK(*at(r, {"network", "x-mtu"}).as_integer() == 1200);
    CHECK(*at(r, {"unattended", "plan", "escalate_to"}).as_string() == "owner");
    CHECK(*at(r, {"mcp_servers", "keep", "x-color"}).as_string() == "red");
    CHECK_MESSAGE(!at(r, {"mcp_servers"}).as_table()->contains("drop"), "deleted server came back");
    CHECK(*at(r, {"projects", "p", "future_digest_mode"}).as_string() == "dense");

    Config again = load_from_disk(store);
    REQUIRE(store.save(again));
    CHECK(r == raw(store.path()));
}

TEST_CASE("invalid values name the offending key") {
    const std::vector<std::pair<std::string, std::string>> cases = {
        {"[network]\nport = \"high\"", "network.port"},
        {"[network]\nport = 0", "network.port"},
        {"[network]\nrelay = \"ftp://relay\"", "network.relay"},
        {"theme = 5", "theme"},
        {"[harness]\ndefault = \"\"", "harness.default"},
        {"[harness]\nexecutables = { omp = \"\" }", "harness.executables.omp"},
        {"[mcp_servers.s]\ncommand = \"a\"\nurl = \"https://b\"", "mcp_servers.s"},
        {"[mcp_servers.s]\nenabled = true", "mcp_servers.s"},
        {"[mcp_servers.s]\ncommand = \" \"", "mcp_servers.s.command"},
        {"[mcp_servers.s]\nurl = \"localhost:80\"", "mcp_servers.s.url"},
        {"[mcp_servers.s]\ncommand = \"a\"\nargs = 5", "mcp_servers.s.args"},
        {"[remote_hosts.h]\nuser = \"me\"", "remote_hosts.h"},
        {"[remote_hosts.h]\nhost = \"h\"\nport = 0", "remote_hosts.h.port"},
        {"[remote_hosts.h]\nhost = \"h\"\nallowed_paths = [\"srv\"]", "remote_hosts.h.allowed_paths"},
        {"[unattended.approval]\ntimeout_secs = 5\nanswer = \"maybe\"", "unattended.approval.answer"},
        {"[unattended.plan]\ntimeout_secs = 5\nanswer = \"edit\"", "unattended.plan.answer"},
        {"[unattended.form]\ntimeout_secs = 5\nanswer = \"[1]\"", "unattended.form.answer"},
        {"[unattended.question]\ntimeout_secs = 0\nanswer = \"yes\"", "unattended.question.timeout_secs"},
        {"[projects.p]\ntoken_budget = 0", "projects.p.token_budget"},
        {"[updater]\ncheck_interval_secs = 0", "updater.check_interval_secs"},
        {"version = \"one\"", "version"},
        {"version = -1", "version"},
    };
    for (const auto& [snippet, key] : cases) {
        std::string text = snippet.starts_with("version") ? snippet + "\n" : "version = 1\n" + snippet + "\n";
        ConfigError err = load_err(text);
        INFO("for config:\n" << text << "got " << err.to_string());
        REQUIRE(err.kind == Kind::Invalid);
        CHECK(err.key == key);
        CHECK(err.to_string().find(key) != std::string::npos);
    }
}

TEST_CASE("saved files hold only what is set") {
    const std::vector<std::tuple<std::string, std::string, bool>> cases = {
        {"", "version = 1", true},
        {"", "theme = \"system\"", true},
        {"", "project_tabs", false},
        {"", "github_token", false},
        {"", "trusted_keys", false},
        {"", "pinned", false},
        {"project_tabs = true", "project_tabs = true", true},
        {"theme = \"light\"", "theme = \"light\"", true},
        {"[theme]\nname = \"dark\"", "theme = \"dark\"", true},
        {"[theme]\nname = \"nord\"\naccent = \"#88c0d0\"", "accent = \"#88c0d0\"", true},
        {"[network]\nrelay = \"off\"", "relay = \"off\"", true},
        {"[updater]\ntrusted_keys = [\"ab\"]", "trusted_keys = [\"ab\"]", true},
        {"[surfaces]\nbrowser_capture = \"virtual-display\"", "browser_capture = \"virtual-display\"", true},
        {"[mcp_servers.s]\ncommand = \"a\"", "args", false},
    };
    for (const auto& [snippet, needle, present] : cases) {
        auto [dir, store] = store_with("version = 1\n" + snippet + "\n");
        auto c = store.load();
        REQUIRE(c);
        REQUIRE(store.save(*c));
        std::string saved = slurp(store.path());
        INFO(needle << " after loading " << snippet << ":\n" << saved);
        CHECK((saved.find(needle) != std::string::npos) == present);
        CHECK(load_from_disk(store) == *c);
    }
}

TEST_CASE("refused files are left alone") {
    CHECK(load_err("theme = \"dark\"\n").kind == Kind::MissingVersion);
    CHECK(load_err("version = 1\n[network\n").kind == Kind::Parse);
    ConfigError too_new = load_err("version = 99\n");
    CHECK(too_new.kind == Kind::TooNew);
    CHECK(too_new.found == 99);
    CHECK(too_new.supported == CURRENT_VERSION);
    ConfigError old = load_err("version = 0\n");
    CHECK(old.kind == Kind::MissingMigration);
    CHECK(old.from == 0);

    auto [dir, store] = store_with(full());
    auto err = store.edit([](Config& c) { c.network.port = 0; });
    REQUIRE(!err);
    CHECK(err.error().kind == Kind::Invalid);
    CHECK(err.error().key == "network.port");
    CHECK(slurp(store.path()) == full());
}

TEST_CASE("load sees external edit") {
    auto [dir, store] = store_with(full());
    auto c = store.load();
    REQUIRE(c);
    REQUIRE(store.save(*c));
    CHECK(*store.load() == *c);

    // A longer file, so the length alone shows the edit.
    std::string saved = slurp(store.path());
    spit(store.path(), "version = 1\ntheme = \"light\"\n# " + std::string(saved.size(), 'x') + "\n");
    CHECK(store.load()->appearance.theme == ThemeRef{"light"});
}

TEST_CASE("edit saves only changes and watch counts saves") {
    auto dir = tempdir();
    ConfigStore store = ConfigStore::in_dir(dir.path() / "new");
    SaveGeneration saves = ConfigStore(store).watch();
    auto unchanged = store.edit([](Config& c) { return c.project_tabs; });
    REQUIRE(unchanged);
    CHECK(!*unchanged);
    CHECK_MESSAGE(!fs::exists(store.path()), "an edit that changes nothing doesn't save");
    REQUIRE(store.edit([](Config& c) { c.pinned.insert("s1"); }));
    CHECK(saves.value() == 1);
    std::vector<fs::path> names;
    for (const auto& e : fs::directory_iterator(store.path().parent_path())) names.push_back(e.path());
    CHECK_MESSAGE((names == std::vector<fs::path>{store.path()}), "no temp files are left");
}

TEST_CASE("apply_to_session adds enabled servers, toggles and executable") {
    auto [dir, store] = store_with(full());
    auto c = store.load();
    REQUIRE(c);
    loa::contracts::SessionConfig session;
    session.project = "loa";
    session.disabled_tools.push_back("task");
    c->apply_to_session("omp", session);
    std::vector<std::string> names;
    for (const auto& s : session.mcp_servers) names.push_back(s.name);
    CHECK_MESSAGE((names == std::vector<std::string>{"files", "search"}), "disabled servers must not reach the harness");
    CHECK((session.disabled_tools == std::vector<std::string>{"task", "bash"}));
    CHECK(session.executable == fs::path("/opt/omp/bin/omp"));

    fs::path staged = "/staging/omp";
    loa::contracts::SessionConfig staged_session;
    staged_session.executable = staged;
    c->apply_to_session("omp", staged_session);
    CHECK_MESSAGE(staged_session.executable == staged, "the updater's staged executable wins");
}

TEST_CASE("config_path and file note forms") {
    CHECK(config_path("/etc/loa") == fs::path("/etc/loa/config.toml"));
    CHECK(DEFAULT_PORT == 31415);
    using loa::settings::FileNote;
    CHECK(FileNote::parse("src/a.rs: the parser") == FileNote{"src/a.rs", "the parser"});
    CHECK(FileNote::parse("src/a.rs") == FileNote{"src/a.rs", ""});
    CHECK(FileNote{"a", "b"}.encode() == "a: b");
    CHECK(FileNote{"a", ""}.encode() == "a");
}
