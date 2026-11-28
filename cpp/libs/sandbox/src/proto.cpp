#include "loa/sandbox/proto.hpp"

#include <format>

#include "loa/sandbox/channel.hpp"

namespace loa::sandbox::proto {

namespace {

// postcard: unsigned integers above u8 are LEB128 varints, signed ones zigzag first, enum
// variants a varint index, options a 0/1 tag, sequences a varint length.
struct Writer {
    Bytes out;

    void varint(std::uint64_t v) {
        while (v >= 0x80) {
            out.push_back(static_cast<char>(v | 0x80));
            v >>= 7;
        }
        out.push_back(static_cast<char>(v));
    }
    void i32(std::int32_t v) { varint((static_cast<std::uint32_t>(v) << 1) ^ static_cast<std::uint32_t>(v >> 31)); }
    void boolean(bool v) { out.push_back(v ? 1 : 0); }
    void bytes(std::string_view b) {
        varint(b.size());
        out.append(b);
    }
    template <typename T, typename F>
    void option(const std::optional<T>& v, F&& put) {
        boolean(v.has_value());
        if (v) put(*v);
    }
};

loa::Error invalid(std::string_view what) {
    return loa::Error(ErrorCode::parse, std::format("sandbox protocol: {}", what));
}

struct Reader {
    std::string_view in;

    Result<std::uint64_t> varint(int max_bytes, std::uint64_t max) {
        std::uint64_t v = 0;
        for (int i = 0; i < max_bytes; ++i) {
            if (in.empty()) return std::unexpected(invalid("Hit the end of buffer, expected more data"));
            const auto byte = static_cast<std::uint8_t>(in.front());
            in.remove_prefix(1);
            v |= static_cast<std::uint64_t>(byte & 0x7f) << (7 * i);
            if (!(byte & 0x80)) {
                if (v > max) break;
                return v;
            }
        }
        return std::unexpected(invalid("Found a bad varint"));
    }
    Result<std::uint32_t> u32() {
        auto v = varint(5, 0xffffffffu);
        if (!v) return std::unexpected(v.error());
        return static_cast<std::uint32_t>(*v);
    }
    Result<std::uint64_t> u64() { return varint(10, ~std::uint64_t{0}); }
    Result<std::int32_t> i32() {
        auto v = u32();
        if (!v) return std::unexpected(v.error());
        return static_cast<std::int32_t>(*v >> 1) ^ -static_cast<std::int32_t>(*v & 1);
    }
    Result<bool> boolean() {
        if (in.empty()) return std::unexpected(invalid("Hit the end of buffer, expected more data"));
        const auto byte = static_cast<std::uint8_t>(in.front());
        in.remove_prefix(1);
        if (byte > 1) return std::unexpected(invalid("Found a bool that wasn't 0 or 1"));
        return byte == 1;
    }
    Result<std::string> bytes() {
        auto len = varint(10, ~std::uint64_t{0});
        if (!len) return std::unexpected(len.error());
        if (*len > in.size()) return std::unexpected(invalid("Hit the end of buffer, expected more data"));
        std::string out(in.substr(0, *len));
        in.remove_prefix(*len);
        return out;
    }
    Result<std::string> string() {
        auto s = bytes();
        if (s && !valid_utf8(*s)) return std::unexpected(invalid("Found invalid UTF-8"));
        return s;
    }
    template <typename T, typename F>
    Result<std::optional<T>> option(F&& get) {
        auto tag = boolean();
        if (!tag) return std::unexpected(invalid("Found an Option discriminant that wasn't 0 or 1"));
        if (!*tag) return std::optional<T>{};
        auto v = get();
        if (!v) return std::unexpected(v.error());
        return std::optional<T>(std::move(*v));
    }

