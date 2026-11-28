#include "private_file.hpp"

#if defined(_WIN32)

#include <windows.h>
#include <aclapi.h>

#include <openssl/rand.h>

#include <system_error>
#include <vector>

namespace loa::trust::detail {
namespace {

std::string last_error(DWORD code = GetLastError()) { return std::system_category().message(static_cast<int>(code)); }

struct Handle {
    HANDLE h = INVALID_HANDLE_VALUE;
    ~Handle() {
        if (h && h != INVALID_HANDLE_VALUE) CloseHandle(h);
    }
};

// Creates `path` (never an existing file) readable and writable only by the
// current user, through a protected DACL, on a volume that enforces ACLs.
std::expected<Handle, std::string> create(const std::filesystem::path& path) {
    Handle token;
    if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token.h)) return std::unexpected(last_error());
    DWORD needed = 0;
    GetTokenInformation(token.h, TokenUser, nullptr, 0, &needed);
    if (needed == 0) return std::unexpected(last_error());
    // A TOKEN_USER and its SID must be aligned and stay alive until the ACL is built.
    std::vector<std::uintptr_t> buffer((needed + sizeof(std::uintptr_t) - 1) / sizeof(std::uintptr_t));
    if (!GetTokenInformation(token.h, TokenUser, buffer.data(), needed, &needed)) return std::unexpected(last_error());
    PSID sid = reinterpret_cast<TOKEN_USER*>(buffer.data())->User.Sid;
    EXPLICIT_ACCESS_W access{};
    access.grfAccessPermissions = FILE_ALL_ACCESS;
    access.grfAccessMode = SET_ACCESS;
    access.grfInheritance = 0;
    access.Trustee.pMultipleTrustee = nullptr;
    access.Trustee.MultipleTrusteeOperation = NO_MULTIPLE_TRUSTEE;
    access.Trustee.TrusteeForm = TRUSTEE_IS_SID;
    access.Trustee.TrusteeType = TRUSTEE_IS_USER;
    access.Trustee.ptstrName = static_cast<LPWSTR>(sid);
    PACL acl = nullptr;
    if (const DWORD r = SetEntriesInAclW(1, &access, nullptr, &acl); r != ERROR_SUCCESS) return std::unexpected(last_error(r));
    struct AclGuard {
        PACL p;
        ~AclGuard() { LocalFree(p); }
    } guard{acl};
    SECURITY_DESCRIPTOR descriptor{};
    if (!InitializeSecurityDescriptor(&descriptor, SECURITY_DESCRIPTOR_REVISION) ||
        !SetSecurityDescriptorDacl(&descriptor, TRUE, acl, FALSE) ||
        !SetSecurityDescriptorControl(&descriptor, SE_DACL_PROTECTED, SE_DACL_PROTECTED))
        return std::unexpected(last_error());
    SECURITY_ATTRIBUTES attrs{sizeof(SECURITY_ATTRIBUTES), &descriptor, FALSE};
    // CREATE_NEW won't open a pre-existing path; share-delete allows the atomic publish.
    Handle file;
    file.h = CreateFileW(path.c_str(), GENERIC_WRITE, FILE_SHARE_DELETE, &attrs, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file.h == INVALID_HANDLE_VALUE) return std::unexpected(last_error());
    // FAT and exFAT ignore the DACL; never write a key that anyone can read.
    DWORD flags = 0;
    const BOOL queried = GetVolumeInformationByHandleW(file.h, nullptr, 0, nullptr, nullptr, &flags, nullptr, 0);
    if (!queried || !(flags & FILE_PERSISTENT_ACLS)) {
        std::string error = queried ? "the key folder's volume can't restrict file access" : last_error();
        CloseHandle(std::exchange(file.h, INVALID_HANDLE_VALUE));
        DeleteFileW(path.c_str());
        return std::unexpected(error);
    }
    return file;
}

} // namespace

std::expected<void, std::string> store_private_windows(const std::filesystem::path& path, std::span<const std::uint8_t> secret) {
    std::filesystem::path dir = path.parent_path();
    if (dir.empty()) dir = ".";
    std::error_code ec;
    std::filesystem::create_directories(dir, ec);
    if (ec) return std::unexpected(ec.message());
    for (int attempt = 0;; ++attempt) {
        unsigned char nonce[6];
        if (RAND_bytes(nonce, sizeof nonce) != 1) return std::unexpected(std::string("os random source failed"));
        static constexpr char digits[] = "abcdefghijklmnopqrstuvwxyz0123456789";
        std::string name = ".node-key-";
        for (const auto b : nonce) name += digits[b % 36];
        const auto temp = dir / name;
        auto file = create(temp);
        if (!file) {
            if (GetLastError() == ERROR_FILE_EXISTS && attempt < 16) continue;
            return std::unexpected(file.error());
        }
        DWORD written = 0;
        const bool ok = WriteFile(file->h, secret.data(), static_cast<DWORD>(secret.size()), &written, nullptr) &&
                        written == secret.size() && FlushFileBuffers(file->h);
        const std::string write_error = ok ? std::string() : last_error();
        CloseHandle(std::exchange(file->h, INVALID_HANDLE_VALUE));
        if (!ok || !MoveFileExW(temp.c_str(), path.c_str(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) {
            const std::string error = ok ? last_error() : write_error;
            DeleteFileW(temp.c_str());
            return std::unexpected(error);
        }
        return {};
    }
}

} // namespace loa::trust::detail

#endif
