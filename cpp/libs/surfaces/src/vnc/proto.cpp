// RFB wire format and errors. Port of `loa-surfaces/src/vnc/{mod,proto}.rs`.

#define OPENSSL_SUPPRESS_DEPRECATED
#include <openssl/des.h>
#include <openssl/crypto.h>

#include <asio/awaitable.hpp>
#include <asio/use_awaitable.hpp>

#include <algorithm>
#include <charconv>
#include <cstring>
#include <format>

#include "internal.hpp"

namespace loa::surfaces::vnc {

namespace {

constexpr std::uint32_t SEC_INVALID = 0;
constexpr std::uint8_t SEC_NONE = 1;
constexpr std::uint8_t SEC_VNC_AUTH = 2;

// Upper bound for server supplied strings (reasons, desktop name).
constexpr std::uint32_t MAX_STRING = 1U << 16U;

// Rust `{:?}` of a string.
std::string debug_str(std::string_view s) {
    std::string out = "\"";
    for (const char32_t c : detail::utf8_decode(s)) {
        switch (c) {
        case U'"': out += "\\\""; break;
        case U'\\': out += "\\\\"; break;
        case U'\n': out += "\\n"; break;
        case U'\r': out += "\\r"; break;
        case U'\t': out += "\\t"; break;
        case U'\0': out += "\\0"; break;
        default:
            if (c < 0x20 || (c >= 0x7f && c < 0xa0)) {
                out += std::format("\\u{{{:x}}}", std::uint32_t(c));
            } else {
                out += detail::utf8_encode(c);
            }
        }
    }
    return out + "\"";
}

class FnPasswords final : public PasswordSource {
  public:
    explicit FnPasswords(std::function<std::optional<std::string>(std::string_view)> fn) : fn_(std::move(fn)) {}
    loa::contracts::Awaitable<std::optional<std::string>> password(std::string endpoint) override { co_return fn_(endpoint); }

