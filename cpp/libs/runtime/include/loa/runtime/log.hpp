#pragma once

#include <expected>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/runtime/error.hpp"

namespace loa::runtime::log {

// `off` disables output; otherwise a message passes when its level <= the filter level.
enum class Level : int { off = 0, error, warn, info, debug, trace };

[[nodiscard]] std::string_view to_string(Level level) noexcept;
[[nodiscard]] std::optional<Level> parse_level(std::string_view text) noexcept;

// `LOA_LOG` syntax: comma-separated directives; a bare level sets the default,
// `target=level` overrides it for `target` and its `::` children (longest match wins).
struct Filter {
    Level default_level = Level::error;
    std::vector<std::pair<std::string, Level>> overrides;

    [[nodiscard]] static std::expected<Filter, loa::Error> parse(std::string_view spec);
    [[nodiscard]] Level level_for(std::string_view target) const noexcept;
    [[nodiscard]] bool enabled(Level level, std::string_view target) const noexcept {
        return level != Level::off && level <= level_for(target);
    }
};

// Reloads the global filter from `LOA_LOG` (invalid or unset -> default `error`).
void init_from_env();
void set_filter(Filter filter);
[[nodiscard]] bool enabled(Level level, std::string_view target);
// Writes "LEVEL target: message" to stderr if enabled.
void write(Level level, std::string_view target, std::string_view message);

inline void error(std::string_view target, std::string_view m) { write(Level::error, target, m); }
inline void warn(std::string_view target, std::string_view m) { write(Level::warn, target, m); }
inline void info(std::string_view target, std::string_view m) { write(Level::info, target, m); }
inline void debug(std::string_view target, std::string_view m) { write(Level::debug, target, m); }
inline void trace(std::string_view target, std::string_view m) { write(Level::trace, target, m); }

} // namespace loa::runtime::log
