// Ports of the inline tests of `spec.rs` and `manager.rs`.

#include <doctest/doctest.h>

#include "loa/mcp/manager.hpp"
#include "loa/mcp/spec.hpp"

using namespace loa::mcp;
namespace json = loa::runtime::json;
using Env = std::map<std::string, std::string>;
using Words = std::vector<std::string>;

TEST_CASE("command lines parse and round trip") {
    struct Case {
        const char* line;
        const char* command;
        Words args;
        Env env;
    };
    const Case cases[] = {
        {R"(TOKEN=a\ b DEBUG=1 npx -y "@scope/server name" --dir='/tmp/x y')", "npx", {"-y", "@scope/server name", "--dir=/tmp/x y"},
         {{"TOKEN", "a b"}, {"DEBUG", "1"}}},
        {"server KEY=value", "server", {"KEY=value"}, {}},
    };
    for (const auto& c : cases) {
        CAPTURE(c.line);
        auto spec = StdioSpec::parse(c.line);
        REQUIRE(spec);
        CHECK(spec->command == c.command);
        CHECK(spec->args == c.args);
        CHECK(spec->env == c.env);
        auto line = spec->command_line();
        REQUIRE(line);
        auto back = StdioSpec::parse(*line);
        REQUIRE(back);
        CHECK(*back == *spec);
    }
    const std::pair<const char*, const char*> bad[] = {{"  ", "empty"}, {"A=1", "empty"}, {"npx 'open", "unclosed"}};
    for (const auto& [line, want] : bad) {
        CAPTURE(line);
        auto spec = StdioSpec::parse(line);
        REQUIRE(!spec);
        CHECK(spec.error().to_string().find(want) != std::string::npos);
    }
}

TEST_CASE("urls, headers and names are checked") {
    auto header = [](const char* name) { return std::map<std::string, std::string>{{name, "x"}}; };
    CHECK(HttpSpec::make("https://mcp.example.com/mcp"));
    CHECK(HttpSpec::make("ftp://example.com").error().kind == Error::Kind::Url);
    CHECK(HttpSpec::make("not a url").error().kind == Error::Kind::Url);
    CHECK(HttpSpec::make("http://127.0.0.1:1/mcp", header("Mcp-Session-Id")).error().kind == Error::Kind::ReservedHeader);
    CHECK(HttpSpec::make("http://127.0.0.1:1/mcp", header("Bad Header")).error().kind == Error::Kind::Header);
    CHECK(HttpSpec::make("http://127.0.0.1:1/mcp", header("Authorization")));
    CHECK(HttpSpec::make("  https://trimmed.example.com/mcp ")->url == "https://trimmed.example.com/mcp");
    CHECK(*parse_header("Authorization:  Bearer abc ") == std::pair<std::string, std::string>{"Authorization", "Bearer abc"});
    CHECK(!parse_header("no colon"));
    const std::pair<const char*, bool> names[] = {{"hub-mcp_2.0", true}, {"", false}, {"has space", false}, {"a/b", false}};
    for (const auto& [name, ok] : names) {
        CAPTURE(name);
        CHECK(validate_name(name).has_value() == ok);
    }
    auto spec = ServerSpec::make("fs", *StdioSpec::parse("mcp-fs /tmp"));
    REQUIRE(spec);
    const auto value = spec->to_json();
    CHECK(json::dump(value) == R"({"name":"fs","enabled":true,"type":"stdio","command":"mcp-fs","args":["/tmp"]})");
    CHECK(*ServerSpec::from_json(value) == *spec);
    auto defaulted = ServerSpec::from_json(*json::parse(R"({"name":"w","type":"http","url":"https://x.test"})"));
    REQUIRE(defaulted);
    CHECK(defaulted->enabled);
}

TEST_CASE("same_server compares transports, not names") {
    auto stdio = [](const char* name, const char* line) { return *ServerSpec::make(name, *StdioSpec::parse(line)); };
    auto http = [](const char* name, const char* url) { return *ServerSpec::make(name, *HttpSpec::make(url)); };
    CHECK(stdio("a", "srv -v").same_server(stdio("b", "ENV=1 srv -v")));
    CHECK(!stdio("a", "srv -v").same_server(stdio("a", "srv")));
    CHECK(http("a", "https://X.test/mcp").same_server(http("b", "https://x.test:443/mcp")));
    CHECK(http("a", "https://x.test").same_server(http("b", "https://x.test/")));
    CHECK(!http("a", "https://x.test/a").same_server(http("b", "https://x.test/b")));
    CHECK(!http("a", "https://x.test").same_server(stdio("a", "srv")));
}

TEST_CASE("backoff doubles up to the cap") {
    const Backoff backoff{std::chrono::milliseconds(100), std::chrono::seconds(1)};
    std::vector<long long> delays;
    for (std::uint32_t attempt = 0; attempt < 6; ++attempt) delays.push_back(backoff.delay(attempt).count());
    CHECK(delays == std::vector<long long>{100, 200, 400, 800, 1000, 1000});
    CHECK(backoff.delay(UINT32_MAX) == std::chrono::seconds(1));

    const Options defaults;
    CHECK(defaults.backoff.initial == std::chrono::milliseconds(500));
    CHECK(defaults.backoff.max == std::chrono::seconds(30));
    CHECK(defaults.connect_timeout == std::chrono::seconds(30));
    std::vector<long long> restarts;
    for (std::uint32_t attempt = 0; attempt < 9; ++attempt) restarts.push_back(defaults.backoff.delay(attempt).count());
    CHECK(restarts == std::vector<long long>{500, 1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000});
}

TEST_CASE("error messages read like the Rust ones") {
    CHECK(Error::invalid_name("a b").to_string() == "`a b` isn't a valid server name: use letters, digits, `-`, `_` or `.`");
    CHECK(Error::reserved_header("Accept").to_string() == "the `Accept` header is set by the MCP client and can't be overridden");
    CHECK(Error::not_allowed("s", "t").to_string() == "tool `t` of server `s` isn't allowed here");
}
