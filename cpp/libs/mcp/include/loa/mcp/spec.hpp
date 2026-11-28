#pragma once

// Plain data describing a registered MCP server.

#include <filesystem>
#include <map>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/mcp/error.hpp"
#include "loa/runtime/json.hpp"

namespace loa::mcp {

struct StdioSpec {
    std::string command;
    std::vector<std::string> args;
    // Added to the app's own environment.
    std::map<std::string, std::string> env;
    // Working directory; the app's own when absent.
    std::optional<std::filesystem::path> cwd;

    // Parses a shell style command line such as `GITHUB_TOKEN=abc npx -y "@scope/server" --flag`.
    // Leading `NAME=value` words become environment variables.
    [[nodiscard]] static Result<StdioSpec> parse(std::string_view line);

    // The command line `parse` reads back into this spec, for the edit form. None when a word holds a NUL byte.
    [[nodiscard]] std::optional<std::string> command_line() const;

    friend bool operator==(const StdioSpec&, const StdioSpec&) = default;
};

struct HttpSpec {
    std::string url;
    // Sent with every request, e.g. `Authorization`.
    std::map<std::string, std::string> headers;

    // An HTTP server at `url` (trimmed) with extra request headers.
    [[nodiscard]] static Result<HttpSpec> make(std::string_view url, std::map<std::string, std::string> headers = {});

    [[nodiscard]] Result<> validate() const;

    friend bool operator==(const HttpSpec&, const HttpSpec&) = default;
};

// How the client reaches the server; serialized with a `type` tag of `stdio` or `http`.
using Transport = std::variant<StdioSpec, HttpSpec>;

// One registered server, as the central config stores it.
struct ServerSpec {
    std::string name;
    // A disabled server is kept in the config but never started.
    bool enabled = true;
    Transport transport;

    // A validated spec, enabled.
    [[nodiscard]] static Result<ServerSpec> make(std::string name, Transport transport);

    // Checks the name, and the url and headers of an HTTP server. Specs that come from a
    // store or an import go through this before they are used.
    [[nodiscard]] Result<> validate() const;

    // Whether both specs reach the same server, whatever their names.
    [[nodiscard]] bool same_server(const ServerSpec& other) const;

    // The serde form: `{"name", "enabled", "type", ...transport fields}`; empty lists, maps and
    // an absent `cwd` are skipped.
    [[nodiscard]] runtime::json::Value to_json() const;
    [[nodiscard]] static std::expected<ServerSpec, std::string> from_json(const runtime::json::Value& value);

    friend bool operator==(const ServerSpec&, const ServerSpec&) = default;
};

// Server names key the config and prefix tool names in harnesses, so they are limited to
// letters, digits, `-`, `_` and `.`.
[[nodiscard]] Result<> validate_name(std::string_view name);

// Parses one `Name: value` header line from the add server form.
[[nodiscard]] Result<std::pair<std::string, std::string>> parse_header(std::string_view line);

// Whether two URLs are equal once parsed (scheme and host case, default port, empty path);
// unparsable ones compare as text.
[[nodiscard]] bool same_url(std::string_view a, std::string_view b);

} // namespace loa::mcp
