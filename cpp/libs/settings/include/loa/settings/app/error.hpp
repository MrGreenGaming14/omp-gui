#pragma once

#include <cstdint>
#include <expected>
#include <filesystem>
#include <string>

#include "loa/runtime/error.hpp"

namespace loa::settings::app {

// Everything that can go wrong loading, validating or saving the config.
struct ConfigError {
    enum class Kind : std::uint8_t {
        Read,             // path, message
        Write,            // path, message
        Parse,            // message: the TOML parser's description
        MissingVersion,
        Invalid,          // key (dotted path), reason
        TooNew,           // found, supported: written by a newer app; never downgraded
        MissingMigration, // from
        Serialize,        // message
    };

    Kind kind = Kind::Invalid;
    std::filesystem::path path;
    std::string message;
    std::string key;
    std::string reason;
    std::uint32_t found = 0;
    std::uint32_t supported = 0;
    std::uint32_t from = 0;

    [[nodiscard]] static ConfigError invalid(std::string key, std::string reason);

    // The Rust `Display` text.
    [[nodiscard]] std::string to_string() const;
    // As a `loa::Error` for callers that don't care about the kind.
    [[nodiscard]] loa::Error to_error() const;
};

template <typename T = void>
using Result = std::expected<T, ConfigError>;

} // namespace loa::settings::app
