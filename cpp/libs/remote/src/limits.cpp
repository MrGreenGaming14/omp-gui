#include "loa/remote/limits.hpp"

#include <algorithm>

namespace loa::remote {

std::expected<void, std::string> HostLimits::check_action(Action action) const {
    if (read_only && mutates(action)) return std::unexpected("host is read only; " + std::string(to_string(action)) + " refused");
    if (action == Action::command && !commands) return std::unexpected(std::string("commands are not allowed on this host"));
    return {};
}

std::expected<void, std::string> HostLimits::check_path(std::string_view normalized) const {
    if (allowed_paths.empty()) return std::unexpected(std::string(normalized) + " refused: no paths are allowed on this host");
    const auto all = prefixes();
    if (std::ranges::any_of(all, [&](const std::string& p) { return within(normalized, p); })) return {};
    std::string joined;
    for (const auto& p : allowed_paths) {
        if (!joined.empty()) joined += ", ";
        joined += p;
    }
    return std::unexpected(std::string(normalized) + " is outside the allowed paths (" + joined + ")");
}

bool HostLimits::needs_approval(Action action) const noexcept {
    switch (action) {
    case Action::write:
    case Action::edit: return !auto_approve_writes;
    case Action::command: return !auto_approve_commands;
    default: return false;
    }
}

std::vector<std::string> HostLimits::prefixes() const {
    std::vector<std::string> out;
    for (const auto& p : allowed_paths) {
        if (auto n = normalize(p)) out.push_back(std::move(*n));
    }
    return out;
}

bool HostLimits::allows_every_path() const {
    return std::ranges::any_of(prefixes(), [](const std::string& p) { return p == "/"; });
}

std::expected<std::string, std::string> normalize(std::string_view path) {
    if (path.find('\0') != std::string_view::npos) return std::unexpected(std::string("path contains a NUL byte"));
    if (!path.starts_with('/')) return std::unexpected("path `" + std::string(path) + "` is not absolute");
    std::vector<std::string_view> parts;
    std::size_t start = 0;
    while (start <= path.size()) {
        const auto end = std::min(path.find('/', start), path.size());
        const auto part = path.substr(start, end - start);
        if (part == "..") {
            if (!parts.empty()) parts.pop_back();
        } else if (!part.empty() && part != ".") {
            parts.push_back(part);
        }
        start = end + 1;
    }
    if (parts.empty()) return std::string("/");
    std::string out;
    for (const auto p : parts) {
        out += '/';
        out += p;
    }
    return out;
}

bool within(std::string_view path, std::string_view prefix) noexcept {
    if (prefix == "/") return true;
    if (!path.starts_with(prefix)) return false;
    const auto rest = path.substr(prefix.size());
    return rest.empty() || rest.starts_with('/');
}

std::pair<std::string_view, std::string_view> split(std::string_view path) noexcept {
    const auto i = path.rfind('/');
    if (i == std::string_view::npos) return {"/", path};
    if (i == 0) return {"/", path.substr(1)};
    return {path.substr(0, i), path.substr(i + 1)};
}

} // namespace loa::remote
