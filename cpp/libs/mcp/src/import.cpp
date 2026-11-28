#include "loa/mcp/import.hpp"

#include <algorithm>
#include <fstream>
#include <map>
#include <sstream>
#include <system_error>

#include "loa/runtime/json.hpp"
#include "loa/settings/app/toml.hpp"

namespace loa::mcp::import {

namespace json = loa::runtime::json;
namespace toml = loa::settings::app::toml;

namespace {

// Format-neutral access to one server entry: the entry as a JSON object (TOML tables are
// converted the way serde_json serializes them).
class ConfigValue {
  public:
    explicit ConfigValue(const json::Value& object) : object_(object) {}

    // The value at `key`; absent and null are both none.
    [[nodiscard]] const json::Value* get(std::string_view key) const {
        const auto* value = object_.find(key);
        return value == nullptr || value->is_null() ? nullptr : value;
    }

    [[nodiscard]] std::expected<std::optional<std::string>, std::string> str(std::string_view key) const {
        const auto* value = get(key);
        if (value == nullptr) return std::optional<std::string>();
        if (const auto* text = value->as_string()) return std::optional(*text);
        return std::unexpected("`" + std::string(key) + "` isn't a string");
    }

    [[nodiscard]] std::expected<std::vector<std::string>, std::string> strings(std::string_view key) const {
        const auto* value = get(key);
        if (value == nullptr) return std::vector<std::string>();
        const auto* items = value->as_array();
        if (items == nullptr) return std::unexpected("`" + std::string(key) + "` isn't a list");
        std::vector<std::string> out;
        for (const auto& item : *items) {
            const auto* text = item.as_string();
            if (text == nullptr) return std::unexpected("`" + std::string(key) + "` holds a non string");
            out.push_back(*text);
        }
        return out;
    }

    // A map of strings; numbers and booleans are written as text.
    [[nodiscard]] std::expected<std::map<std::string, std::string>, std::string> map(std::string_view key) const {
        const auto* value = get(key);
        if (value == nullptr) return std::map<std::string, std::string>();
        const auto* items = value->as_object();
        if (items == nullptr) return std::unexpected("`" + std::string(key) + "` isn't a map");
        std::map<std::string, std::string> out;
        for (const auto& [name, item] : *items) {
            if (const auto* text = item.as_string()) out.insert_or_assign(name, *text);
            else if (item.is_number() || item.is_bool()) out.insert_or_assign(name, json::dump(item));
            else return std::unexpected("`" + std::string(key) + "." + name + "` isn't a string");
        }
        return out;
    }

    [[nodiscard]] std::optional<bool> flag(std::string_view key) const {
        const auto* value = get(key);
        if (value == nullptr || value->as_bool() == nullptr) return std::nullopt;
        return *value->as_bool();
    }

