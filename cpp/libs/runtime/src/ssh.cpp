#include "loa/runtime/ssh.hpp"

#include <algorithm>
#include <charconv>
#include <fstream>
#include <iterator>
#include <system_error>

#include "loa/runtime/exec.hpp"
#include "loa/runtime/fs.hpp"
#include "utf8.hpp"

namespace loa::runtime::ssh {

namespace stdfs = std::filesystem;

namespace {

bool ws_at(std::string_view s, std::size_t i, std::size_t& width) { return detail::is_whitespace(detail::decode(s, i, width)); }

std::vector<std::string_view> split_whitespace(std::string_view s) {
    std::vector<std::string_view> out;
    std::size_t i = 0, start = std::string_view::npos;
    while (i < s.size()) {
        std::size_t w = 0;
        if (ws_at(s, i, w)) {
            if (start != std::string_view::npos) out.push_back(s.substr(start, i - start));
            start = std::string_view::npos;
        } else if (start == std::string_view::npos) {
            start = i;
        }
        i += w;
    }
    if (start != std::string_view::npos) out.push_back(s.substr(start));
    return out;
}

std::string lower(std::string_view s) {
    std::string out(s);
    std::ranges::transform(out, out.begin(), [](unsigned char c) { return static_cast<char>(c >= 'A' && c <= 'Z' ? c + 32 : c); });
    return out;
}

// Expands `*` and `?` in any component of `pattern`, sorted; matches nothing when nothing exists.
std::vector<stdfs::path> glob(const stdfs::path& pattern) {
    std::vector<stdfs::path> current{stdfs::path()};
    for (const auto& component : pattern) {
        const auto part = component.string();
        if (part.empty()) continue;
        if (part.find_first_of("*?") == std::string::npos) {
            for (auto& p : current) p /= component;
            continue;
        }
        std::vector<stdfs::path> next;
        for (const auto& dir : current) {
            std::vector<std::string> names;
            std::error_code ec;
            for (stdfs::directory_iterator it(dir, ec), end; !ec && it != end; it.increment(ec)) {
                auto name = it->path().filename().string();
                if (!name.starts_with('.') && wildcard_match(part, name)) names.push_back(std::move(name));
            }
            std::ranges::sort(names);
            for (const auto& n : names) next.push_back(dir / n);
        }
        current = std::move(next);
    }
    std::vector<stdfs::path> out;
    for (auto& p : current) {
        std::error_code ec;
        if (stdfs::is_regular_file(p, ec)) out.push_back(std::move(p));
    }
    return out;
}

void scan(const stdfs::path& file, const stdfs::path& base, std::size_t depth, std::vector<std::string>& out) {
    std::ifstream in(file, std::ios::binary);
    if (!in) return;
    const std::string text{std::istreambuf_iterator<char>(in), {}};
    if (in.bad()) return;
    for (auto raw : detail::lines(text)) {
        const auto line = detail::trim(raw);
        if (line.empty() || line.starts_with('#')) continue;
        std::size_t cut = 0, width = 1;
        while (cut < line.size()) {
            if (line[cut] == '=' || ws_at(line, cut, width)) break;
            ++cut;
        }
        const auto key = line.substr(0, cut);
        auto rest = cut < line.size() ? line.substr(cut + (line[cut] == '=' ? 1 : width)) : std::string_view();
        while (!rest.empty()) {
            std::size_t w = 1;
            if (rest.front() != '=' && !ws_at(rest, 0, w)) break;
            rest.remove_prefix(rest.front() == '=' ? 1 : w);
        }
        std::vector<std::string_view> words;
        for (auto w : split_whitespace(rest)) {
            while (w.starts_with('"')) w.remove_prefix(1);
            while (w.ends_with('"')) w.remove_suffix(1);
            words.push_back(w);
        }
        const auto k = lower(key);
        if (k == "host") {
            for (const auto w : words) {
                if (w.starts_with('!') || w.find_first_of("*?") != std::string_view::npos) continue;
                auto alias = lower(w);
                if (std::ranges::find(out, alias) == out.end()) out.push_back(std::move(alias));
            }
        } else if (k == "include" && depth < 16) {
            for (const auto pattern : words) {
                const auto path = pattern.starts_with("~/") ? fs::home().value_or(stdfs::path()) / pattern.substr(2) : base / pattern;
                for (const auto& p : glob(path)) scan(p, base, depth + 1, out);
            }
        }
    }
}

} // namespace

std::vector<std::string> aliases(const stdfs::path& config) {
    std::vector<std::string> out;
    scan(config, config.has_parent_path() ? config.parent_path() : stdfs::path("."), 0, out);
    return out;
}

bool wildcard_match(std::string_view p, std::string_view t) noexcept {
    std::size_t pi = 0, ti = 0;
    bool starred = false;
    std::size_t sp = 0, st = 0;
    while (ti < t.size()) {
        if (pi < p.size() && (p[pi] == '?' || p[pi] == t[ti])) {
            ++pi;
            ++ti;
        } else if (pi < p.size() && p[pi] == '*') {
            starred = true;
            sp = pi;
            st = ti;
            ++pi;
        } else if (starred) {
            pi = sp + 1;
            ti = ++st;
        } else {
            return false;
        }
    }
    return std::all_of(p.begin() + static_cast<std::ptrdiff_t>(pi), p.end(), [](char c) { return c == '*'; });
}

std::expected<ResolvedHost, Error> resolve(std::string_view alias) { return resolve_with("ssh", {}, alias); }

std::expected<ResolvedHost, Error> resolve_with(const stdfs::path& program, const std::vector<std::string>& args, std::string_view host) {
    exec::Command cmd{program, {"-G"}, {}, {}};
    cmd.args.insert(cmd.args.end(), args.begin(), args.end());
    cmd.args.emplace_back("--");
    cmd.args.emplace_back(host);
    auto out = exec::output(cmd);
    if (!out && out.error().code() == ErrorCode::not_found)
        return std::unexpected(Error(ErrorCode::not_found, "no OpenSSH client found (`" + program.string() +
                                                               "` is not installed or not on PATH); install OpenSSH to use Remote hosts"));
    if (!out) return std::unexpected(out.error());
    if (!out->status.success())
        return std::unexpected(Error(ErrorCode::io, "ssh -G " + std::string(host) + ": " + std::string(detail::trim(out->err))));
    auto resolved = parse(out->out);
    if (resolved.hostname.empty() || resolved.port == 0)
        return std::unexpected(Error(ErrorCode::io, "ssh -G " + std::string(host) + " printed no hostname or port"));
    return resolved;
}

ResolvedHost parse(std::string_view text) {
    ResolvedHost out;
    for (auto raw : detail::lines(text)) {
        const auto line = detail::trim(raw);
        const auto sp = line.find(' ');
        if (sp == std::string_view::npos) continue;
        const auto key = line.substr(0, sp);
        const auto value = line.substr(sp + 1);
        const auto set = lower(value) == "none" ? std::nullopt : std::optional<std::string>(value);
        if (key == "hostname") out.hostname = value;
        else if (key == "user") out.user = value;
        else if (key == "port") {
            std::uint16_t port = 0;
            auto [end, ec] = std::from_chars(value.data(), value.data() + value.size(), port);
            out.port = ec == std::errc() && end == value.data() + value.size() ? port : 0;
        } else if (key == "proxyjump") out.proxy_jump = set;
        else if (key == "identityagent") out.identity_agent = set;
    }
    return out;
}

} // namespace loa::runtime::ssh
