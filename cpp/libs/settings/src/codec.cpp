// Conversion between the raw TOML document and `Config`. The decoder mirrors serde's
// behavior for the Rust schema: fields in key order, serde error messages, the path of
// the failing value (serde_path_to_error) and the paths of ignored keys (serde_ignored).

#include "loa/settings/app/codec.hpp"

#include <limits>

namespace loa::settings::app {
namespace {

using toml::Array;
using toml::Table;
using toml::Value;
using Path = std::vector<std::string>;

struct DecodeError {
    Path path;
    std::string message;
};

std::string join(const Path& path) {
    if (path.empty()) return "(top level)";
    std::string s;
    for (const auto& seg : path) {
        if (!s.empty() && !seg.starts_with('[')) s += '.';
        s += seg;
    }
    return s;
}

[[noreturn]] void fail(const Path& path, std::string message) { throw DecodeError{path, std::move(message)}; }

// serde's `Unexpected` text for a value.
std::string unexpected(const Value& v) {
    if (auto* s = v.as_string()) return "string \"" + *s + "\"";
    if (auto* i = v.as_integer()) return "integer `" + std::to_string(*i) + "`";
    if (auto* f = v.as_float()) return "floating point `" + toml::write_value(*f) + "`";
    if (auto* b = v.as_bool()) return std::string("boolean `") + (*b ? "true" : "false") + "`";
    if (v.as_array()) return "sequence";
    return "map";
}

[[noreturn]] void invalid_type(const Path& path, const Value& v, std::string_view expected) {
    fail(path, "invalid type: " + unexpected(v) + ", expected " + std::string(expected));
}

Path child(const Path& path, std::string key) {
    Path p = path;
    p.push_back(std::move(key));
    return p;
}

struct Decoder {
    std::vector<Path> ignored;

    std::string string(const Value& v, const Path& path, std::string_view expected = "a string") {
        if (auto* s = v.as_string()) return *s;
        invalid_type(path, v, expected);
    }

    std::filesystem::path path_value(const Value& v, const Path& path) {
        return std::filesystem::path(string(v, path, "path string"));
    }

    bool boolean(const Value& v, const Path& path) {
        if (auto* b = v.as_bool()) return *b;
        invalid_type(path, v, "a boolean");
    }

    template <typename U>
    U uint(const Value& v, const Path& path, std::string_view name) {
        auto* i = v.as_integer();
        if (!i) invalid_type(path, v, name);
        if (*i < 0 || static_cast<std::uint64_t>(*i) > std::numeric_limits<U>::max())
            fail(path, "invalid value: integer `" + std::to_string(*i) + "`, expected " + std::string(name));
        return static_cast<U>(*i);
    }

    const Table& table(const Value& v, const Path& path, std::string_view expected) {
        if (auto* t = v.as_table()) return *t;
        invalid_type(path, v, expected);
    }

    const Array& array(const Value& v, const Path& path) {
        if (auto* a = v.as_array()) return *a;
        invalid_type(path, v, "a sequence");
    }

    std::vector<std::string> strings(const Value& v, const Path& path) {
        std::vector<std::string> out;
        const Array& a = array(v, path);
        for (std::size_t i = 0; i < a.size(); ++i) out.push_back(string(a[i], child(path, "[" + std::to_string(i) + "]")));
        return out;
    }

    std::set<std::string> string_set(const Value& v, const Path& path) {
        auto list = strings(v, path);
        return {list.begin(), list.end()};
    }

    std::map<std::string, std::string> string_map(const Value& v, const Path& path) {
        std::map<std::string, std::string> out;
        for (const auto& [k, e] : table(v, path, "a map")) out.emplace(k, string(e, child(path, k)));
        return out;
    }

    void ignore(const Path& path, const std::string& key) { ignored.push_back(child(path, key)); }

    [[noreturn]] void missing(const Path& path, std::string_view field) {
        fail(path, "missing field `" + std::string(field) + "`");
    }

    ActiveProject active_project(const Value& v, const Path& path) {
        const Table& t = table(v, path, "struct ActiveProject");
        ActiveProject out;
        bool name = false, cwd = false;
        for (const auto& [k, e] : t) {
            if (k == "name") out.name = string(e, child(path, k)), name = true;
            else if (k == "cwd") out.cwd = path_value(e, child(path, k)), cwd = true;
            else ignore(path, k);
        }
        if (!name) missing(path, "name");
        if (!cwd) missing(path, "cwd");
        return out;
    }

