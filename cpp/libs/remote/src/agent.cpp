#include "loa/remote/agent.hpp"

#include <cerrno>
#include <cstring>
#include <string>
#include <system_error>

#if defined(_WIN32)
#include <windows.h>
#else
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#endif

#include "wire.hpp"

namespace loa::remote {

namespace {

constexpr std::uint8_t REQUEST_IDENTITIES = 11;
constexpr std::uint8_t IDENTITIES_ANSWER = 12;
constexpr std::uint8_t FAILURE = 5;

std::string os_error(int code) { return std::error_code(code, std::system_category()).message(); }

#if defined(_WIN32)
struct Conn {
    HANDLE h = INVALID_HANDLE_VALUE;
    ~Conn() {
        if (h != INVALID_HANDLE_VALUE) CloseHandle(h);
    }
    std::optional<std::string> open(const std::filesystem::path& path) {
        h = CreateFileW(path.c_str(), GENERIC_READ | GENERIC_WRITE, 0, nullptr, OPEN_EXISTING, 0, nullptr);
        if (h == INVALID_HANDLE_VALUE) return os_error(static_cast<int>(GetLastError()));
        return std::nullopt;
    }
    bool write_all(std::string_view data) {
        while (!data.empty()) {
            DWORD n = 0;
            if (!WriteFile(h, data.data(), static_cast<DWORD>(data.size()), &n, nullptr)) return false;
            data.remove_prefix(n);
        }
        return true;
    }
    bool read_exact(char* out, std::size_t len) {
        while (len > 0) {
            DWORD n = 0;
            if (!ReadFile(h, out, static_cast<DWORD>(len), &n, nullptr) || n == 0) return false;
            out += n;
            len -= n;
        }
        return true;
    }
    static int last_error() { return static_cast<int>(GetLastError()); }
};
#else
struct Conn {
    int fd = -1;
    ~Conn() {
        if (fd >= 0) ::close(fd);
    }
    std::optional<std::string> open(const std::filesystem::path& path) {
        sockaddr_un addr{};
        addr.sun_family = AF_UNIX;
        const auto s = path.string();
        if (s.size() >= sizeof addr.sun_path) return std::string("socket path is too long");
        std::memcpy(addr.sun_path, s.c_str(), s.size() + 1);
        fd = ::socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
        if (fd < 0) return os_error(errno);
        if (::connect(fd, reinterpret_cast<const sockaddr*>(&addr), sizeof addr) != 0) return os_error(errno);
        return std::nullopt;
    }
    bool write_all(std::string_view data) {
        while (!data.empty()) {
            const auto n = ::send(fd, data.data(), data.size(), MSG_NOSIGNAL);
            if (n < 0 && errno == EINTR) continue;
            if (n <= 0) return false;
            data.remove_prefix(static_cast<std::size_t>(n));
        }
        return true;
    }
    bool read_exact(char* out, std::size_t len) {
        while (len > 0) {
            const auto n = ::read(fd, out, len);
            if (n < 0 && errno == EINTR) continue;
            if (n <= 0) {
                if (n == 0) errno = ECONNRESET;
                return false;
            }
            out += n;
            len -= static_cast<std::size_t>(n);
        }
        return true;
    }
    static int last_error() { return errno; }
};
#endif

} // namespace

Result<std::filesystem::path> socket_for(const AgentSocket& agent, const std::optional<std::filesystem::path>& fallback,
                                         std::string_view host) {
    switch (agent.kind) {
    case AgentSocket::Kind::path: return agent.path;
    case AgentSocket::Kind::default_socket:
        if (fallback) return *fallback;
        break;
    case AgentSocket::Kind::disabled: break;
    }
    return std::unexpected(Error::no_agent(std::string(host)));
}

Result<std::vector<PublicKey>> agent_identities(const std::filesystem::path& socket) {
    const auto fail = [&](std::string message) { return std::unexpected(Error::agent(socket.string(), std::move(message))); };
    Conn conn;
    if (auto e = conn.open(socket)) return fail(*e);
    wire::Bytes request;
    wire::put_u32(request, 1);
    wire::put_u8(request, REQUEST_IDENTITIES);
    if (!conn.write_all(request)) return fail(os_error(Conn::last_error()));
    char len_bytes[4];
    if (!conn.read_exact(len_bytes, 4)) return fail(os_error(Conn::last_error()));
    const auto len = *wire::Reader(std::string_view(len_bytes, 4)).u32();
    if (len == 0 || len > 256 * 1024) return fail("agent reply of " + std::to_string(len) + " bytes");
    std::string body(len, '\0');
    if (!conn.read_exact(body.data(), len)) return fail(os_error(Conn::last_error()));
    wire::Reader r(body);
    const auto type = r.u8();
    if (type == FAILURE) return fail("the agent refused to list identities");
    if (type != IDENTITIES_ANSWER) return fail("unexpected agent reply " + std::to_string(type.value_or(0)));
    const auto count = r.u32();
    if (!count) return fail("truncated identities answer");
    std::vector<PublicKey> out;
    for (std::uint32_t i = 0; i < *count; ++i) {
        const auto blob = r.string();
        const auto comment = r.string();
        if (!blob || !comment) return fail("truncated identities answer");
        if (auto key = PublicKey::from_blob(std::string(*blob))) out.push_back(std::move(*key));
    }
    return out;
}

} // namespace loa::remote
