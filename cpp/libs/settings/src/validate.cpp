// The rules the decoder can't express, as one table. A rule's key may hold `*`,
// which its check fills with the map key it failed on.

#include <algorithm>
#include <array>
#include <cstdio>
#include <functional>
#include <optional>
#include <ranges>

#include "loa/runtime/json.hpp"
#include "loa/settings/app/schema.hpp"

namespace loa::settings::app {
namespace {

// What failed: the text for `*` and the reason.
using Failure = std::optional<std::pair<std::string, std::string>>;

Failure when(bool bad, std::string reason) {
    if (bad) return std::pair{std::string(), std::move(reason)};
    return std::nullopt;
}

// The first entry of `map` that `bad` rejects.
template <typename Map, typename Bad>
Failure each(const Map& map, Bad bad) {
    for (const auto& [k, v] : map)
        if (std::optional<std::string> r = bad(v)) return std::pair{std::string(k), std::move(*r)};
    return std::nullopt;
}

bool blank(std::string_view s) {
    return std::ranges::all_of(s, [](unsigned char c) { return c == ' ' || (c >= '\t' && c <= '\r'); });
}

bool not_http(std::string_view s) {
    for (std::string_view scheme : {std::string_view("https://"), std::string_view("http://")})
        if (s.starts_with(scheme) && s.size() > scheme.size()) return false;
    return true;
}

// Rust's `{:?}` of a string.
std::string debug(std::string_view s) {
    std::string out = "\"";
    for (unsigned char c : s) {
        switch (c) {
        case '"': out += "\\\""; break;
        case '\\': out += "\\\\"; break;
        case '\n': out += "\\n"; break;
        case '\r': out += "\\r"; break;
        case '\t': out += "\\t"; break;
        default:
            if (c < 0x20 || c == 0x7f) {
                char buf[12];
                std::snprintf(buf, sizeof buf, "\\u{%x}", unsigned(c));
                out += buf;
            } else {
                out += char(c);
            }
        }
    }
    return out + "\"";
}

constexpr std::string_view PORT = "must be between 1 and 65535";
constexpr std::string_view POSITIVE = "must be at least 1";
constexpr std::string_view EMPTY = "must not be empty";

// How an unattended default answer is checked.
struct AnswerRule {
    enum Kind { OneOf, NonEmpty, JsonObject } kind;
    std::vector<std::string_view> allowed;