    // Datetimes go through serde's buffered `Content` in the flattened overrides and come
    // out as strings, as in the Rust app.
    static Value strip_datetimes(Value v) {
        if (auto* d = v.as_datetime()) return Value(d->text);
        if (auto* t = v.as_table()) {
            for (auto& [k, e] : *t) e = strip_datetimes(std::move(e));
        } else if (auto* a = std::get_if<Array>(&v.data)) {
            for (auto& e : *a) e = strip_datetimes(std::move(e));
        }
        return v;
    }

    Appearance appearance(const Value& v, const Path& path) {
        if (auto* s = v.as_string()) return Appearance{ThemeRef{*s}, {}};
        if (auto* t = v.as_table()) {
            Appearance out;
            bool ok = true;
            for (const auto& [k, e] : *t) {
                if (k == "name") {
                    if (auto* s = e.as_string()) out.theme = ThemeRef{*s};
                    else ok = false;
                } else {
                    out.overrides.emplace(k, strip_datetimes(e));
                }
            }
            if (ok) return out;
        }
        fail(path, "data did not match any variant of untagged enum AppearanceRepr");
    }

    HarnessSettings harness(const Value& v, const Path& path) {
        HarnessSettings out;
        for (const auto& [k, e] : table(v, path, "struct HarnessSettings")) {
            Path p = child(path, k);
            if (k == "default") out.default_ = string(e, p);
            else if (k == "executables") {
                for (const auto& [name, exe] : table(e, p, "a map")) out.executables.emplace(name, path_value(exe, child(p, name)));
            } else if (k == "approval_mode") out.approval_mode = string(e, p);
            else ignore(path, k);
        }
        return out;
    }

    Relay relay(const Value& v, const Path& path) {
        if (auto* s = v.as_string()) return Relay::from_string(*s);
        fail(path, "data did not match any variant of untagged enum Relay");
    }

    NetworkSettings network(const Value& v, const Path& path) {
        NetworkSettings out;
        for (const auto& [k, e] : table(v, path, "struct NetworkSettings")) {
            Path p = child(path, k);
            if (k == "port") out.port = uint<std::uint16_t>(e, p, "u16");
            else if (k == "relay") out.relay = relay(e, p);
            else if (k == "discovery") {
                for (const auto& [dk, de] : table(e, p, "struct Discovery")) {
                    if (dk == "mdns") out.discovery.mdns = boolean(de, child(p, dk));
                    else if (dk == "tailscale") out.discovery.tailscale = boolean(de, child(p, dk));
                    else ignore(p, dk);
                }
            } else ignore(path, k);
        }
        return out;
    }

    UpdaterSettings updater(const Value& v, const Path& path) {
        UpdaterSettings out;
        for (const auto& [k, e] : table(v, path, "struct UpdaterSettings")) {
            Path p = child(path, k);
            if (k == "check_interval_secs") out.check_interval_secs = uint<std::uint64_t>(e, p, "u64");
            else if (k == "pins") out.pins = string_map(e, p);
            else if (k == "github_token") out.github_token = string(e, p);
            else if (k == "trusted_keys") out.trusted_keys = strings(e, p);
            else ignore(path, k);
        }
        return out;
    }

    DecisionDefault decision_default(const Value& v, const Path& path) {
        DecisionDefault out;
        bool timeout = false, answer = false;
        for (const auto& [k, e] : table(v, path, "struct DecisionDefault")) {
            Path p = child(path, k);
            if (k == "timeout_secs") out.timeout_secs = uint<std::uint64_t>(e, p, "u64"), timeout = true;
            else if (k == "answer") out.answer = string(e, p), answer = true;
            else if (k == "reason") out.reason = string(e, p);
            else ignore(path, k);
        }
        if (!timeout) missing(path, "timeout_secs");
        if (!answer) missing(path, "answer");
        return out;
    }

    UnattendedSettings unattended(const Value& v, const Path& path) {
        UnattendedSettings out;
        for (const auto& [k, e] : table(v, path, "struct UnattendedSettings")) {
            Path p = child(path, k);
            if (k == "approval") out.approval = decision_default(e, p);
            else if (k == "question") out.question = decision_default(e, p);
            else if (k == "plan") out.plan = decision_default(e, p);
            else if (k == "form") out.form = decision_default(e, p);
            else ignore(path, k);
        }
        return out;
    }

