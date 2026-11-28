#include "loa/runtime/log.hpp"

#include <cstdio>
#include <cstdlib>
#include <mutex>
#include <shared_mutex>

namespace loa::runtime::log {
namespace {

std::string_view trim(std::string_view s) {
    while (!s.empty() && (s.front() == ' ' || s.front() == '\t')) s.remove_prefix(1);
    while (!s.empty() && (s.back() == ' ' || s.back() == '\t')) s.remove_suffix(1);
    return s;
}

struct Global {
    std::shared_mutex mu;
    Filter filter;
    std::mutex write_mu;
    bool loaded = false;
};

Global& global() {
    static Global g;
    return g;
}

Filter env_filter() {
    const char* spec = std::getenv("LOA_LOG");
    if (spec == nullptr) return {};
    return Filter::parse(spec).value_or(Filter{});
}

} // namespace

std::string_view to_string(Level level) noexcept {
    switch (level) {
    case Level::off: return "OFF";
    case Level::error: return "ERROR";
    case Level::warn: return "WARN";
    case Level::info: return "INFO";
    case Level::debug: return "DEBUG";
    case Level::trace: return "TRACE";
    }
    return "OFF";
}

std::optional<Level> parse_level(std::string_view text) noexcept {
    std::string lower(text);
    for (char& c : lower) c = static_cast<char>(c >= 'A' && c <= 'Z' ? c - 'A' + 'a' : c);
    if (lower == "off") return Level::off;
    if (lower == "error") return Level::error;
    if (lower == "warn" || lower == "warning") return Level::warn;
    if (lower == "info") return Level::info;
    if (lower == "debug") return Level::debug;
    if (lower == "trace") return Level::trace;
    return std::nullopt;
}

std::expected<Filter, loa::Error> Filter::parse(std::string_view spec) {
    Filter filter;
    while (!spec.empty()) {
        const auto comma = spec.find(',');
        const std::string_view item = trim(spec.substr(0, comma));
        spec = comma == std::string_view::npos ? std::string_view{} : spec.substr(comma + 1);
        if (item.empty()) continue;
        const auto eq = item.find('=');
        if (eq == std::string_view::npos) {
            const auto level = parse_level(item);
            if (!level) return std::unexpected(loa::Error(ErrorCode::parse, "LOA_LOG: unknown level '" + std::string(item) + "'"));
            filter.default_level = *level;
            continue;
        }
        const std::string_view target = trim(item.substr(0, eq));
        const auto level = parse_level(trim(item.substr(eq + 1)));
        if (target.empty()) return std::unexpected(loa::Error(ErrorCode::parse, "LOA_LOG: empty target in '" + std::string(item) + "'"));
        if (!level) return std::unexpected(loa::Error(ErrorCode::parse, "LOA_LOG: unknown level in '" + std::string(item) + "'"));
        filter.overrides.emplace_back(std::string(target), *level);
    }
    return filter;
}

Level Filter::level_for(std::string_view target) const noexcept {
    Level level = default_level;
    std::size_t best = 0;
    bool matched = false;
    for (const auto& [prefix, l] : overrides) {
        const bool hit = target == prefix ||
                         (target.starts_with(prefix) && target.substr(prefix.size()).starts_with("::"));
        // Longest match wins; a later directive of equal length replaces an earlier one.
        if (hit && (!matched || prefix.size() >= best)) {
            level = l;
            best = prefix.size();
            matched = true;
        }
    }
    return level;
}

void init_from_env() { set_filter(env_filter()); }

void set_filter(Filter filter) {
    auto& g = global();
    std::unique_lock lock(g.mu);
    g.filter = std::move(filter);
    g.loaded = true;
}

bool enabled(Level level, std::string_view target) {
    auto& g = global();
    {
        std::shared_lock lock(g.mu);
        if (g.loaded) return g.filter.enabled(level, target);
    }
    std::unique_lock lock(g.mu);
    if (!g.loaded) {
        g.filter = env_filter();
        g.loaded = true;
    }
    return g.filter.enabled(level, target);
}

void write(Level level, std::string_view target, std::string_view message) {
    if (!enabled(level, target)) return;
    const auto name = to_string(level);
    auto& g = global();
    std::lock_guard lock(g.write_mu);
    std::fprintf(stderr, "%.*s %.*s: %.*s\n", static_cast<int>(name.size()), name.data(), static_cast<int>(target.size()),
                 target.data(), static_cast<int>(message.size()), message.data());
}

} // namespace loa::runtime::log