    static bool valid_utf8(std::string_view s) {
        std::size_t i = 0;
        while (i < s.size()) {
            const auto c = static_cast<std::uint8_t>(s[i]);
            std::size_t n = 0;
            std::uint32_t cp = 0;
            if (c < 0x80) {
                ++i;
                continue;
            } else if ((c & 0xe0) == 0xc0) {
                n = 1, cp = c & 0x1f;
            } else if ((c & 0xf0) == 0xe0) {
                n = 2, cp = c & 0x0f;
            } else if ((c & 0xf8) == 0xf0) {
                n = 3, cp = c & 0x07;
            } else {
                return false;
            }
            if (i + n >= s.size()) return false;
            for (std::size_t k = 1; k <= n; ++k) {
                const auto cc = static_cast<std::uint8_t>(s[i + k]);
                if ((cc & 0xc0) != 0x80) return false;
                cp = (cp << 6) | (cc & 0x3f);
            }
            if ((n == 1 && cp < 0x80) || (n == 2 && cp < 0x800) || (n == 3 && cp < 0x10000) || cp > 0x10ffff ||
                (cp >= 0xd800 && cp <= 0xdfff))
                return false;
            i += n + 1;
        }
        return true;
    }
};

#define LOA_TRY(var, expr)                                         \
    auto var = (expr);                                             \
    if (!var) return std::unexpected(std::move(var).error())

Result<ExecRequest> read_exec(Reader& r) {
    ExecRequest exec;
    LOA_TRY(argc, r.varint(10, ~std::uint64_t{0}));
    for (std::uint64_t i = 0; i < *argc; ++i) {
        LOA_TRY(arg, r.string());
        exec.argv.push_back(std::move(*arg));
    }
    LOA_TRY(cwd, r.option<std::string>([&] { return r.string(); }));
    exec.cwd = std::move(*cwd);
    LOA_TRY(envc, r.varint(10, ~std::uint64_t{0}));
    for (std::uint64_t i = 0; i < *envc; ++i) {
        LOA_TRY(key, r.string());
        LOA_TRY(value, r.string());
        exec.env.emplace_back(std::move(*key), std::move(*value));
    }
    LOA_TRY(in, r.option<std::string>([&] { return r.bytes(); }));
    exec.stdin_data = std::move(*in);
    LOA_TRY(timeout, r.option<std::uint64_t>([&] { return r.u64(); }));
    exec.timeout_ms = *timeout;
    return exec;
}

Bytes framed(Bytes body) {
    const auto len = static_cast<std::uint32_t>(body.size());
    Bytes out;
    out.reserve(body.size() + 4);
    for (int i = 0; i < 4; ++i) out.push_back(static_cast<char>((len >> (8 * i)) & 0xff));
    out += body;
    return out;
}

std::string debug_bytes(std::string_view data) {
    std::string out = "[";
    for (std::size_t i = 0; i < data.size(); ++i) {
        if (i) out += ", ";
        out += std::to_string(static_cast<std::uint8_t>(data[i]));
    }
    return out + "]";
}

std::string debug_opt(const std::optional<std::int32_t>& v) {
    return v ? std::format("Some({})", *v) : std::string("None");
}

std::string debug_str(std::string_view s) {
    std::string out = "\"";
    for (char c : s) {
        if (c == '"' || c == '\\') out += '\\';
        out += c;
    }
    return out + "\"";
}

} // namespace

ExecRequest ExecRequest::shell(std::string_view script, const std::vector<std::string>& args) {
    ExecRequest exec;
    exec.argv = {"/bin/sh", "-c", std::string(script), "sh"};
    exec.argv.insert(exec.argv.end(), args.begin(), args.end());
    return exec;
}

std::optional<std::uint32_t> response_id(const Response& response) noexcept {
    return std::visit(
        [](const auto& r) -> std::optional<std::uint32_t> {
            if constexpr (std::is_same_v<std::decay_t<decltype(r)>, response::Ready>) return std::nullopt;
            else return r.id;
        },
        response);
}

std::string debug(const Response& response) {
    return std::visit(
        [](const auto& r) -> std::string {
            using T = std::decay_t<decltype(r)>;
            if constexpr (std::is_same_v<T, response::Ready>) return std::format("Ready {{ version: {} }}", r.version);
            else if constexpr (std::is_same_v<T, response::Stdout>) return std::format("Stdout {{ id: {}, data: {} }}", r.id, debug_bytes(r.data));
            else if constexpr (std::is_same_v<T, response::Stderr>) return std::format("Stderr {{ id: {}, data: {} }}", r.id, debug_bytes(r.data));
            else if constexpr (std::is_same_v<T, response::Exit>)
                return std::format("Exit {{ id: {}, status: ExitStatus {{ code: {}, signal: {}, timed_out: {} }} }}", r.id,
                                   debug_opt(r.status.code), debug_opt(r.status.signal), r.status.timed_out);
            else return std::format("Error {{ id: {}, message: {} }}", r.id, debug_str(r.message));
        },
        response);
}

Bytes encode(const Request& request) {
    Writer w;
    w.varint(request.index());
    if (const auto* exec = std::get_if<request::Exec>(&request)) {
        w.varint(exec->id);
        const auto& e = exec->exec;
        w.varint(e.argv.size());
        for (const auto& arg : e.argv) w.bytes(arg);
        w.option(e.cwd, [&](const std::string& s) { w.bytes(s); });
        w.varint(e.env.size());
        for (const auto& [key, value] : e.env) {
            w.bytes(key);
            w.bytes(value);
        }
        w.option(e.stdin_data, [&](const std::string& s) { w.bytes(s); });
        w.option(e.timeout_ms, [&](std::uint64_t v) { w.varint(v); });
    }
    return std::move(w.out);
}

Bytes encode(const Response& response) {
    Writer w;
    w.varint(response.index());
    std::visit(
        [&](const auto& r) {
            using T = std::decay_t<decltype(r)>;
            if constexpr (std::is_same_v<T, response::Ready>) {
                w.varint(r.version);
            } else {
                w.varint(r.id);
                if constexpr (std::is_same_v<T, response::Stdout> || std::is_same_v<T, response::Stderr>) {
                    w.bytes(r.data);
                } else if constexpr (std::is_same_v<T, response::Exit>) {
                    w.option(r.status.code, [&](std::int32_t v) { w.i32(v); });
                    w.option(r.status.signal, [&](std::int32_t v) { w.i32(v); });
                    w.boolean(r.status.timed_out);
                } else {
                    w.bytes(r.message);
                }
            }
        },
        response);
    return std::move(w.out);
}

Bytes frame(const Request& request) { return framed(encode(request)); }
Bytes frame(const Response& response) { return framed(encode(response)); }

std::expected<Request, loa::Error> decode_request(std::string_view body) {
    Reader r{body};
    LOA_TRY(variant, r.u32());
    switch (*variant) {
    case 0: {
        LOA_TRY(id, r.u32());
        LOA_TRY(exec, read_exec(r));
        return request::Exec{*id, std::move(*exec)};
    }
    case 1:
        return request::Shutdown{};
    default:
        return std::unexpected(invalid("Found an enum discriminant that was out of range"));
    }
}

std::expected<Response, loa::Error> decode_response(std::string_view body) {
    Reader r{body};
    LOA_TRY(variant, r.u32());
    if (*variant > 4) return std::unexpected(invalid("Found an enum discriminant that was out of range"));
    LOA_TRY(first, r.u32());
    switch (*variant) {
    case 0:
        return response::Ready{*first};
    case 1:
    case 2: {
        LOA_TRY(data, r.bytes());
        if (*variant == 1) return response::Stdout{*first, std::move(*data)};
        return response::Stderr{*first, std::move(*data)};
    }
    case 3: {
        ExitStatus status;
        LOA_TRY(code, r.option<std::int32_t>([&] { return r.i32(); }));
        LOA_TRY(signal, r.option<std::int32_t>([&] { return r.i32(); }));
        LOA_TRY(timed_out, r.boolean());
        status.code = *code;
        status.signal = *signal;
        status.timed_out = *timed_out;
        return response::Exit{*first, status};
    }
    default: {
        LOA_TRY(message, r.string());
        return response::Error{*first, std::move(*message)};
    }
    }
}

namespace {
// Rust `read_exact`: false when the stream ended after `got` bytes.
Result<std::size_t> read_exact(ByteReader& reader, char* buf, std::size_t len) {
    std::size_t got = 0;
    while (got < len) {
        auto n = reader.read({buf + got, len - got});
        if (!n) return std::unexpected(n.error());
        if (*n == 0) break;
        got += *n;
    }
    return got;
}
} // namespace

std::expected<bool, loa::Error> read_frame(ByteReader& reader, Bytes& buf) {
    char header[4];
    LOA_TRY(got, read_exact(reader, header, 4));
    if (*got < 4) return false;
    std::uint32_t len = 0;
    for (int i = 0; i < 4; ++i) len |= static_cast<std::uint32_t>(static_cast<std::uint8_t>(header[i])) << (8 * i);
    if (len > MAX_FRAME) return std::unexpected(loa::Error(ErrorCode::parse, std::format("frame of {} bytes is over the limit", len)));
    buf.resize(len);
    LOA_TRY(body, read_exact(reader, buf.data(), len));
    if (*body < len) return std::unexpected(loa::Error(ErrorCode::io, "failed to fill whole buffer"));
    return true;
}

std::expected<bool, loa::Error> skip_to_magic(ByteReader& reader) {
    std::size_t matched = 0;
    char byte = 0;
    while (matched < MAGIC.size()) {
        auto n = reader.read({&byte, 1});
        if (!n) return std::unexpected(n.error());
        if (*n == 0) return false;
        // MAGIC starts with a NUL found nowhere else in it, so a mismatch can only restart
        // the match at this byte.
        if (MAGIC[matched] == byte) ++matched;
        else matched = byte == MAGIC[0] ? 1 : 0;
    }
    return true;
}

std::expected<void, loa::Error> write_hello(ByteWriter& writer) {
    Bytes hello(MAGIC);
    hello += frame(Response{response::Ready{VERSION}});
    return writer.write_all(hello);
}

} // namespace loa::sandbox::proto