    SurfaceSettings surfaces(const Value& v, const Path& path) {
        SurfaceSettings out;
        for (const auto& [k, e] : table(v, path, "struct SurfaceSettings")) {
            Path p = child(path, k);
            if (k == "video") out.video = boolean(e, p);
            else if (k == "browser_capture") {
                std::string s = string(e, p, "enum BrowserCapture");
                if (s == "screencast") out.browser_capture = BrowserCapture::Screencast;
                else if (s == "virtual-display") out.browser_capture = BrowserCapture::VirtualDisplay;
                else fail(p, "unknown variant `" + s + "`, expected `screencast` or `virtual-display`");
            } else ignore(path, k);
        }
        return out;
    }

    McpServer mcp_server(const Value& v, const Path& path) {
        bool enabled = true;
        std::optional<std::string> command, url;
        std::vector<std::string> args;
        std::map<std::string, std::string> env, headers;
        for (const auto& [k, e] : table(v, path, "struct McpServerRepr")) {
            Path p = child(path, k);
            if (k == "enabled") enabled = boolean(e, p);
            else if (k == "command") command = string(e, p);
            else if (k == "args") args = strings(e, p);
            else if (k == "env") env = string_map(e, p);
            else if (k == "url") url = string(e, p);
            else if (k == "headers") headers = string_map(e, p);
            else ignore(path, k);
        }
        McpServer out;
        out.enabled = enabled;
        if (command && !url && headers.empty())
            out.transport = loa::contracts::McpStdio{std::move(*command), std::move(args), std::move(env)};
        else if (!command && url && args.empty() && env.empty())
            out.transport = loa::contracts::McpHttp{std::move(*url), std::move(headers)};
        else if (command && url) fail(path, "set `command` or `url`, not both");
        else if (!command && !url) fail(path, "set `command` (stdio) or `url` (HTTP)");
        else fail(path, "`args` and `env` only apply to `command` servers, `headers` only to `url` ones");
        return out;
    }

    RemoteHost remote_host(const Value& v, const Path& path) {
        RemoteHost out;
        bool host = false;
        for (const auto& [k, e] : table(v, path, "struct RemoteHost")) {
            Path p = child(path, k);
            if (k == "host") out.host = string(e, p), host = true;
            else if (k == "user") out.user = string(e, p);
            else if (k == "port") out.port = uint<std::uint16_t>(e, p, "u16");
            else if (k == "proxy_jump") out.proxy_jump = string(e, p);
            else if (k == "allowed_paths") out.allowed_paths = strings(e, p);
            else if (k == "read_only") out.read_only = boolean(e, p);
            else if (k == "commands") out.commands = boolean(e, p);
            else if (k == "auto_approve") out.auto_approve = boolean(e, p);
            else ignore(path, k);
        }
        if (!host) missing(path, "host");
        return out;
    }

    ProjectSettings project(const Value& v, const Path& path) {
        ProjectSettings out;
        for (const auto& [k, e] : table(v, path, "struct ProjectSettings")) {
            Path p = child(path, k);
            if (k == "instructions") out.instructions = string(e, p);
            else if (k == "pinned_files") {
                const Array& a = array(e, p);
                for (std::size_t i = 0; i < a.size(); ++i)
                    out.pinned_files.push_back(path_value(a[i], child(p, "[" + std::to_string(i) + "]")));
            } else if (k == "digest_role") out.digest_role = string(e, p);
            else if (k == "token_budget") out.token_budget = uint<std::uint32_t>(e, p, "u32");
            else if (k == "disabled_tools") out.disabled_tools = string_set(e, p);
            else if (k == "disabled_mcp_tools") {
                for (const auto& [server, tools] : table(e, p, "a map"))
                    out.disabled_mcp_tools.emplace(server, string_set(tools, child(p, server)));
            } else ignore(path, k);
        }
        return out;
    }

    template <typename T, typename F>
    std::map<std::string, T> map_of(const Value& v, const Path& path, F each) {
        std::map<std::string, T> out;
        for (const auto& [k, e] : table(v, path, "a map")) out.emplace(k, (this->*each)(e, child(path, k)));
        return out;
    }

