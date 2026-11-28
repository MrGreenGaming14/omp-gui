#pragma once

// Every error of the updater: a kind to match on and the Rust `Display` text.

#include <cstdint>
#include <expected>
#include <filesystem>
#include <string>
#include <string_view>
#include <system_error>

#include "loa/runtime/error.hpp"
#include "loa/runtime/semver.hpp"

namespace loa::updater {

using Version = loa::runtime::semver::Version;
using Path = std::filesystem::path;

class Error {
  public:
    enum class Kind : std::uint8_t {
        Io,
        Json,
        UnknownProduct,
        NotInstalled,
        NothingOffered,
        NotStaged,
        NoExecutable,
        Http,
        Status,
        RateLimited,
        Feed,
        Unverifiable,
        BadName,
        TooLarge,
        Mismatch,
        UnsafeEntry,
        Archive,
        NoPrevious,
        Unsigned,
        Signature,
    };

    Error(Kind kind, std::string message) : kind_(kind), message_(std::move(message)) {}

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // The Rust `Display` text.
    [[nodiscard]] const std::string& message() const noexcept { return message_; }
    [[nodiscard]] std::string to_string() const { return message_; }
    // For `RateLimited`: the unix time the limit resets.
    [[nodiscard]] std::uint64_t retry_at() const noexcept { return retry_at_; }

    [[nodiscard]] static Error io(const Path& path, std::string_view source);
    [[nodiscard]] static Error io(const Path& path, std::error_code ec);
    [[nodiscard]] static Error io(const Path& path, const loa::Error& source);
    [[nodiscard]] static Error json(const Path& path, std::string_view source);
    [[nodiscard]] static Error unknown_product(std::string_view id);
    [[nodiscard]] static Error not_installed(std::string_view product, std::string_view reason);
    [[nodiscard]] static Error nothing_offered(std::string_view product);
    [[nodiscard]] static Error not_staged(std::string_view product, const Version& version);
    [[nodiscard]] static Error no_executable(std::string_view product, const Version& version, std::string_view reason);
    [[nodiscard]] static Error http(std::string_view url, std::string_view reason);
    [[nodiscard]] static Error status(std::string_view url, long status);
    [[nodiscard]] static Error rate_limited(std::uint64_t until);
    [[nodiscard]] static Error feed(std::string_view message);
    [[nodiscard]] static Error unverifiable(std::string_view product, const Version& version, std::string_view what);
    [[nodiscard]] static Error bad_name(std::string_view name);
    [[nodiscard]] static Error too_large(std::string_view url, std::uint64_t limit);
    [[nodiscard]] static Error mismatch(std::string_view what, std::string_view expected, std::string_view actual);
    [[nodiscard]] static Error unsafe_entry(std::string_view entry, std::string_view reason);
    [[nodiscard]] static Error archive(std::string_view message);
    [[nodiscard]] static Error no_previous(std::string_view product);
    [[nodiscard]] static Error unsigned_artifact(const Path& artifact);
    [[nodiscard]] static Error signature(std::string_view message);

  private:
    Kind kind_;
    std::string message_;
    std::uint64_t retry_at_ = 0;
};

template <typename T = void>
using Result = std::expected<T, Error>;

// Rust `{:?}` of a string: quoted, with `"`, `\` and control characters escaped.
[[nodiscard]] std::string debug_str(std::string_view s);

} // namespace loa::updater
