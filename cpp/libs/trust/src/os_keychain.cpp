// OsKeychain: the `loa`/`node-key` entry exactly as the Rust `keyring` 4.2
// stores write it, so either app reads the other's key.
//   Linux/BSD: Secret Service item in the `default` collection, attributes
//     `service`=loa and `username`=node-key, label `keyring:node-key@loa`,
//     content type `application/octet-stream`.
//   macOS: generic password, service `loa`, account `node-key`.
//   Windows: generic credential, target `node-key.loa`, user `node-key`,
//     enterprise persistence.
// Errors map like Rust `keychain_error`: no entry is none; no store, no access
// or a platform failure is `unavailable`; anything else is `store`.

#include "loa/trust/keystore.hpp"

#include <string>

#if defined(__APPLE__)
#include <CoreFoundation/CoreFoundation.h>
#include <Security/Security.h>
#elif defined(_WIN32)
#include <windows.h>
#include <wincred.h>
#else
#include <libsecret/secret.h>
#endif

namespace loa::trust {

#if defined(__APPLE__)

namespace {

struct Cf {
    CFTypeRef ref = nullptr;
    ~Cf() {
        if (ref) CFRelease(ref);
    }
};

TrustError keychain_error(OSStatus status) {
    std::string text = "OSStatus " + std::to_string(status);
    if (CFStringRef message = SecCopyErrorMessageString(status, nullptr)) {
        char buf[512];
        if (CFStringGetCString(message, buf, sizeof buf, kCFStringEncodingUTF8)) text = buf;
        CFRelease(message);
    }
    // No storage access (-61, -25244, -25291, -25292, -25294, -25295) and every
    // other platform failure: the keychain can't be used.
    return TrustError::unavailable(text);
}

CFMutableDictionaryRef query() {
    CFMutableDictionaryRef q = CFDictionaryCreateMutable(nullptr, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFStringRef service = CFStringCreateWithCString(nullptr, OsKeychain::SERVICE, kCFStringEncodingUTF8);
    CFStringRef account = CFStringCreateWithCString(nullptr, OsKeychain::USER, kCFStringEncodingUTF8);
    CFDictionarySetValue(q, kSecClass, kSecClassGenericPassword);
    CFDictionarySetValue(q, kSecAttrService, service);
    CFDictionarySetValue(q, kSecAttrAccount, account);
    CFRelease(service);
    CFRelease(account);
    return q;
}

constexpr OSStatus ITEM_NOT_FOUND = -25300;

} // namespace

Result<std::optional<SecretBytes>> OsKeychain::load() const {
    Cf q{query()};
    auto* dict = (CFMutableDictionaryRef)q.ref;
    CFDictionarySetValue(dict, kSecReturnData, kCFBooleanTrue);
    CFDictionarySetValue(dict, kSecMatchLimit, kSecMatchLimitOne);
    Cf data;
    const OSStatus status = SecItemCopyMatching(dict, &data.ref);
    if (status == ITEM_NOT_FOUND) return std::optional<SecretBytes>();
    if (status != errSecSuccess) return std::unexpected(keychain_error(status));
    auto* bytes = (CFDataRef)data.ref;
    return std::optional<SecretBytes>(SecretBytes(std::span(CFDataGetBytePtr(bytes), static_cast<std::size_t>(CFDataGetLength(bytes)))));
}

Result<void> OsKeychain::store(std::span<const std::uint8_t> secret) const {
    Cf q{query()};
    auto* dict = (CFMutableDictionaryRef)q.ref;
    Cf data{CFDataCreate(nullptr, secret.data(), static_cast<CFIndex>(secret.size()))};
    Cf update{CFDictionaryCreateMutable(nullptr, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks)};
    CFDictionarySetValue((CFMutableDictionaryRef)update.ref, kSecValueData, data.ref);
    OSStatus status = SecItemUpdate(dict, (CFDictionaryRef)update.ref);
    if (status == ITEM_NOT_FOUND) {
        CFDictionarySetValue(dict, kSecValueData, data.ref);
        status = SecItemAdd(dict, nullptr);
    }
    if (status != errSecSuccess) return std::unexpected(keychain_error(status));
    return {};
}

Result<void> OsKeychain::remove() const {
    Cf q{query()};
    const OSStatus status = SecItemDelete((CFDictionaryRef)q.ref);
    if (status != errSecSuccess && status != ITEM_NOT_FOUND) return std::unexpected(keychain_error(status));
    return {};
}

#elif defined(_WIN32)

namespace {

constexpr const wchar_t* TARGET = L"node-key.loa";
constexpr const wchar_t* USER_W = L"node-key";

TrustError keychain_error(DWORD code) {
    // ERROR_NO_SUCH_LOGON_SESSION is no storage access; the rest platform failures.
    return TrustError::unavailable("Windows error " + std::to_string(code));
}

} // namespace

Result<std::optional<SecretBytes>> OsKeychain::load() const {
    PCREDENTIALW cred = nullptr;
    if (!CredReadW(TARGET, CRED_TYPE_GENERIC, 0, &cred)) {
        const DWORD code = GetLastError();
        if (code == ERROR_NOT_FOUND) return std::optional<SecretBytes>();
        return std::unexpected(keychain_error(code));
    }
    SecretBytes secret(std::span<const std::uint8_t>(cred->CredentialBlob, cred->CredentialBlobSize));
    SecureZeroMemory(cred->CredentialBlob, cred->CredentialBlobSize);
    CredFree(cred);
    return std::optional<SecretBytes>(std::move(secret));
}

Result<void> OsKeychain::store(std::span<const std::uint8_t> secret) const {
    CREDENTIALW cred{};
    cred.Type = CRED_TYPE_GENERIC;
    cred.TargetName = const_cast<LPWSTR>(TARGET);
    cred.UserName = const_cast<LPWSTR>(USER_W);
    cred.CredentialBlobSize = static_cast<DWORD>(secret.size());
    cred.CredentialBlob = const_cast<LPBYTE>(secret.data());
    cred.Persist = CRED_PERSIST_ENTERPRISE;
    if (!CredWriteW(&cred, 0)) return std::unexpected(keychain_error(GetLastError()));
    return {};
}

Result<void> OsKeychain::remove() const {
    if (!CredDeleteW(TARGET, CRED_TYPE_GENERIC, 0)) {
        const DWORD code = GetLastError();
        if (code != ERROR_NOT_FOUND) return std::unexpected(keychain_error(code));
    }
    return {};
}

#else

namespace {

const SecretSchema* schema() {
    // Items are matched on `service` and `username` only, like keyring's
    // search; DONT_MATCH_NAME keeps items without `xdg:schema` visible.
    static const SecretSchema s = {
        "org.freedesktop.Secret.Generic",
        SECRET_SCHEMA_DONT_MATCH_NAME,
        {{"service", SECRET_SCHEMA_ATTRIBUTE_STRING}, {"username", SECRET_SCHEMA_ATTRIBUTE_STRING}, {nullptr, SECRET_SCHEMA_ATTRIBUTE_STRING}},
        0, nullptr, nullptr, nullptr, nullptr, nullptr, nullptr, nullptr};
    return &s;
}

struct Attributes {
    GHashTable* table = g_hash_table_new(g_str_hash, g_str_equal);
    Attributes() {
        g_hash_table_insert(table, const_cast<char*>("service"), const_cast<char*>(OsKeychain::SERVICE));
        g_hash_table_insert(table, const_cast<char*>("username"), const_cast<char*>(OsKeychain::USER));
    }
    ~Attributes() { g_hash_table_unref(table); }
};

// Every Secret Service failure is a lost connection, a locked collection or a
// dismissed prompt: the store can't be used.
TrustError keychain_error(GError* error) {
    std::string text = error && error->message ? error->message : "secret service failure";
    if (error) g_error_free(error);
    return TrustError::unavailable(std::move(text));
}

struct Items {
    GList* list = nullptr;
    ~Items() { g_list_free_full(list, g_object_unref); }
};

// The matching items; more than one is keyring's `Ambiguous`.
Result<GList*> find(Items& items, bool load) {
    GError* error = nullptr;
    int flags = SECRET_SEARCH_ALL | SECRET_SEARCH_UNLOCK;
    if (load) flags |= SECRET_SEARCH_LOAD_SECRETS;
    Attributes attrs;
    items.list = secret_password_searchv_sync(schema(), attrs.table, static_cast<SecretSearchFlags>(flags), nullptr, &error);
    if (error) return std::unexpected(keychain_error(error));
    if (g_list_length(items.list) > 1)
        return std::unexpected(TrustError::store("Entry is matched by multiple credentials"));
    return items.list;
}

} // namespace

Result<std::optional<SecretBytes>> OsKeychain::load() const {
    Items items;
    auto found = find(items, true);
    if (!found) return std::unexpected(found.error());
    if (!*found) return std::optional<SecretBytes>();
    GError* error = nullptr;
    SecretValue* value = secret_retrievable_retrieve_secret_sync(SECRET_RETRIEVABLE((*found)->data), nullptr, &error);
    if (error) return std::unexpected(keychain_error(error));
    if (!value) return std::unexpected(TrustError::unavailable("the secret service returned no secret"));
    gsize len = 0;
    const gchar* data = secret_value_get(value, &len);
    SecretBytes secret(std::span(reinterpret_cast<const std::uint8_t*>(data), len));
    secret_value_unref(value);
    return std::optional<SecretBytes>(std::move(secret));
}

Result<void> OsKeychain::store(std::span<const std::uint8_t> secret) const {
    Items items;
    if (auto found = find(items, false); !found) return std::unexpected(found.error());
    SecretValue* value = secret_value_new(reinterpret_cast<const gchar*>(secret.data()), static_cast<gssize>(secret.size()),
                                          "application/octet-stream");
    GError* error = nullptr;
    Attributes attrs;
    const std::string label = std::string("keyring:") + USER + "@" + SERVICE;
    secret_password_storev_binary_sync(schema(), attrs.table, SECRET_COLLECTION_DEFAULT, label.c_str(), value, nullptr, &error);
    secret_value_unref(value);
    if (error) return std::unexpected(keychain_error(error));
    return {};
}

Result<void> OsKeychain::remove() const {
    Items items;
    if (auto found = find(items, false); !found) return std::unexpected(found.error());
    GError* error = nullptr;
    Attributes attrs;
    secret_password_clearv_sync(schema(), attrs.table, nullptr, &error);
    if (error) return std::unexpected(keychain_error(error));
    return {};
}

#endif

} // namespace loa::trust
