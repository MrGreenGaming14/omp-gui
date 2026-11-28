#pragma once

// `known_hosts` checks: hashed names (HMAC-SHA1), wildcard and negated patterns, `[host]:port`,
// `@cert-authority` and `@revoked`. Files are re-read on every check; accepted keys are appended.

#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/remote/error.hpp"

namespace loa::remote {

// An SSH public key or certificate in wire (blob) form.
struct PublicKey {
    // The blob's type string, e.g. `ssh-ed25519` or `ssh-ed25519-cert-v01@openssh.com`.
    std::string type;
    std::string blob;

    // `type base64 [comment]`, the type matching the blob's; none when malformed.
    [[nodiscard]] static std::optional<PublicKey> from_openssh(std::string_view line);
    [[nodiscard]] static std::optional<PublicKey> from_blob(std::string blob);
    [[nodiscard]] bool is_certificate() const noexcept;
    // `type base64`, no comment.
    [[nodiscard]] std::string to_openssh() const;
    // The key algorithm (`ssh-ed25519` for an ed25519 certificate too).
    [[nodiscard]] std::string algorithm() const;
    // The plain key: the certified key of a certificate, else this key.
    [[nodiscard]] PublicKey key_data() const;
    friend bool operator==(const PublicKey&, const PublicKey&) = default;
};

// The name `known_hosts` uses for a host: `host` on port 22, else `[host]:port`.
[[nodiscard]] std::string host_name(std::string_view hostname, std::uint16_t port);
// `SHA256:…` fingerprint of a key (of its plain key data), as `ssh-keygen -l` prints it.
[[nodiscard]] std::string fingerprint(const PublicKey& key);

// A key already on file for a host.
struct KnownKey {
    std::filesystem::path file;
    std::size_t line = 0;
    std::string key_type;
    std::string fingerprint;
    friend bool operator==(const KnownKey&, const KnownKey&) = default;
};

// Outcome of checking a presented host key.
struct HostKeyStatus {
    enum class Kind {
        trusted,
        // Listed under `@revoked`: never connect, never prompt.
        revoked,
        unknown,
        // Keys are on file for this host but none matches.
        changed,
    };
    Kind kind = Kind::unknown;
    // The `@revoked` entry, for `revoked`.
    std::optional<KnownKey> entry;
    // The keys on file, for `changed`.
    std::vector<KnownKey> previous;
    friend bool operator==(const HostKeyStatus&, const HostKeyStatus&) = default;
};

// The `known_hosts` files a connection is checked against.
class KnownHosts {
  public:
    // `files` are read in order; accepted keys are appended to `write_to`.
    KnownHosts(std::vector<std::filesystem::path> files, std::filesystem::path write_to)
        : files_(std::move(files)), write_to_(std::move(write_to)) {}

    // Checks the key (or certificate) `hostname:port` presented.
    [[nodiscard]] Result<HostKeyStatus> check(std::string_view hostname, std::uint16_t port, const PublicKey& presented) const;
    // Appends `hostname:port key` (the plain key) to the writable file.
    [[nodiscard]] Result<void> add(std::string_view hostname, std::uint16_t port, const PublicKey& key) const;

  private:
    std::vector<std::filesystem::path> files_;
    std::filesystem::path write_to_;
    mutable std::mutex write_lock_;
};

} // namespace loa::remote
