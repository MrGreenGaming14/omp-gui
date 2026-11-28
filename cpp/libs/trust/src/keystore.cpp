#include "loa/trust/keystore.hpp"

#include <openssl/crypto.h>

#include <string_view>
#include <system_error>

#include "loa/runtime/fs.hpp"

#if defined(_WIN32)
#include "private_file.hpp"
#endif

namespace loa::trust {

SecretBytes& SecretBytes::operator=(const SecretBytes& other) {
    if (this != &other) {
        wipe();
        bytes_ = other.bytes_;
    }
    return *this;
}

SecretBytes& SecretBytes::operator=(SecretBytes&& other) noexcept {
    if (this != &other) {
        wipe();
        bytes_ = std::exchange(other.bytes_, {});
    }
    return *this;
}

SecretBytes::~SecretBytes() { wipe(); }

void SecretBytes::wipe() noexcept {
    if (!bytes_.empty()) OPENSSL_cleanse(bytes_.data(), bytes_.size());
    bytes_.clear();
}

// ---------------------------------------------------------------- memory

MemoryKeyStore MemoryKeyStore::with_secret(std::span<const std::uint8_t> secret) {
    MemoryKeyStore store;
    store.secret_ = SecretBytes(secret);
    return store;
}

Result<std::optional<SecretBytes>> MemoryKeyStore::load() const {
    std::lock_guard lock(mutex_);
    return secret_;
}

Result<void> MemoryKeyStore::store(std::span<const std::uint8_t> secret) const {
    std::lock_guard lock(mutex_);
    secret_ = SecretBytes(secret);
    return {};
}

Result<void> MemoryKeyStore::remove() const {
    std::lock_guard lock(mutex_);
    secret_.reset();
    return {};
}

// ---------------------------------------------------------------- file

namespace {

TrustError file_error(const std::filesystem::path& path, const std::string& e) {
    return TrustError::store(path.string() + ": " + e);
}

#if defined(_WIN32)
// Publishes only a fully written, synced key. The file gets its private DACL
// before another process can open it.
Result<void> store_private(const std::filesystem::path& path, std::span<const std::uint8_t> secret) {
    if (auto r = detail::store_private_windows(path, secret); !r) return std::unexpected(file_error(path, r.error()));
    return {};
}
#else
Result<void> store_private(const std::filesystem::path& path, std::span<const std::uint8_t> secret) {
    const std::string_view bytes(reinterpret_cast<const char*>(secret.data()), secret.size());
    if (auto r = runtime::fs::write_atomic(path, bytes, 0600u); !r) return std::unexpected(file_error(path, r.error().to_string()));
    return {};
}
#endif

} // namespace

Result<std::optional<SecretBytes>> FileKeyStore::load() const {
    auto read = runtime::fs::read_optional(path_);
    if (!read) return std::unexpected(file_error(path_, read.error().to_string()));
    if (!*read) return std::optional<SecretBytes>();
    std::string& text = **read;
    SecretBytes secret(std::span(reinterpret_cast<const std::uint8_t*>(text.data()), text.size()));
    if (!text.empty()) OPENSSL_cleanse(text.data(), text.size());
    return std::optional<SecretBytes>(std::move(secret));
}

Result<void> FileKeyStore::store(std::span<const std::uint8_t> secret) const { return store_private(path_, secret); }

Result<void> FileKeyStore::remove() const {
    std::error_code ec;
    std::filesystem::remove(path_, ec);
    if (ec && ec != std::errc::no_such_file_or_directory) return std::unexpected(file_error(path_, ec.message()));
    return {};
}

// ---------------------------------------------------------------- fallback

KeySource FallbackStore::source() const {
    std::lock_guard lock(mutex_);
    if (!fell_back_) return key_source::Keychain{};
    return key_source::File{fallback_->path().value_or(std::filesystem::path()), *fell_back_};
}

const KeyStore& FallbackStore::active() const {
    std::lock_guard lock(mutex_);
    return fell_back_ ? *fallback_ : *primary_;
}

Result<std::optional<SecretBytes>> FallbackStore::load() const {
    auto primary = primary_->load();
    if (primary && !*primary) {
        auto secret = fallback_->load();
        if (!secret || !*secret) return secret;
        auto seed = seed_of((*secret)->bytes());
        if (!seed) return std::unexpected(seed.error());
        auto stored = primary_->store(*seed);
        OPENSSL_cleanse(seed->data(), seed->size());
        if (!stored) return std::unexpected(stored.error());
        if (auto r = fallback_->remove(); !r) return std::unexpected(r.error());
        return secret;
    }
    if (!primary && primary.error().kind() == TrustError::Kind::unavailable) {
        {
            std::lock_guard lock(mutex_);
            fell_back_ = primary.error().message();
        }
        return fallback_->load();
    }
    return primary;
}

Result<void> FallbackStore::store(std::span<const std::uint8_t> secret) const { return active().store(secret); }
Result<void> FallbackStore::remove() const { return active().remove(); }

Result<std::pair<NodeIdentity, KeySource>> load_app_identity(const std::filesystem::path& data_dir) {
    const FallbackStore store(std::make_shared<OsKeychain>(), std::make_shared<FileKeyStore>(FileKeyStore::in_dir(data_dir)));
    auto identity = NodeIdentity::load_or_create(store);
    if (!identity) return std::unexpected(identity.error());
    return std::pair{std::move(*identity), store.source()};
}

} // namespace loa::trust