    Config config(const Table& doc) {
        Config out;
        const Path root;
        for (const auto& [k, e] : doc) {
            Path p = child(root, k);
            if (k == "theme") out.appearance = appearance(e, p);
            else if (k == "pinned") out.pinned = string_set(e, p);
            else if (k == "pinned_projects") out.pinned_projects = string_set(e, p);
            else if (k == "active_project") out.active_project = active_project(e, p);
            else if (k == "project_tabs") out.project_tabs = boolean(e, p);
            else if (k == "harness") out.harness = harness(e, p);
            else if (k == "network") out.network = network(e, p);
            else if (k == "updater") out.updater = updater(e, p);
            else if (k == "unattended") out.unattended = unattended(e, p);
            else if (k == "surfaces") out.surfaces = surfaces(e, p);
            else if (k == "mcp_servers") out.mcp_servers = map_of<McpServer>(e, p, &Decoder::mcp_server);
            else if (k == "remote_hosts") out.remote_hosts = map_of<RemoteHost>(e, p, &Decoder::remote_host);
            else if (k == "projects") out.projects = map_of<ProjectSettings>(e, p, &Decoder::project);
            else ignore(root, k);
        }
        return out;
    }
};

const Value* lookup(const Table& doc, const Path& path) {
    const Table* t = &doc;
    for (std::size_t i = 0; i + 1 < path.size(); ++i) {
        auto it = t->find(path[i]);
        if (it == t->end() || !(t = it->second.as_table())) return nullptr;
    }
    auto it = t->find(path.back());
    return it == t->end() ? nullptr : &it->second;
}

// Puts an unknown key back where it was. When its parent no longer exists (the user
// deleted that server or project), the key goes with it.
void restore(Table& doc, const UnknownKey& key) {
    if (key.path.empty()) return;
    Table* t = &doc;
    for (std::size_t i = 0; i + 1 < key.path.size(); ++i) {
        auto it = t->find(key.path[i]);
        if (it == t->end() || !(t = it->second.as_table())) return;
    }
    t->try_emplace(key.path.back(), key.value);
}

Array string_array(const auto& list) {
    Array out;
    for (const auto& s : list) out.emplace_back(std::string(s));
    return out;
}

Table string_table(const std::map<std::string, std::string>& map) {
    Table out;
    for (const auto& [k, v] : map) out.emplace(k, v);
    return out;
}

Value decision(const DecisionDefault& d) {
    Table t{{"timeout_secs", Value(std::int64_t(d.timeout_secs))}, {"answer", Value(d.answer)}};
    if (d.reason) t.emplace("reason", *d.reason);
    return t;
}

Value mcp_server(const McpServer& s) {
    Table t{{"enabled", Value(s.enabled)}};
    if (auto* stdio = std::get_if<loa::contracts::McpStdio>(&s.transport)) {
        t.emplace("command", stdio->command);
        if (!stdio->args.empty()) t.emplace("args", string_array(stdio->args));
        if (!stdio->env.empty()) t.emplace("env", string_table(stdio->env));
    } else {
        const auto& http = std::get<loa::contracts::McpHttp>(s.transport);
        t.emplace("url", http.url);
        if (!http.headers.empty()) t.emplace("headers", string_table(http.headers));
    }
    return t;
}

Value remote_host(const RemoteHost& h) {
    Table t{{"host", Value(h.host)},
            {"allowed_paths", string_array(h.allowed_paths)},
            {"read_only", Value(h.read_only)},
            {"commands", Value(h.commands)},
            {"auto_approve", Value(h.auto_approve)}};
    if (h.user) t.emplace("user", *h.user);
    if (h.port) t.emplace("port", std::int64_t{*h.port});
    if (h.proxy_jump) t.emplace("proxy_jump", *h.proxy_jump);
    return t;
}

Value project(const ProjectSettings& p) {
    Array pinned;
    for (const auto& f : p.pinned_files) pinned.emplace_back(f.string());
    Table mcp;
    for (const auto& [server, tools] : p.disabled_mcp_tools) mcp.emplace(server, string_array(tools));
    return Table{{"instructions", Value(p.instructions)},
                 {"pinned_files", std::move(pinned)},
                 {"digest_role", Value(p.digest_role)},
                 {"token_budget", Value(std::int64_t{p.token_budget})},
                 {"disabled_tools", string_array(p.disabled_tools)},
                 {"disabled_mcp_tools", std::move(mcp)}};
}

template <typename T, typename F>
Table table_of(const std::map<std::string, T>& map, F f) {
    Table out;
    for (const auto& [k, v] : map) out.emplace(k, f(v));
    return out;
}

Table to_table(const Config& c) {
    Table doc;
    if (c.appearance.overrides.empty()) {
        doc.emplace("theme", c.appearance.theme.name);
    } else {
        Table theme = c.appearance.overrides;
        theme.insert_or_assign("name", c.appearance.theme.name);
        doc.emplace("theme", std::move(theme));
    }
    if (!c.pinned.empty()) doc.emplace("pinned", string_array(c.pinned));
    if (!c.pinned_projects.empty()) doc.emplace("pinned_projects", string_array(c.pinned_projects));
    if (c.active_project)
        doc.emplace("active_project",
                    Table{{"name", Value(c.active_project->name)}, {"cwd", Value(c.active_project->cwd.string())}});
    if (c.project_tabs) doc.emplace("project_tabs", true);

    Table harness{{"default", Value(c.harness.default_)}};
    Table exes;
    for (const auto& [k, p] : c.harness.executables) exes.emplace(k, p.string());
    harness.emplace("executables", std::move(exes));
    if (c.harness.approval_mode) harness.emplace("approval_mode", *c.harness.approval_mode);
    doc.emplace("harness", std::move(harness));

    doc.emplace("network", Table{{"port", Value(std::int64_t{c.network.port})},
                                 {"relay", Value(c.network.relay.to_string())},
                                 {"discovery", Table{{"mdns", Value(c.network.discovery.mdns)},
                                                     {"tailscale", Value(c.network.discovery.tailscale)}}}});

    Table updater{{"check_interval_secs", Value(std::int64_t(c.updater.check_interval_secs))},
                  {"pins", string_table(c.updater.pins)}};
    if (c.updater.github_token) updater.emplace("github_token", *c.updater.github_token);
    if (!c.updater.trusted_keys.empty()) updater.emplace("trusted_keys", string_array(c.updater.trusted_keys));
    doc.emplace("updater", std::move(updater));

    Table unattended;
    if (c.unattended.approval) unattended.emplace("approval", decision(*c.unattended.approval));
    if (c.unattended.question) unattended.emplace("question", decision(*c.unattended.question));
    if (c.unattended.plan) unattended.emplace("plan", decision(*c.unattended.plan));
    if (c.unattended.form) unattended.emplace("form", decision(*c.unattended.form));
    doc.emplace("unattended", std::move(unattended));

    doc.emplace("surfaces", Table{{"video", Value(c.surfaces.video)},
                                  {"browser_capture", Value(std::string(to_string(c.surfaces.browser_capture)))}});
    doc.emplace("mcp_servers", table_of(c.mcp_servers, mcp_server));
    doc.emplace("remote_hosts", table_of(c.remote_hosts, remote_host));
    doc.emplace("projects", table_of(c.projects, project));
    return doc;
}

} // namespace

Result<toml::Table> parse(std::string_view text) {
    auto doc = toml::parse(text);
    if (!doc) {
        ConfigError e;
        e.kind = ConfigError::Kind::Parse;
        e.message = std::move(doc.error());
        return std::unexpected(std::move(e));
    }
    return std::move(*doc);
}

Result<std::uint32_t> version_of(const toml::Table& doc) {
    auto it = doc.find("version");
    if (it == doc.end()) {
        ConfigError e;
        e.kind = ConfigError::Kind::MissingVersion;
        return std::unexpected(std::move(e));
    }
    const auto* v = it->second.as_integer();
    if (!v || *v < 0 || *v > std::numeric_limits<std::uint32_t>::max())
        return std::unexpected(ConfigError::invalid(
            "version", "must be a non-negative integer, found " + toml::write_value(it->second)));
    return static_cast<std::uint32_t>(*v);
}

Result<Config> decode(toml::Table doc) {
    doc.erase("version");
    Decoder decoder;
    Config config;
    try {
        config = decoder.config(doc);
    } catch (DecodeError& e) {
        return std::unexpected(ConfigError::invalid(join(e.path), std::move(e.message)));
    }
    for (auto& path : decoder.ignored) {
        if (const Value* v = lookup(doc, path)) config.unknown.push_back(UnknownKey{std::move(path), *v});
    }
    if (auto valid = config.validate(); !valid) return std::unexpected(std::move(valid.error()));
    return config;
}

Result<std::string> encode(const Config& config) {
    Table doc = to_table(config);
    doc.insert_or_assign("version", Value(std::int64_t{CURRENT_VERSION}));
    for (const auto& key : config.unknown) restore(doc, key);
    return toml::write(doc);
}

} // namespace loa::settings::app
