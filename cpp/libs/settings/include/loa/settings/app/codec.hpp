#pragma once

// Conversion between the raw TOML document and `Config`, keeping unknown keys.

#include <cstdint>
#include <string>
#include <string_view>

#include "loa/settings/app/error.hpp"
#include "loa/settings/app/schema.hpp"
#include "loa/settings/app/toml.hpp"

namespace loa::settings::app {

// Parses config text into a raw document (`ConfigError::Kind::Parse` on bad TOML).
[[nodiscard]] Result<toml::Table> parse(std::string_view text);

// Reads the top level `version` of a raw document.
[[nodiscard]] Result<std::uint32_t> version_of(const toml::Table& doc);

// Turns a document of the current version into a validated `Config`. Keys the schema
// doesn't know land in `Config::unknown`, by dotted path.
[[nodiscard]] Result<Config> decode(toml::Table doc);

// Writes `config` as TOML text, version and unknown keys included, as the Rust app does.
[[nodiscard]] Result<std::string> encode(const Config& config);

} // namespace loa::settings::app
