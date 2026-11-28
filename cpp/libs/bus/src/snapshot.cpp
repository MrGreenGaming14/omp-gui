#include "loa/bus/snapshot.hpp"

#include <algorithm>
#include <array>
#include <cerrno>
#include <charconv>
#include <format>
#include <fstream>
#include <string_view>

#include "journal_io.hpp"
#include "loa/bus/frame.hpp"
#include "loa/bus/proto.hpp"
#include "loa/runtime/fs.hpp"

namespace loa::bus::snapshot {

namespace {

constexpr frame::Magic magic{'L', 'O', 'A', 'S', 'N', 'A', 'P', 0};
constexpr std::uint32_t version = 1;
constexpr std::size_t header_len = 52;
constexpr std::string_view prefix = "snap-";
constexpr std::string_view suffix = ".loas";

JournalError corrupt(const std::filesystem::path& path, std::string reason) {
    return journal_error::Corrupt{path, 0, std::move(reason)};
}

std::expected<std::optional<Meta>, JournalError> read_header(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::unexpected(detail::io_error(path, errno));
    std::array<std::uint8_t, header_len> h{};
    if (!in.read(reinterpret_cast<char*>(h.data()), h.size())) {
        if (in.eof()) return std::nullopt;
        return std::unexpected(detail::io_error(path, EIO));
    }
    const auto pre = frame::preamble(magic, version);
    if (!std::equal(pre.begin(), pre.end(), h.begin()) ||
        frame::le_u32(h, 48) != frame::crc32(std::span(h).first(48)))
        return std::nullopt;
    Meta meta{path,
              frame::le_u64(h, 12),
              frame::le_u64(h, 20),
              frame::le_u64(h, 28),
              frame::le_u64(h, 36),
              frame::le_u32(h, 44)};
    if (meta.seq > 0 && meta.prev_offset < meta.offset) return meta;
    return std::nullopt;
}

} // namespace

std::string file_name(std::uint64_t seq) { return std::format("{}{:020}{}", prefix, seq, suffix); }

std::expected<std::vector<Meta>, JournalError> list(const std::filesystem::path& dir) {
    std::vector<Meta> out;
    std::error_code ec;
    std::filesystem::directory_iterator it(dir, ec), end;
    if (ec) return std::unexpected(detail::io_error(dir, ec.value()));
    for (; it != end; it.increment(ec)) {
        if (ec) return std::unexpected(detail::io_error(dir, ec.value()));
        const std::string name = it->path().filename().string();
        if (!name.starts_with(prefix) || !name.ends_with(suffix) || name.size() < prefix.size() + suffix.size())
            continue;
        const std::string_view digits(name.data() + prefix.size(), name.size() - prefix.size() - suffix.size());
        std::uint64_t seq = 0;
        const auto [ptr, err] = std::from_chars(digits.data(), digits.data() + digits.size(), seq);
        if (digits.empty() || err != std::errc{} || ptr != digits.data() + digits.size()) continue;
        auto meta = read_header(it->path());
        if (!meta) return std::unexpected(meta.error());
        if (*meta) out.push_back(std::move(**meta));
    }
    if (ec) return std::unexpected(detail::io_error(dir, ec.value()));
    std::stable_sort(out.begin(), out.end(), [](const Meta& a, const Meta& b) { return a.seq < b.seq; });
    return out;
}

std::expected<core::Dom, JournalError> read_dom(const Meta& meta) {
    const auto& path = meta.path;
    std::ifstream in(path, std::ios::binary | std::ios::ate);
    if (!in) return std::unexpected(detail::io_error(path, errno));
    const auto len = static_cast<std::uint64_t>(in.tellg());
    if (len != header_len + meta.body_len) return std::unexpected(corrupt(path, "body length doesn't match the file"));
    frame::Bytes body(meta.body_len);
    in.seekg(static_cast<std::streamoff>(header_len));
    if (!in.read(reinterpret_cast<char*>(body.data()), static_cast<std::streamsize>(body.size())))
        return std::unexpected(corrupt(path, "file shorter than its header"));
    if (frame::crc32(body) != meta.body_crc) return std::unexpected(corrupt(path, "body checksum mismatch"));
    auto dom = proto::decode_dom(body);
    if (!dom) return std::unexpected(detail::proto_error(proto::to_error(dom.error())));
    if (dom->seq() != meta.seq) return std::unexpected(corrupt(path, "body seq differs from header"));
    return std::move(*dom);
}

std::expected<Meta, JournalError> write(const std::filesystem::path& dir, const core::Dom& dom, std::uint64_t offset,
                                        std::uint64_t prev_offset) {
    const auto body = proto::encode_dom(dom);
    const std::uint64_t body_len = body.size();
    const std::uint32_t body_crc = frame::crc32(body);
    const std::uint64_t seq = dom.seq();
    const auto pre = frame::preamble(magic, version);
    frame::Bytes buf(pre.begin(), pre.end());
    buf.reserve(header_len + body.size());
    const auto put_le = [&](std::uint64_t v, int bytes) {
        for (int k = 0; k < bytes; ++k) buf.push_back(static_cast<std::uint8_t>(v >> (8 * k)));
    };
    for (std::uint64_t field : {seq, offset, prev_offset, body_len}) put_le(field, 8);
    put_le(body_crc, 4);
    put_le(frame::crc32(buf), 4);
    buf.insert(buf.end(), body.begin(), body.end());

    const auto path = dir / file_name(seq);
    auto ok = loa::runtime::fs::write_atomic(
        path, std::string_view(reinterpret_cast<const char*>(buf.data()), buf.size()));
    if (!ok) return std::unexpected(detail::io_error(path, ok.error()));
    return Meta{path, seq, offset, prev_offset, body_len, body_crc};
}

} // namespace loa::bus::snapshot