  private:
    const json::Value& object_;
};

#define LOA_TRY(var, expr)                                                    \
    auto var##_result = (expr);                                               \
    if (!var##_result) return std::unexpected(std::move(var##_result.error())); \
    auto var = std::move(*var##_result)

// One entry in any of the dialects: Claude/Cursor `type`/`headers`/`disabled`, Codex
// `http_headers`/`bearer_token`/`enabled`.
SpecOrReason entry_spec(std::string_view name, const ConfigValue& entry) {
    LOA_TRY(kind, entry.str("type"));
    const bool http = kind && (*kind == "http" || *kind == "streamable-http" || *kind == "streamableHttp");
    if (kind && *kind == "sse") return std::unexpected(std::string("SSE servers aren't supported, only streamable HTTP"));
    if (kind && *kind != "stdio" && !http) return std::unexpected("transport `" + *kind + "` isn't supported");
    if (entry.get("envFile") != nullptr) return std::unexpected(std::string("`envFile` isn't supported"));
    LOA_TRY(token_var, entry.str("bearer_token_env_var"));
    if (token_var)
        return std::unexpected("the bearer token comes from the environment variable `" + *token_var + "`, which isn't supported");
    LOA_TRY(env_headers, entry.map("env_http_headers"));
    if (!env_headers.empty())
        return std::unexpected(std::string("headers taken from environment variables (`env_http_headers`) aren't supported"));
    LOA_TRY(command, entry.str("command"));
    LOA_TRY(url, entry.str("url"));
    Transport transport;
    if (command && !http) {
        LOA_TRY(cwd, entry.str("cwd"));
        LOA_TRY(args, entry.strings("args"));
        LOA_TRY(env, entry.map("env"));
        StdioSpec stdio{std::move(*command), std::move(args), std::move(env), std::nullopt};
        if (cwd) stdio.cwd = Path(*cwd);
        transport = std::move(stdio);
    } else if (url) {
        LOA_TRY(headers, entry.map("headers"));
        LOA_TRY(http_headers, entry.map("http_headers"));
        for (auto& [k, v] : http_headers) headers.insert_or_assign(k, std::move(v));
        LOA_TRY(token, entry.str("bearer_token"));
        if (token) headers.insert_or_assign("Authorization", "Bearer " + *token);
        transport = HttpSpec{std::move(*url), std::move(headers)};
    } else {
        return std::unexpected(std::string("the entry needs a `command` or a `url`"));
    }
    const bool enabled = entry.flag("enabled").value_or(true) && !entry.flag("disabled").value_or(false);
    ServerSpec spec{std::string(name), enabled, std::move(transport)};
    if (auto ok = spec.validate(); !ok) return std::unexpected(ok.error().to_string());
    return spec;
}

#undef LOA_TRY

// `serde_json::to_value` of a TOML value.
json::Value to_json(const toml::Value& value) {
    return std::visit(
        [](const auto& v) -> json::Value {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, toml::Datetime>) {
                return json::Object{{"$__toml_private_datetime", v.text}};
            } else if constexpr (std::is_same_v<T, toml::Array>) {
                json::Array out;
                for (const auto& item : v) out.push_back(to_json(item));
                return out;
            } else if constexpr (std::is_same_v<T, toml::Table>) {
                json::Object out;
                for (const auto& [k, item] : v) out.emplace_back(k, to_json(item));
                return out;
            } else {
                return json::Value(v);
            }
        },
        value.data);
}

} // namespace

std::expected<std::vector<Found>, std::string> ImportSource::read() const {
    std::error_code ec;
    if (!std::filesystem::exists(path(), ec) && !ec) return std::vector<Found>();
    std::ifstream in(path(), std::ios::binary);
    if (!in) return std::unexpected("reading " + path().string() + ": " + std::make_error_code(std::errc::io_error).message());
    std::ostringstream text;
    text << in.rdbuf();
    auto entries = this->entries(text.str());
    if (!entries) return std::unexpected("parsing " + path().string() + ": " + entries.error());
    std::vector<Found> out;
    for (auto& [name, spec] : *entries) out.push_back(Found{app(), path(), std::move(name), std::move(spec)});
    return out;
}

std::expected<std::vector<Entry>, std::string> JsonServers::entries(std::string_view text) const {
    auto root = json::parse(text);
    if (!root) return std::unexpected(root.error().message());
    std::vector<const json::Value*> scopes{&*root};
    if (project_) {
        if (const auto* projects = root->find("projects"))
            if (const auto* local = projects->find(project_->string())) scopes.push_back(local);
    }
    std::vector<Entry> out;
    for (const auto* scope : scopes) {
        const auto* servers = scope->find("mcpServers");
        if (servers == nullptr || servers->as_object() == nullptr) continue;
        std::vector<Entry> entries;
        for (const auto& [name, entry] : *servers->as_object()) {
            if (!entry.is_object()) entries.emplace_back(name, std::unexpected(std::string("the entry isn't an object")));
            else entries.emplace_back(name, entry_spec(name, ConfigValue(entry)));
        }
        std::ranges::sort(entries, {}, &Entry::first);
        for (auto& entry : entries) out.push_back(std::move(entry));
    }
    return out;
}

std::expected<std::vector<Entry>, std::string> CodexToml::entries(std::string_view text) const {
    auto root = toml::parse(text);
    if (!root) return std::unexpected(root.error());
    std::vector<Entry> out;
    auto it = root->find("mcp_servers");
    if (it == root->end() || it->second.as_table() == nullptr) return out;
    for (const auto& [name, value] : *it->second.as_table()) {
        if (value.as_table() == nullptr) {
            out.emplace_back(name, std::unexpected(std::string("the entry isn't a table")));
            continue;
        }
        const auto converted = to_json(value);
        out.emplace_back(name, entry_spec(name, ConfigValue(converted)));
    }
    return out;
}

std::vector<std::unique_ptr<ImportSource>> defaults(const Path& home, const std::optional<Path>& project) {
    std::vector<std::unique_ptr<ImportSource>> files;
    files.push_back(std::make_unique<JsonServers>(App::Claude, home / ".claude.json", project));
    if (project) files.push_back(std::make_unique<JsonServers>(App::Claude, *project / ".mcp.json"));
    files.push_back(std::make_unique<JsonServers>(App::Cursor, home / ".cursor/mcp.json"));
    if (project) files.push_back(std::make_unique<JsonServers>(App::Cursor, *project / ".cursor/mcp.json"));
    files.push_back(std::make_unique<CodexToml>(home / ".codex/config.toml"));
    return files;
}

std::vector<ServerSpec> ImportReport::imported() const {
    std::vector<ServerSpec> out;
    for (const auto& entry : entries)
        if (const auto* imported = std::get_if<outcome::Imported>(&entry.outcome)) out.push_back(imported->spec);
    return out;
}

ImportReport plan_found(std::span<const ServerSpec> existing, std::vector<Found> found) {
    std::vector<ServerSpec> known(existing.begin(), existing.end());
    ImportReport report;
    for (auto& item : found) {
        Outcome outcome;
        if (!item.spec) {
            outcome = outcome::Unsupported{std::move(item.spec.error())};
        } else {
            auto& spec = *item.spec;
            auto other = std::ranges::find_if(known, [&](const ServerSpec& o) { return o.same_server(spec) || o.name == spec.name; });
            if (other != known.end()) {
                outcome = outcome::Duplicate{other->name};
            } else {
                known.push_back(spec);
                outcome = outcome::Imported{std::move(spec)};
            }
        }
        report.entries.push_back(ImportEntry{item.app, std::move(item.file), std::move(item.name), std::move(outcome)});
    }
    return report;
}

std::pair<std::vector<Found>, std::vector<FileError>> scan(std::span<const std::unique_ptr<ImportSource>> files) {
    std::pair<std::vector<Found>, std::vector<FileError>> out;
    for (const auto& file : files) {
        auto entries = file->read();
        if (entries) {
            for (auto& entry : *entries) out.first.push_back(std::move(entry));
        } else {
            out.second.push_back(FileError{file->path(), std::move(entries.error())});
        }
    }
    return out;
}

} // namespace loa::mcp::import