  private:
    std::function<std::optional<std::string>(std::string_view)> fn_;
};

std::uint8_t reverse_bits(std::uint8_t b) {
    b = std::uint8_t((b & 0xF0U) >> 4U | (b & 0x0FU) << 4U);
    b = std::uint8_t((b & 0xCCU) >> 2U | (b & 0x33U) << 2U);
    return std::uint8_t((b & 0xAAU) >> 1U | (b & 0x55U) << 1U);
}

void be16(std::vector<std::uint8_t>& buf, std::uint16_t v) {
    buf.push_back(std::uint8_t(v >> 8U));
    buf.push_back(std::uint8_t(v));
}
void be32(std::vector<std::uint8_t>& buf, std::uint32_t v) {
    for (int shift = 24; shift >= 0; shift -= 8) buf.push_back(std::uint8_t(v >> unsigned(shift)));
}

// Reads a u32 length prefixed string.
asio::awaitable<std::string> read_string(Reader& rd) {
    const std::uint32_t len = co_await rd.u32();
    if (len > MAX_STRING) detail::fail(RfbError::protocol(std::format("string of {} bytes", len)));
    std::vector<std::uint8_t> bytes(len);
    co_await rd.read_exact(bytes);
    // from_utf8_lossy: invalid sequences become U+FFFD.
    std::string text;
    for (const char32_t c : detail::utf8_decode({reinterpret_cast<const char*>(bytes.data()), bytes.size()})) {
        text += detail::utf8_encode(c);
    }
    co_return text;
}

asio::awaitable<void> security_result(Reader& rd, std::uint32_t version) {
    if (co_await rd.u32() == 0) co_return;
    if (version == 8) detail::fail(RfbError::auth(co_await read_string(rd)));
    detail::fail(RfbError::auth("the server rejected the credentials"));
}

} // namespace

RfbError RfbError::security(std::vector<std::uint8_t> types) {
    RfbError e(Kind::Security, {});
    e.types_ = std::move(types);
    return e;
}

std::string RfbError::message() const {
    switch (kind_) {
    case Kind::Io: return "i/o: " + detail_;
    case Kind::Timeout: return std::format("connecting to {} timed out", detail_);
    case Kind::Protocol: return "protocol error: " + detail_;
    case Kind::Refused: return "the server refused the connection: " + detail_;
    case Kind::Auth: return "authentication failed: " + detail_;
    case Kind::NoPassword: return "no password available for endpoint " + detail_;
    case Kind::Security: {
        std::string list = "[";
        for (std::size_t i = 0; i < types_.size(); ++i) list += std::format("{}{}", i == 0 ? "" : ", ", types_[i]);
        return "no supported security type among " + list + "]";
    }
    case Kind::Zlib: return "zlib: " + detail_;
    case Kind::Key: return "no X11 keysym for key " + debug_str(detail_);
    case Kind::ForwardButton: return "RFB pointer events have no forward button";
    case Kind::Disconnected: return "not connected, reconnecting";
    }
    return detail_;
}

loa::contracts::surface::SurfaceError RfbError::to_surface_error() const {
    return loa::contracts::surface::SurfaceError::backend(message());
}

std::shared_ptr<PasswordSource> password_fn(std::function<std::optional<std::string>(std::string_view)> fn) {
    return std::make_shared<FnPasswords>(std::move(fn));
}

void Msg::write(std::vector<std::uint8_t>& buf) const {
    switch (type) {
    case Type::SetPixelFormat:
        buf.insert(buf.end(), {0, 0, 0, 0});
        buf.insert(buf.end(), PIXEL_FORMAT.begin(), PIXEL_FORMAT.end());
        break;
    case Type::SetEncodings:
        buf.insert(buf.end(), {2, 0});
        be16(buf, std::uint16_t(encodings.size()));
        for (const std::int32_t e : encodings) be32(buf, std::uint32_t(e));
        break;
    case Type::UpdateRequest:
        buf.insert(buf.end(), {3, std::uint8_t(flag), 0, 0, 0, 0});
        be16(buf, x);
        be16(buf, y);
        break;
    case Type::Key:
        buf.insert(buf.end(), {4, std::uint8_t(flag), 0, 0});
        be32(buf, keysym);
        break;
    case Type::Pointer:
        buf.insert(buf.end(), {5, mask});
        be16(buf, x);
        be16(buf, y);
        break;
    }
}

RfbResult<std::uint32_t> parse_version(std::span<const std::uint8_t> banner) {
    const std::string_view raw(reinterpret_cast<const char*>(banner.data()), banner.size());
    std::string lossy;
    for (const char32_t c : detail::utf8_decode(raw)) lossy += detail::utf8_encode(c);
    const auto bad = [&] { return std::unexpected(RfbError::protocol("bad version banner " + debug_str(lossy))); };
    if (lossy != raw || !raw.starts_with("RFB ") || !raw.ends_with('\n')) return bad();
    const std::string_view rest = raw.substr(4, raw.size() - 5);
    const auto dot = rest.find('.');
    if (dot == std::string_view::npos) return bad();
    const auto number = [](std::string_view s) -> std::optional<std::uint32_t> {
        if (s.starts_with('+')) s.remove_prefix(1);
        std::uint32_t v = 0;
        const auto [end, ec] = std::from_chars(s.data(), s.data() + s.size(), v);
        if (s.empty() || ec != std::errc{} || end != s.data() + s.size()) return std::nullopt;
        return v;
    };
    const auto major = number(rest.substr(0, dot));
    const auto minor = number(rest.substr(dot + 1));
    if (!major || !minor || *major != 3) return bad();
    if (*minor >= 8) return 8U;
    if (*minor == 7) return 7U;
    if (*minor >= 3) return 3U;
    return bad();
}

std::array<std::uint8_t, 16> vnc_response(std::span<const std::uint8_t> password,
                                          const std::array<std::uint8_t, 16>& challenge) {
    DES_cblock key{};
    for (std::size_t i = 0; i < std::min<std::size_t>(8, password.size()); ++i) key[i] = reverse_bits(password[i]);
    DES_key_schedule schedule;
    DES_set_key_unchecked(&key, &schedule);
    std::array<std::uint8_t, 16> out = challenge;
    for (std::size_t at = 0; at < out.size(); at += 8) {
        DES_cblock in;
        DES_cblock block;
        std::memcpy(in, out.data() + at, 8);
        DES_ecb_encrypt(&in, &block, &schedule, DES_ENCRYPT);
        std::memcpy(out.data() + at, block, 8);
    }
    OPENSSL_cleanse(key, sizeof key);
    OPENSSL_cleanse(&schedule, sizeof schedule);
    return out;
}

namespace detail {

std::u32string utf8_decode(std::string_view s) {
    std::u32string out;
    std::size_t i = 0;
    while (i < s.size()) {
        const auto b0 = std::uint8_t(s[i]);
        std::size_t len = 0;
        char32_t c = 0;
        char32_t min = 0;
        if (b0 < 0x80) {
            out.push_back(b0);
            ++i;
            continue;
        }
        if ((b0 & 0xE0U) == 0xC0) { len = 2; c = b0 & 0x1FU; min = 0x80; }
        else if ((b0 & 0xF0U) == 0xE0) { len = 3; c = b0 & 0x0FU; min = 0x800; }
        else if ((b0 & 0xF8U) == 0xF0) { len = 4; c = b0 & 0x07U; min = 0x10000; }
        std::size_t n = 1;
        if (len != 0) {
            for (; n < len && i + n < s.size() && (std::uint8_t(s[i + n]) & 0xC0U) == 0x80; ++n) {
                c = (c << 6U) | (std::uint8_t(s[i + n]) & 0x3FU);
            }
        }
        if (len == 0 || n != len || c < min || c > 0x10FFFF || (c >= 0xD800 && c <= 0xDFFF)) {
            out.push_back(0xFFFD);
            i += std::max<std::size_t>(1, len == 0 ? 1 : n);
            continue;
        }
        out.push_back(c);
        i += len;
    }
    return out;
}

std::string utf8_encode(char32_t c) {
    std::string out;
    const auto u = std::uint32_t(c);
    if (u < 0x80) {
        out += char(u);
    } else if (u < 0x800) {
        out += char(0xC0 | (u >> 6U));
        out += char(0x80 | (u & 0x3FU));
    } else if (u < 0x10000) {
        out += char(0xE0 | (u >> 12U));
        out += char(0x80 | ((u >> 6U) & 0x3FU));
        out += char(0x80 | (u & 0x3FU));
    } else {
        out += char(0xF0 | (u >> 18U));
        out += char(0x80 | ((u >> 12U) & 0x3FU));
        out += char(0x80 | ((u >> 6U) & 0x3FU));
        out += char(0x80 | (u & 0x3FU));
    }
    return out;
}

asio::awaitable<ServerInit> handshake(Reader& rd, const WriteFn& write, const std::string& endpoint,
                                      PasswordSource& passwords, bool shared) {
    std::array<std::uint8_t, 12> banner{};
    co_await rd.read_exact(banner);
    // The protocol minor version spoken: 3, 7 or 8.
    const auto version = parse_version(banner);
    if (!version) fail(version.error());
    const std::string answer = std::format("RFB 003.{:03}\n", *version);
    co_await write({reinterpret_cast<const std::uint8_t*>(answer.data()), answer.size()});

    std::uint8_t security = 0;
    if (*version == 3) {
        const std::uint32_t t = co_await rd.u32();
        if (t == SEC_INVALID) fail(RfbError::refused(co_await read_string(rd)));
        if (t > 0xFF) fail(RfbError::security({}));
        security = std::uint8_t(t);
    } else {
        const std::uint8_t count = co_await rd.u8();
        if (count == 0) fail(RfbError::refused(co_await read_string(rd)));
        std::vector<std::uint8_t> types(count);
        co_await rd.read_exact(types);
        const auto has = [&](std::uint8_t t) { return std::ranges::find(types, t) != types.end(); };
        if (has(SEC_NONE)) {
            security = SEC_NONE;
        } else if (has(SEC_VNC_AUTH)) {
            security = SEC_VNC_AUTH;
        } else {
            fail(RfbError::security(std::move(types)));
        }
        const std::array<std::uint8_t, 1> chosen{security};
        co_await write(chosen);
    }

    if (security == SEC_NONE) {
        if (*version == 8) co_await security_result(rd, *version);
    } else if (security == SEC_VNC_AUTH) {
        std::array<std::uint8_t, 16> challenge{};
        co_await rd.read_exact(challenge);
        auto password = co_await passwords.password(endpoint);
        if (!password) fail(RfbError::no_password(endpoint));
        const auto response = vnc_response({reinterpret_cast<const std::uint8_t*>(password->data()), password->size()}, challenge);
        OPENSSL_cleanse(password->data(), password->size());
        password.reset();
        co_await write(response);
        co_await security_result(rd, *version);
    } else {
        fail(RfbError::security({security}));
    }

    const std::array<std::uint8_t, 1> client_init{std::uint8_t(shared)};
    co_await write(client_init);
    ServerInit init;
    init.width = co_await rd.u16();
    init.height = co_await rd.u16();
    std::array<std::uint8_t, 16> server_format{};
    co_await rd.read_exact(server_format);
    init.name = co_await read_string(rd);

    std::vector<std::uint8_t> setup;
    setup.reserve(64);
    const auto list = encodings();
    Msg::set_pixel_format().write(setup);
    Msg::set_encodings(list).write(setup);
    Msg::update_request(false, init.width, init.height).write(setup);
    co_await write(setup);
    co_return init;
}

} // namespace detail

} // namespace loa::surfaces::vnc
