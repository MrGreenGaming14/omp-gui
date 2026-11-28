#pragma once

// Where the node's seed lives between runs: the KeyStore port, memory, OS
// keychain and file adapters, and the FallbackStore migration policy.

#include <cstdint>
#include <filesystem>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/trust/error.hpp"
#include "loa/trust/key.hpp"

namespace loa::trust {

// Secret bytes read from a KeyStore; wiped from memory when destroyed.
class SecretBytes {
  public:
    SecretBytes() = default;
    explicit SecretBytes(std::span<const std::uint8_t> bytes) : bytes_(bytes.begin(), bytes.end()) {}
    SecretBytes(const SecretBytes&) = default;
    SecretBytes(SecretBytes&& other) noexcept : bytes_(std::exchange(other.bytes_, {})) {}
    SecretBytes& operator=(const SecretBytes& other);
    SecretBytes& operator=(SecretBytes&& other) noexcept;
    ~SecretBytes();

    [[nodiscard]] std::span<const std::uint8_t> bytes() const noexcept { return bytes_; }
    [[nodiscard]] std::size_t size() const noexcept { return bytes_.size(); }

  private:
    void wipe() noexcept;
    std::vector<std::uint8_t> bytes_;
};

// Storage for this node's seed. Adapters store it verbatim and never log it.
class KeyStore {
  public:
    virtual ~KeyStore() = default;
    // The stored secret, none before the first store.
    [[nodiscard]] virtual Result<std::optional<SecretBytes>> load() const = 0;
    [[nodiscard]] virtual Result<void> store(std::span<const std::uint8_t> secret) const = 0;
    // Removes the secret; removing nothing succeeds (Rust `delete`).
    [[nodiscard]] virtual Result<void> remove() const = 0;
    // The file holding the secret, for stores that have one.
    [[nodiscard]] virtual std::optional<std::filesystem::path> path() const { return std::nullopt; }
};

// In-process store for tests and ephemeral nodes.
class MemoryKeyStore final : public KeyStore {
  public:
    MemoryKeyStore() = default;
    // A store already holding `secret`, of any length.
    [[nodiscard]] static MemoryKeyStore with_secret(std::span<const std::uint8_t> secret);
    MemoryKeyStore(MemoryKeyStore&& other) noexcept : secret_(std::move(other.secret_)) {}

    Result<std::optional<SecretBytes>> load() const override;
    Result<void> store(std::span<const std::uint8_t> secret) const override;
    Result<void> remove() const override;

  private:
    mutable std::mutex mutex_;
    mutable std::optional<SecretBytes> secret_;
};

// The OS credential store (Secret Service via libsecret, Keychain Services,
// Credential Manager): the app's `loa`/`node-key` entry.
class OsKeychain final : public KeyStore {
  public:
    static constexpr const char* SERVICE = "loa";
    static constexpr const char* USER = "node-key";

    Result<std::optional<SecretBytes>> load() const override;
    Result<void> store(std::span<const std::uint8_t> secret) const override;
    Result<void> remove() const override;
};

// `node.key` in the data directory, readable only by the user (0600 on Unix,
// a protected owner-only DACL on Windows).
class FileKeyStore final : public KeyStore {
  public:
    [[nodiscard]] static FileKeyStore in_dir(const std::filesystem::path& dir) { return FileKeyStore(dir / "node.key"); }

    Result<std::optional<SecretBytes>> load() const override;
    Result<void> store(std::span<const std::uint8_t> secret) const override;
    Result<void> remove() const override;
    std::optional<std::filesystem::path> path() const override { return path_; }

  private:
    explicit FileKeyStore(std::filesystem::path path) : path_(std::move(path)) {}
    std::filesystem::path path_;
};

// Where the loaded node key lives.
namespace key_source {
struct Keychain {
    friend bool operator==(const Keychain&, const Keychain&) = default;
};
// The key file, because the keychain could not be used.
struct File {
    std::filesystem::path path;
    std::string keychain_error;
    friend bool operator==(const File&, const File&) = default;
};
// Memory only: neither store works, or the key was handed in.
struct Memory {
    std::string reason;
    friend bool operator==(const Memory&, const Memory&) = default;
};
} // namespace key_source
using KeySource = std::variant<key_source::Keychain, key_source::File, key_source::Memory>;

// The migration policy as a decorator: `primary` unless it is `unavailable`,
// then `fallback`. A key left in `fallback` moves into `primary` once that
// works, so the address stays. Other primary errors (a malformed value) never
// switch stores.
class FallbackStore final : public KeyStore {
  public:
    FallbackStore(std::shared_ptr<const KeyStore> primary, std::shared_ptr<const KeyStore> fallback)
        : primary_(std::move(primary)), fallback_(std::move(fallback)) {}

    // Which store the last load used.
    [[nodiscard]] KeySource source() const;

    Result<std::optional<SecretBytes>> load() const override;
    Result<void> store(std::span<const std::uint8_t> secret) const override;
    Result<void> remove() const override;

  private:
    [[nodiscard]] const KeyStore& active() const;

    std::shared_ptr<const KeyStore> primary_;
    std::shared_ptr<const KeyStore> fallback_;
    mutable std::mutex mutex_;
    // Why `primary` was skipped, once it was.
    mutable std::optional<std::string> fell_back_;
};

// The app's node key: the OS keychain, else `<data_dir>/node.key`.
[[nodiscard]] Result<std::pair<NodeIdentity, KeySource>> load_app_identity(const std::filesystem::path& data_dir);

} // namespace loa::trust