    [[nodiscard]] std::optional<std::string> check(const std::string& answer) const {
        switch (kind) {
        case OneOf:
            if (std::ranges::find(allowed, std::string_view(answer)) == allowed.end()) {
                std::string list;
                for (auto a : allowed) list += (list.empty() ? "" : ", ") + std::string(a);
                return "must be one of " + list + ", found " + debug(answer);
            }
            return std::nullopt;
        case NonEmpty:
            if (blank(answer)) return std::string(EMPTY);
            return std::nullopt;
        case JsonObject: {
            auto v = loa::runtime::json::parse(answer);
            if (!v) return "must be a JSON object: " + v.error().message();
            if (!v->is_object()) return std::string("must be a JSON object: invalid type: not a map, expected a map");
            return std::nullopt;
        }
        }
        return std::nullopt;
    }
};

// The configured unattended defaults, by kind, with the rule their answer follows.
std::vector<std::pair<std::string, std::pair<const DecisionDefault*, AnswerRule>>> unattended(const Config& c) {
    namespace a = loa::core::answer;
    const auto& u = c.unattended;
    std::vector<std::pair<std::string, std::pair<const DecisionDefault*, AnswerRule>>> out;
    auto add = [&](const char* kind, const std::optional<DecisionDefault>& d, AnswerRule rule) {
        if (d) out.push_back({kind, {&*d, std::move(rule)}});
    };
    add("approval", u.approval, {AnswerRule::OneOf, {a::ALLOW_ONCE, a::ALLOW_SESSION, a::DENY}});
    add("question", u.question, {AnswerRule::NonEmpty, {}});
    add("plan", u.plan, {AnswerRule::OneOf, {a::APPROVE, a::REJECT}});
    add("form", u.form, {AnswerRule::JsonObject, {}});
    return out;
}

using Pair = std::pair<const DecisionDefault*, AnswerRule>;

struct Rule {
    std::string_view key;
    std::function<Failure(const Config&)> check;
};

const std::vector<Rule>& rules() {
    static const std::vector<Rule> r = {
        {"harness.default", [](const Config& c) { return when(blank(c.harness.default_), "must name a harness"); }},
        {"harness.executables.*",
         [](const Config& c) {
             return each(c.harness.executables, [](const std::filesystem::path& p) -> std::optional<std::string> {
                 if (p.empty()) return std::string(EMPTY);
                 return std::nullopt;
             });
         }},
        {"network.port", [](const Config& c) { return when(c.network.port == 0, std::string(PORT)); }},
        {"network.relay",
         [](const Config& c) -> Failure {
             if (auto* url = std::get_if<Relay::Url>(&c.network.relay.value))
                 return when(not_http(url->url),
                             "must be \"default\", \"off\" or an http(s) URL, found " + debug(url->url));
             return std::nullopt;
         }},
        {"updater.check_interval_secs",
         [](const Config& c) { return when(c.updater.check_interval_secs == 0, std::string(POSITIVE)); }},
        {"updater.pins.*",
         [](const Config& c) {
             return each(c.updater.pins, [](const std::string& v) -> std::optional<std::string> {
                 if (blank(v)) return std::string("must be a version");
                 return std::nullopt;
             });
         }},
        {"unattended.*.timeout_secs",
         [](const Config& c) {
             return each(unattended(c), [](const Pair& p) -> std::optional<std::string> {
                 if (p.first->timeout_secs == 0) return std::string(POSITIVE);
                 return std::nullopt;
             });
         }},
        {"unattended.*.answer",
         [](const Config& c) {
             return each(unattended(c), [](const Pair& p) { return p.second.check(p.first->answer); });
         }},
        {"mcp_servers",
         [](const Config& c) {
             return when(std::ranges::any_of(c.mcp_servers, [](const auto& e) { return blank(e.first); }),
                         "server names must not be empty");
         }},
        {"mcp_servers.*.command",
         [](const Config& c) {
             return each(c.mcp_servers, [](const McpServer& s) -> std::optional<std::string> {
                 if (auto* stdio = std::get_if<loa::contracts::McpStdio>(&s.transport); stdio && blank(stdio->command))
                     return std::string(EMPTY);
                 return std::nullopt;
             });
         }},
        {"mcp_servers.*.url",
         [](const Config& c) {
             return each(c.mcp_servers, [](const McpServer& s) -> std::optional<std::string> {
                 if (auto* http = std::get_if<loa::contracts::McpHttp>(&s.transport); http && not_http(http->url))
                     return "must be an http(s) URL, found " + debug(http->url);
                 return std::nullopt;
             });
         }},
        {"remote_hosts.*.host",
         [](const Config& c) {
             return each(c.remote_hosts, [](const RemoteHost& h) -> std::optional<std::string> {
                 if (blank(h.host)) return std::string(EMPTY);
                 return std::nullopt;
             });
         }},
        {"remote_hosts.*.port",
         [](const Config& c) {
             return each(c.remote_hosts, [](const RemoteHost& h) -> std::optional<std::string> {
                 if (h.port == std::optional<std::uint16_t>(0)) return std::string(PORT);
                 return std::nullopt;
             });
         }},
        {"remote_hosts.*.allowed_paths",
         [](const Config& c) {
             return each(c.remote_hosts, [](const RemoteHost& h) -> std::optional<std::string> {
                 for (const auto& p : h.allowed_paths) {
                     bool dotdot = false;
                     for (auto part : std::views::split(std::string_view(p), '/'))
                         if (std::string_view(part.begin(), part.end()) == "..") dotdot = true;
                     if (!p.starts_with('/') || dotdot)
                         return "must hold absolute paths without `..`, found " + debug(p);
                 }
                 return std::nullopt;
             });
         }},
        {"projects.*.digest_role",
         [](const Config& c) {
             return each(c.projects, [](const ProjectSettings& p) -> std::optional<std::string> {
                 if (blank(p.digest_role)) return std::string("must name a model role");
                 return std::nullopt;
             });
         }},
        {"projects.*.token_budget",
         [](const Config& c) {
             return each(c.projects, [](const ProjectSettings& p) -> std::optional<std::string> {
                 if (p.token_budget == 0) return std::string(POSITIVE);
                 return std::nullopt;
             });
         }},
        {"projects.*.pinned_files",
         [](const Config& c) {
             return each(c.projects, [](const ProjectSettings& p) -> std::optional<std::string> {
                 if (std::ranges::any_of(p.pinned_files, [](const auto& f) { return f.empty(); }))
                     return std::string("must not hold empty paths");
                 return std::nullopt;
             });
         }},
    };
    return r;
}

} // namespace

Result<> Config::validate() const {
    for (const Rule& rule : rules()) {
        if (Failure f = rule.check(*this)) {
            std::string key(rule.key);
            if (auto star = key.find('*'); star != std::string::npos) key.replace(star, 1, f->first);
            return std::unexpected(ConfigError::invalid(std::move(key), std::move(f->second)));
        }
    }
    return {};
}

} // namespace loa::settings::app
