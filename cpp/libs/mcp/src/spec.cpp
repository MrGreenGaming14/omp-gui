#include "loa/mcp/spec.hpp"

#include <curl/curl.h>

#include <algorithm>
#include <array>
#include <cctype>
#include <memory>

namespace loa::mcp {

namespace json = loa::runtime::json;

namespace {

// Headers the streamable HTTP client sets itself.
constexpr std::array<std::string_view, 4> RESERVED_HEADERS = {"accept", "mcp-session-id", "last-event-id", "mcp-protocol-version"};

// `shlex::split`: POSIX shell words with `'…'`, `"…"` (backslash escapes `$`, backquote, `"`, `\`
// and newline), backslash escapes outside quotes, and `#` comments at word starts. None on an
// unclosed quote or a trailing backslash.
std::optional<std::vector<std::string>> shell_split(std::string_view in) {
    std::vector<std::string> words;
    std::size_t i = 0;
    const auto n = in.size();
    auto blank = [](char c) { return c == ' ' || c == '\t' || c == '\n'; };
    while (i < n) {
        while (i < n && blank(in[i])) ++i;
        if (i >= n) break;
        if (in[i] == '#') {
            while (i < n && in[i] != '\n') ++i;
            continue;
        }
        std::string word;
        while (i < n && !blank(in[i])) {
            const char c = in[i++];
            if (c == '\\') {
                if (i >= n) return std::nullopt;
                const char next = in[i++];
                if (next != '\n') word.push_back(next);
            } else if (c == '\'') {
                const auto end = in.find('\'', i);
                if (end == std::string_view::npos) return std::nullopt;
                word.append(in.substr(i, end - i));
                i = end + 1;
            } else if (c == '"') {
                for (;;) {
                    if (i >= n) return std::nullopt;
                    const char q = in[i++];
                    if (q == '"') break;
                    if (q == '\\') {
                        if (i >= n) return std::nullopt;
                        const char e = in[i++];
                        if (e == '$' || e == '`' || e == '"' || e == '\\') word.push_back(e);
                        else if (e != '\n') {
                            word.push_back('\\');
                            word.push_back(e);
                        }
                    } else {
                        word.push_back(q);
                    }
                }
            } else {
                word.push_back(c);
            }
        }
        words.push_back(std::move(word));
    }
    return words;
}

// `shlex::try_quote`: the word as is when it needs no quoting, else single quoted.
std::optional<std::string> shell_quote(std::string_view word) {
    if (word.find('\0') != std::string_view::npos) return std::nullopt;
    if (word.empty()) return "''";
    const bool safe = std::ranges::all_of(word, [](unsigned char c) {
        return std::isalnum(c) || std::string_view("+-./:@]_,=%^").find(static_cast<char>(c)) != std::string_view::npos;
    });
    if (safe) return std::string(word);
    std::string out = "'";
    for (char c : word) {
        if (c == '\'') out += "'\\''";
        else out.push_back(c);
    }
    out.push_back('\'');
    return out;
}

// `NAME=value` with a shell variable name.
std::optional<std::pair<std::string, std::string>> env_assignment(std::string_view word) {
    const auto eq = word.find('=');
    if (eq == std::string_view::npos || eq == 0) return std::nullopt;
    const auto name = word.substr(0, eq);
    const unsigned char first = static_cast<unsigned char>(name[0]);
    if (!(std::isalpha(first) || first == '_')) return std::nullopt;
    if (!std::ranges::all_of(name, [](unsigned char c) { return std::isalnum(c) || c == '_'; })) return std::nullopt;
    return std::pair{std::string(name), std::string(word.substr(eq + 1))};
}

bool token_char(unsigned char c) {
    return std::isalnum(c) || std::string_view("!#$%&'*+-.^_`|~").find(static_cast<char>(c)) != std::string_view::npos;
}

// `http::HeaderName::from_bytes`: a non-empty token, compared lowercase.
std::optional<std::string> header_name(std::string_view name) {
    if (name.empty() || !std::ranges::all_of(name, [](char c) { return token_char(static_cast<unsigned char>(c)); }))
        return std::nullopt;
    std::string lower(name);
    std::ranges::transform(lower, lower.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return lower;
}

// `http::HeaderValue::from_str`: visible bytes, spaces and tabs.
bool header_value(std::string_view value) {
    return std::ranges::all_of(value, [](char ch) {
        const auto c = static_cast<unsigned char>(ch);
        return c == '\t' || (c >= 32 && c != 127);
    });
}

std::string trim(std::string_view s) {
    const auto first = s.find_first_not_of(" \t\r\n\f\v");
    if (first == std::string_view::npos) return {};
    const auto last = s.find_last_not_of(" \t\r\n\f\v");
    return std::string(s.substr(first, last - first + 1));
}

struct UrlDeleter {
    void operator()(CURLU* url) const noexcept { curl_url_cleanup(url); }
};
using UrlHandle = std::unique_ptr<CURLU, UrlDeleter>;

struct ParsedUrl {
    std::string scheme;
    std::string host;
    std::string normalized;
};

std::string url_part(CURLU* url, CURLUPart part, unsigned flags = 0) {
    char* text = nullptr;
    if (curl_url_get(url, part, &text, flags) != CURLUE_OK || text == nullptr) return {};
    std::string out(text);
    curl_free(text);
    return out;
}

std::expected<ParsedUrl, std::string> parse_url(std::string_view text) {
    UrlHandle url(curl_url());
    if (!url) return std::unexpected(std::string("out of memory"));
    const std::string owned(text);
    if (const auto rc = curl_url_set(url.get(), CURLUPART_URL, owned.c_str(), CURLU_NON_SUPPORT_SCHEME); rc != CURLUE_OK)
        return std::unexpected(std::string(curl_url_strerror(rc)));
    ParsedUrl parsed;
    parsed.scheme = url_part(url.get(), CURLUPART_SCHEME);
    parsed.host = url_part(url.get(), CURLUPART_HOST);
    // WHATWG parsing lowercases the host; curl keeps it as written.
    std::string host = parsed.host;
    std::ranges::transform(host, host.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    if (!host.empty()) curl_url_set(url.get(), CURLUPART_HOST, host.c_str(), 0);
    parsed.normalized = url_part(url.get(), CURLUPART_URL, CURLU_NO_DEFAULT_PORT);
    return parsed;
}

} // namespace

Result<> validate_name(std::string_view name) {
    const bool ok = !name.empty() && std::ranges::all_of(name, [](unsigned char c) {
        return (c < 128 && std::isalnum(c)) || c == '-' || c == '_' || c == '.';
    });
    if (ok) return {};
    return std::unexpected(Error::invalid_name(std::string(name)));
}

bool same_url(std::string_view a, std::string_view b) {
    auto pa = parse_url(a);
    auto pb = parse_url(b);
    if (pa && pb) return pa->normalized == pb->normalized;
    return a == b;
}

Result<StdioSpec> StdioSpec::parse(std::string_view line) {
    auto words = shell_split(line);
    if (!words) return std::unexpected(Error::unclosed_quote());
    StdioSpec spec;
    std::size_t i = 0;
    for (; i < words->size(); ++i) {
        auto assignment = env_assignment((*words)[i]);
        if (!assignment) break;
        spec.env.insert_or_assign(std::move(assignment->first), std::move(assignment->second));
    }
    if (i >= words->size()) return std::unexpected(Error::empty_command());
    spec.command = std::move((*words)[i++]);
    spec.args.assign(std::make_move_iterator(words->begin() + static_cast<std::ptrdiff_t>(i)), std::make_move_iterator(words->end()));
    return spec;
}

std::optional<std::string> StdioSpec::command_line() const {
    std::string line;
    for (const auto& [name, value] : env) {
        auto quoted = shell_quote(value);
        if (!quoted) return std::nullopt;
        line += name + "=" + *quoted + " ";
    }
    auto command_word = shell_quote(command);
    if (!command_word) return std::nullopt;
    line += *command_word;
    for (const auto& arg : args) {
        auto quoted = shell_quote(arg);
        if (!quoted) return std::nullopt;
        line += " " + *quoted;
    }
    return line;
}

Result<HttpSpec> HttpSpec::make(std::string_view url, std::map<std::string, std::string> headers) {
    HttpSpec spec{trim(url), std::move(headers)};
    if (auto ok = spec.validate(); !ok) return std::unexpected(std::move(ok.error()));
    return spec;
}

Result<> HttpSpec::validate() const {
    auto parsed = parse_url(url);
    if (!parsed) return std::unexpected(Error::bad_url(url, parsed.error()));
    if (parsed->scheme != "http" && parsed->scheme != "https")
        return std::unexpected(Error::bad_url(url, "the scheme must be http or https"));
    if (parsed->host.empty()) return std::unexpected(Error::bad_url(url, "there is no host"));
    for (const auto& [name, value] : headers) {
        auto lower = header_name(name);
        if (!lower) return std::unexpected(Error::header(name));
        if (std::ranges::find(RESERVED_HEADERS, *lower) != RESERVED_HEADERS.end()) return std::unexpected(Error::reserved_header(name));
        if (!header_value(value)) return std::unexpected(Error::header(name + ": " + value));
    }
    return {};
}

Result<std::pair<std::string, std::string>> parse_header(std::string_view line) {
    const auto colon = line.find(':');
    if (colon == std::string_view::npos) return std::unexpected(Error::header(std::string(line)));
    auto name = trim(line.substr(0, colon));
    auto value = trim(line.substr(colon + 1));
    if (!header_name(name) || !header_value(value)) return std::unexpected(Error::header(std::string(line)));
    return std::pair{std::move(name), std::move(value)};
}

Result<ServerSpec> ServerSpec::make(std::string name, Transport transport) {
    ServerSpec spec{std::move(name), true, std::move(transport)};
    if (auto ok = spec.validate(); !ok) return std::unexpected(std::move(ok.error()));
    return spec;
}

Result<> ServerSpec::validate() const {
    if (auto ok = validate_name(name); !ok) return ok;
    if (const auto* stdio = std::get_if<StdioSpec>(&transport)) {
        if (trim(stdio->command).empty()) return std::unexpected(Error::empty_command());
        return {};
    }
    return std::get<HttpSpec>(transport).validate();
}

bool ServerSpec::same_server(const ServerSpec& other) const {
    const auto* a = std::get_if<StdioSpec>(&transport);
    const auto* b = std::get_if<StdioSpec>(&other.transport);
    if (a && b) return a->command == b->command && a->args == b->args && a->cwd == b->cwd;
    const auto* ha = std::get_if<HttpSpec>(&transport);
    const auto* hb = std::get_if<HttpSpec>(&other.transport);
    if (ha && hb) return same_url(ha->url, hb->url);
    return false;
}

namespace {

json::Value string_map(const std::map<std::string, std::string>& map) {
    json::Object out;
    for (const auto& [k, v] : map) out.emplace_back(k, v);
    return out;
}

std::expected<std::map<std::string, std::string>, std::string> read_map(const json::Value& object, std::string_view key) {
    std::map<std::string, std::string> out;
    const auto* value = object.find(key);
    if (value == nullptr) return out;
    const auto* items = value->as_object();
    if (items == nullptr) return std::unexpected("invalid type for `" + std::string(key) + "`, expected a map");
    for (const auto& [k, v] : *items) {
        const auto* text = v.as_string();
        if (text == nullptr) return std::unexpected("invalid type for `" + std::string(key) + "." + k + "`, expected a string");
        out.insert_or_assign(k, *text);
    }
    return out;
}

std::expected<std::string, std::string> read_string(const json::Value& object, std::string_view key) {
    const auto* value = object.find(key);
    if (value == nullptr) return std::unexpected("missing field `" + std::string(key) + "`");
    const auto* text = value->as_string();
    if (text == nullptr) return std::unexpected("invalid type for `" + std::string(key) + "`, expected a string");
    return *text;
}

} // namespace

json::Value ServerSpec::to_json() const {
    json::Object out;
    out.emplace_back("name", name);
    out.emplace_back("enabled", enabled);
    if (const auto* stdio = std::get_if<StdioSpec>(&transport)) {
        out.emplace_back("type", "stdio");
        out.emplace_back("command", stdio->command);
        if (!stdio->args.empty()) {
            json::Array args(stdio->args.begin(), stdio->args.end());
            out.emplace_back("args", std::move(args));
        }
        if (!stdio->env.empty()) out.emplace_back("env", string_map(stdio->env));
        if (stdio->cwd) out.emplace_back("cwd", stdio->cwd->string());
    } else {
        const auto& http = std::get<HttpSpec>(transport);
        out.emplace_back("type", "http");
        out.emplace_back("url", http.url);
        if (!http.headers.empty()) out.emplace_back("headers", string_map(http.headers));
    }
    return out;
}

std::expected<ServerSpec, std::string> ServerSpec::from_json(const json::Value& value) {
    if (!value.is_object()) return std::unexpected(std::string("invalid type, expected a server spec map"));
    ServerSpec spec;
    auto name = read_string(value, "name");
    if (!name) return std::unexpected(name.error());
    spec.name = std::move(*name);
    if (const auto* enabled = value.find("enabled")) {
        const auto* flag = enabled->as_bool();
        if (flag == nullptr) return std::unexpected(std::string("invalid type for `enabled`, expected a boolean"));
        spec.enabled = *flag;
    }
    auto kind = read_string(value, "type");
    if (!kind) return std::unexpected(kind.error());
    if (*kind == "stdio") {
        StdioSpec stdio;
        auto command = read_string(value, "command");
        if (!command) return std::unexpected(command.error());
        stdio.command = std::move(*command);
        if (const auto* args = value.find("args")) {
            const auto* items = args->as_array();
            if (items == nullptr) return std::unexpected(std::string("invalid type for `args`, expected a sequence"));
            for (const auto& item : *items) {
                const auto* text = item.as_string();
                if (text == nullptr) return std::unexpected(std::string("invalid type for `args`, expected a string"));
                stdio.args.push_back(*text);
            }
        }
        auto env = read_map(value, "env");
        if (!env) return std::unexpected(env.error());
        stdio.env = std::move(*env);
        if (const auto* cwd = value.find("cwd"); cwd != nullptr && !cwd->is_null()) {
            const auto* text = cwd->as_string();
            if (text == nullptr) return std::unexpected(std::string("invalid type for `cwd`, expected a path"));
            stdio.cwd = std::filesystem::path(*text);
        }
        spec.transport = std::move(stdio);
    } else if (*kind == "http") {
        HttpSpec http;
        auto url = read_string(value, "url");
        if (!url) return std::unexpected(url.error());
        http.url = std::move(*url);
        auto headers = read_map(value, "headers");
        if (!headers) return std::unexpected(headers.error());
        http.headers = std::move(*headers);
        spec.transport = std::move(http);
    } else {
        return std::unexpected("unknown variant `" + *kind + "`, expected `stdio` or `http`");
    }
    return spec;
}

} // namespace loa::mcp
