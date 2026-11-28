// The add-server, add-host and host limits forms. Port of `crates/loa-ui-panel/src/tools/form.rs`.

#include <algorithm>
#include <array>

#include "loa/ui/panel/tools.hpp"

namespace loa::ui::panel::tools {

using kit::widgets::Field;

namespace {

constexpr Field NAME{"name", "Name", "my-server"};
constexpr Field COMMAND{"command", "Command", "npx -y @scope/server"};
constexpr Field URL{"url", "URL", "https://host/mcp"};
constexpr std::array<Field, 2> STDIO_FIELDS{NAME, COMMAND};
constexpr std::array<Field, 2> HTTP_FIELDS{NAME, URL};
constexpr std::array<Field, 3> SERVER_FIELDS{NAME, COMMAND, URL};
constexpr std::array<Field, 4> HOST_FIELDS{Field{"alias", "Alias", "build"}, Field{"hostname", "Hostname", "build.internal"},
                                           Field{"user", "User", "optional"}, Field{"port", "Port", "optional"}};

bool is_space(char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v'; }
bool has_space(std::string_view s) { return std::ranges::any_of(s, is_space); }
bool is_alpha(char c) { return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z'); }
bool is_alnum(char c) { return is_alpha(c) || (c >= '0' && c <= '9'); }

std::string_view trim(std::string_view s) {
    const auto b = std::ranges::find_if_not(s, is_space);
    if (b == s.end()) return {};
    auto e = s.end();
    while (is_space(*(e - 1))) --e;
    return {b, e};
}

std::expected<std::uint16_t, std::string_view> parse_port(std::string_view port) {
    std::string_view digits = port.starts_with('+') ? port.substr(1) : port;
    if (digits.empty() || digits.size() > 5 || !std::ranges::all_of(digits, [](char c) { return c >= '0' && c <= '9'; }))
        return std::unexpected(BAD_PORT);
    unsigned v = 0;
    for (char c : digits) v = v * 10 + static_cast<unsigned>(c - '0');
    if (v == 0 || v > 65535) return std::unexpected(BAD_PORT);
    return static_cast<std::uint16_t>(v);
}

// A shell-split command line: leading `KEY=value` words become the environment.
std::expected<McpTransport, std::string_view> parse_stdio(std::string_view line) {
    auto words = shell_split(line);
    if (!words) return std::unexpected(UNBALANCED);
    auto is_env = [](std::string_view k) {
        return !k.empty() && (is_alpha(k[0]) || k[0] == '_') && std::ranges::all_of(k, [](char c) { return is_alnum(c) || c == '_'; });
    };
    std::map<std::string, std::string> env;
    std::size_t i = 0;
    for (; i < words->size(); ++i) {
        const std::string& w = (*words)[i];
        const auto eq = w.find('=');
        if (eq == std::string::npos || !is_env(std::string_view(w).substr(0, eq))) break;
        env[w.substr(0, eq)] = w.substr(eq + 1);
    }
    if (i == words->size()) return std::unexpected(env.empty() ? EMPTY : NO_COMMAND);
    loa::contracts::McpStdio s{(*words)[i], {words->begin() + static_cast<std::ptrdiff_t>(i) + 1, words->end()}, std::move(env)};
    return McpTransport{std::move(s)};
}

// An HTTP(S) server URL.
std::expected<std::string_view, std::string_view> validate_url(std::string_view url) {
    if (url.empty()) return std::unexpected(EMPTY);
    if (has_space(url)) return std::unexpected(WHITESPACE);
    std::string lower(url.substr(0, std::min<std::size_t>(8, url.size())));
    for (auto& c : lower)
        if (c >= 'A' && c <= 'Z') c = static_cast<char>(c - 'A' + 'a');
    std::string_view rest;
    if (lower.starts_with("https://")) rest = url.substr(8);
    else if (lower.starts_with("http://")) rest = url.substr(7);
    else return std::unexpected(BAD_SCHEME);
    const std::string_view authority = rest.substr(0, std::min(rest.find_first_of("/?#"), rest.size()));
    const auto at = authority.rfind('@');
    const std::string_view host_port = at == std::string_view::npos ? authority : authority.substr(at + 1);
    std::string_view host;
    std::optional<std::string_view> port;
    if (host_port.starts_with('[')) {
        const auto v6 = host_port.substr(1);
        const auto close = v6.find(']');
        if (close == std::string_view::npos) return std::unexpected(NO_HOST);
        host = v6.substr(0, close);
        if (const auto after = v6.substr(close + 1); after.starts_with(':')) port = after.substr(1);
    } else if (const auto colon = host_port.find(':'); colon != std::string_view::npos) {
        host = host_port.substr(0, colon);
        port = host_port.substr(colon + 1);
    } else {
        host = host_port;
    }
    if (host.empty()) return std::unexpected(NO_HOST);
    if (port)
        if (auto p = parse_port(*port); !p) return std::unexpected(p.error());
    return url;
}

} // namespace

std::optional<std::vector<std::string>> shell_split(std::string_view line) {
    enum class S { Delimiter, Backslash, Unquoted, UnquotedBackslash, Single, Double, DoubleBackslash, Comment };
    std::vector<std::string> words;
    std::string word;
    S s = S::Delimiter;
    auto finish = [&] { words.push_back(std::exchange(word, {})); };
    const auto blank = [](char c) { return c == ' ' || c == '\t' || c == '\n'; };
    for (std::size_t i = 0;; ++i) {
        const bool end = i == line.size();
        const char c = end ? '\0' : line[i];
        switch (s) {
        case S::Delimiter:
            if (end) return words;
            if (c == '\'') s = S::Single;
            else if (c == '"') s = S::Double;
            else if (c == '\\') s = S::Backslash;
            else if (blank(c)) s = S::Delimiter;
            else if (c == '#') s = S::Comment;
            else { word.push_back(c); s = S::Unquoted; }
            break;
        case S::Backslash:
        case S::UnquotedBackslash:
            if (end) { word.push_back('\\'); finish(); return words; }
            if (c == '\n') s = s == S::Backslash ? S::Delimiter : S::Unquoted;
            else { word.push_back(c); s = S::Unquoted; }
            break;
        case S::Unquoted:
            if (end) { finish(); return words; }
            if (c == '\'') s = S::Single;
            else if (c == '"') s = S::Double;
            else if (c == '\\') s = S::UnquotedBackslash;
            else if (blank(c)) { finish(); s = S::Delimiter; }
            else word.push_back(c);
            break;
        case S::Single:
            if (end) return std::nullopt;
            if (c == '\'') s = S::Unquoted;
            else word.push_back(c);
            break;
        case S::Double:
            if (end) return std::nullopt;
            if (c == '"') s = S::Unquoted;
            else if (c == '\\') s = S::DoubleBackslash;
            else word.push_back(c);
            break;
        case S::DoubleBackslash:
            if (end) return std::nullopt;
            if (c == '$' || c == '`' || c == '"' || c == '\\') word.push_back(c);
            else if (c != '\n') { word.push_back('\\'); word.push_back(c); }
            s = S::Double;
            break;
        case S::Comment:
            if (end) return words;
            if (c == '\n') s = S::Delimiter;
            break;
        }
    }
}

std::string_view Values::get(std::string_view key) const {
    const auto it = values_.find(key);
    return it == values_.end() ? std::string_view{} : trim(it->second);
}

std::string Values::raw(std::string_view key) const {
    const auto it = values_.find(key);
    return it == values_.end() ? std::string{} : it->second;
}

std::expected<void, std::string> Values::set(std::span<const Field> fields, std::string_view key, std::string_view value) {
    const auto f = std::ranges::find(fields, key, &Field::key);
    if (f == fields.end()) return std::unexpected("no field " + std::string(key));
    values_[f->key] = std::string(value);
    return {};
}

std::span<const Field> AddServerForm::fields() const {
    using KField = kit::widgets::Field;
    return kind == TransportKind::Stdio ? std::span<const KField>(STDIO_FIELDS) : std::span<const KField>(HTTP_FIELDS);
}

std::expected<void, std::string> AddServerForm::set(std::string_view key, std::string_view value) {
    return values.set(SERVER_FIELDS, key, value);
}

std::expected<McpServerSpec, Errors> AddServerForm::validate(std::span<const std::string> existing) const {
    const auto name = values.get("name");
    std::optional<std::string_view> name_error;
    if (name.empty()) name_error = EMPTY;
    else if (!std::ranges::all_of(name, [](char c) { return is_alnum(c) || c == '-' || c == '_'; })) name_error = BAD_NAME;
    else if (std::ranges::find(existing, name) != existing.end()) name_error = DUPLICATE_SERVER;
    std::expected<McpTransport, std::pair<FieldId, std::string_view>> transport = std::unexpected(std::pair{FieldId{}, std::string_view{}});
    if (kind == TransportKind::Stdio) {
        auto t = parse_stdio(values.get("command"));
        transport = t ? decltype(transport)(std::move(*t)) : std::unexpected(std::pair{FieldId{"command"}, t.error()});
    } else {
        auto u = validate_url(values.get("url"));
        transport = u ? decltype(transport)(McpTransport{loa::contracts::McpHttp{std::string(*u), {}}})
                      : std::unexpected(std::pair{FieldId{"url"}, u.error()});
    }
    if (!name_error && transport) return McpServerSpec{std::string(name), std::move(*transport)};
    Errors errors;
    if (name_error) errors.emplace("name", *name_error);
    if (!transport) errors.emplace(transport.error().first, transport.error().second);
    return std::unexpected(std::move(errors));
}

std::span<const Field> AddHostForm::fields() const { return HOST_FIELDS; }

std::expected<void, std::string> AddHostForm::set(std::string_view key, std::string_view value) {
    return values.set(HOST_FIELDS, key, value);
}

std::expected<RemoteHost, Errors> AddHostForm::validate(std::span<const std::string> existing) const {
    const auto alias = values.get("alias"), hostname = values.get("hostname"), user = values.get("user"), port = values.get("port");
    auto word = [](std::string_view v) -> std::optional<std::string_view> {
        if (v.empty()) return EMPTY;
        if (has_space(v)) return WHITESPACE;
        return std::nullopt;
    };
    std::optional<std::uint16_t> port_value;
    std::optional<std::string_view> port_error;
    if (!port.empty()) {
        if (auto p = parse_port(port)) port_value = *p;
        else port_error = p.error();
    }
    auto alias_error = word(alias);
    if (!alias_error && std::ranges::find(existing, alias) != existing.end()) alias_error = DUPLICATE_HOST;
    Errors errors;
    for (auto [k, e] : {std::pair{FieldId{"alias"}, alias_error}, std::pair{FieldId{"hostname"}, word(hostname)},
                        std::pair{FieldId{"user"}, user.empty() ? std::nullopt : word(user)}, std::pair{FieldId{"port"}, port_error}})
        if (e) errors.emplace(k, *e);
    if (!errors.empty()) return std::unexpected(std::move(errors));
    HostLimits limits;
    limits.read_only = true;
    return RemoteHost{std::string(alias), std::string(hostname), user.empty() ? std::nullopt : std::optional(std::string(user)),
                      port_value, std::nullopt, std::move(limits)};
}

void LimitsDraft::add_path() {
    const std::string path(trim(new_path));
    if (path.empty()) path_error = EMPTY;
    else if (!(path.starts_with('/') || path.starts_with('~'))) path_error = RELATIVE_PATH;
    else if (std::ranges::find(limits.allowed_paths, path) != limits.allowed_paths.end()) path_error = DUPLICATE_PATH;
    else {
        limits.allowed_paths.push_back(path);
        new_path.clear();
        path_error = std::nullopt;
    }
}

} // namespace loa::ui::panel::tools
