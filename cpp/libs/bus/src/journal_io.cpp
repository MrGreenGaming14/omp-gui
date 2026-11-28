#include "journal_io.hpp"

#include <fcntl.h>
#include <sys/file.h>
#include <unistd.h>

#include <cerrno>
#include <chrono>
#include <cstring>
#include <format>
#include <thread>

#include "loa/bus/proto.hpp"
#include "loa/runtime/proto.hpp"

namespace loa::bus::detail {

namespace wire = loa::runtime::proto;
namespace je = journal_error;

namespace {

constexpr frame::Magic journal_magic{'L', 'O', 'A', 'J', 'R', 'N', 'L', 0};
// Largest encoded header accepted.
constexpr std::uint32_t header_max_len = 64 * 1024;
// How long `lock` keeps trying a held lock before reporting it locked.
constexpr auto lock_wait = std::chrono::milliseconds(100);

} // namespace

Fd& Fd::operator=(Fd&& o) noexcept {
    if (this != &o) {
        if (fd_ >= 0) ::close(fd_);
        fd_ = std::exchange(o.fd_, -1);
    }
    return *this;
}

Fd::~Fd() {
    if (fd_ >= 0) ::close(fd_);
}

JournalError io_error(const std::filesystem::path& path, int err) {
    return je::Io{path, std::format("{} (os error {})", std::strerror(err), err)};
}

JournalError io_error(const std::filesystem::path& path, const loa::Error& err) {
    return je::Io{path, err.to_string()};
}

JournalError proto_error(const loa::Error& err) { return je::Proto{err}; }

JournalResult<Fd> lock(const std::filesystem::path& dir) {
    const auto path = dir / "lock";
    Fd file(::open(path.c_str(), O_WRONLY | O_CREAT | O_CLOEXEC, 0666));
    if (!file) return std::unexpected(io_error(path, errno));
    const auto deadline = std::chrono::steady_clock::now() + lock_wait;
    for (;;) {
        if (::flock(file.get(), LOCK_EX | LOCK_NB) == 0) return file;
        const int err = errno;
        if (err == EINTR) continue;
        if (err != EWOULDBLOCK) return std::unexpected(io_error(path, err));
        if (std::chrono::steady_clock::now() >= deadline) return std::unexpected(JournalError(je::Locked{dir}));
        std::this_thread::sleep_for(std::chrono::milliseconds(5));
    }
}

int truncate_file(const std::filesystem::path& path, std::uint64_t len) {
    Fd file(::open(path.c_str(), O_WRONLY | O_CLOEXEC));
    if (!file) return errno;
    if (::ftruncate(file.get(), static_cast<off_t>(len)) != 0) return errno;
    if (::fsync(file.get()) != 0) return errno;
    return 0;
}

frame::Bytes write_header(const Header& h) {
    const auto pre = frame::preamble(journal_magic, FORMAT_VERSION);
    frame::Bytes bytes(pre.begin(), pre.end());
    // A header is far below the record limit.
    (void)frame::put(bytes, [&](frame::Bytes& b) {
        if (!h.session_id.empty()) {
            wire::put_tag(b, 1, wire::WireType::len);
            wire::put_len(b, std::string_view(h.session_id));
        }
        if (h.parent_session) {
            wire::put_tag(b, 2, wire::WireType::len);
            wire::put_len(b, std::string_view(*h.parent_session));
        }
        if (h.forked_at) {
            wire::put_tag(b, 3, wire::WireType::varint);
            wire::put_varint(b, *h.forked_at);
        }
    });
    return bytes;
}

namespace {

std::optional<Header> decode_header(std::span<const std::uint8_t> bytes) {
    Header h;
    wire::Reader r(bytes);
    while (!r.done()) {
        auto tag = r.tag();
        if (!tag) return std::nullopt;
        if ((tag->field == 1 || tag->field == 2) && tag->type == wire::WireType::len) {
            auto s = r.len();
            if (!s) return std::nullopt;
            std::string v(s->begin(), s->end());
            if (tag->field == 1) h.session_id = std::move(v);
            else h.parent_session = std::move(v);
        } else if (tag->field == 3 && tag->type == wire::WireType::varint) {
            auto v = r.varint();
            if (!v) return std::nullopt;
            h.forked_at = *v;
        } else if (tag->field >= 1 && tag->field <= 3) {
            return std::nullopt;  // wrong wire type
        } else if (!r.skip(*tag)) {
            return std::nullopt;
        }
    }
    return h;
}

} // namespace

JournalResult<std::pair<Header, std::uint64_t>> read_header(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::unexpected(io_error(path, errno));
    std::array<std::uint8_t, frame::preamble_len + frame::header_len> fixed{};
    if (!in.read(reinterpret_cast<char*>(fixed.data()), fixed.size())) {
        if (in.eof()) return std::unexpected(JournalError(je::NotAJournal{path}));
        return std::unexpected(io_error(path, errno));
    }
    if (!std::equal(journal_magic.begin(), journal_magic.end(), fixed.begin()))
        return std::unexpected(JournalError(je::NotAJournal{path}));
    const std::uint32_t found = frame::le_u32(fixed, 8);
    if (found != FORMAT_VERSION)
        return std::unexpected(JournalError(je::UnsupportedVersion{path, found, FORMAT_VERSION}));
    const auto corrupt = [&] { return std::unexpected(JournalError(je::CorruptHeader{path})); };
    const std::uint32_t len = frame::le_u32(fixed, frame::preamble_len);
    if (len > header_max_len) return corrupt();
    frame::Bytes bytes(len);
    if (!in.read(reinterpret_cast<char*>(bytes.data()), len)) return corrupt();
    if (frame::crc32(bytes) != frame::le_u32(fixed, frame::preamble_len + 4)) return corrupt();
    auto header = decode_header(bytes);
    if (!header) return corrupt();
    return std::pair{std::move(*header), std::uint64_t{fixed.size() + bytes.size()}};
}

JournalResult<void> encode_record(const core::Patch& patch, frame::Bytes& buf) {
    buf.clear();
    auto ok = frame::put(buf, [&](frame::Bytes& b) { proto::encode(patch, b); });
    if (!ok) return std::unexpected(JournalError(je::TooLarge{ok.error()}));
    return {};
}

JournalResult<std::unique_ptr<RecordReader>> RecordReader::open(const std::filesystem::path& path,
                                                                std::uint64_t offset, std::uint64_t end,
                                                                std::uint64_t next_seq) {
    std::unique_ptr<RecordReader> r(new RecordReader());
    r->path_ = path;
    constexpr std::size_t cap = 64 * 1024;
    r->iobuf_ = std::make_unique<char[]>(cap);
    r->in_.rdbuf()->pubsetbuf(r->iobuf_.get(), cap);
    r->in_.open(path, std::ios::binary);
    if (!r->in_) return std::unexpected(io_error(path, errno));
    if (!r->in_.seekg(static_cast<std::streamoff>(offset))) return std::unexpected(io_error(path, EIO));
    r->offset_ = offset;
    r->end_ = end;
    r->next_seq_ = next_seq;
    return r;
}

JournalError RecordReader::corrupt(std::string reason) const { return je::Corrupt{path_, offset_, std::move(reason)}; }

JournalResult<frame::Frame> RecordReader::read() {
    auto f = frame::read(in_, end_ - offset_, buf_);
    if (!f) return std::unexpected(io_error(path_, f.error()));
    return *f;
}

JournalResult<core::Patch> RecordReader::accept(std::uint32_t len) {
    auto patch = proto::decode_patch(buf_);
    if (!patch) return std::unexpected(corrupt(proto::to_string(patch.error())));
    if (patch->seq != next_seq_)
        return std::unexpected(corrupt(std::format("expected seq {}, found {}", next_seq_, patch->seq)));
    offset_ += frame::header_len + len;
    ++next_seq_;
    return std::move(*patch);
}

JournalResult<std::optional<core::Patch>> RecordReader::next_patch() {
    if (offset_ >= end_) return std::nullopt;
    auto f = read();
    if (!f) return std::unexpected(f.error());
    switch (f->kind) {
    case frame::Kind::whole: {
        auto p = accept(f->len);
        if (!p) return std::unexpected(p.error());
        return std::optional<core::Patch>(std::move(*p));
    }
    case frame::Kind::short_: return std::unexpected(corrupt("record runs past the end"));
    case frame::Kind::bad_len: return std::unexpected(corrupt(std::format("bad record length {}", f->len)));
    case frame::Kind::bad_crc: break;
    }
    return std::unexpected(corrupt("checksum mismatch"));
}

JournalResult<core::Patch> RecordReader::expect_patch(std::uint64_t target) {
    auto p = next_patch();
    if (!p) return std::unexpected(p.error());
    if (!*p) return std::unexpected(corrupt(std::format("journal ends before seq {}", target)));
    return std::move(**p);
}

namespace {

// True when every byte from `offset` to the end of the file is zero.
JournalResult<bool> zeros_from(const std::filesystem::path& path, std::uint64_t offset) {
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::unexpected(io_error(path, errno));
    in.seekg(static_cast<std::streamoff>(offset));
    std::array<char, 8192> chunk{};
    while (in) {
        in.read(chunk.data(), chunk.size());
        const auto n = static_cast<std::size_t>(in.gcount());
        for (std::size_t i = 0; i < n; ++i)
            if (chunk[i] != 0) return false;
    }
    if (in.bad()) return std::unexpected(io_error(path, EIO));
    return true;
}

} // namespace

JournalResult<Scan> scan(const std::filesystem::path& path, std::uint64_t file_len, std::uint64_t start,
                         std::uint64_t next_seq) {
    auto r = RecordReader::open(path, start, file_len, next_seq);
    if (!r) return std::unexpected(r.error());
    auto& reader = **r;
    Scan out{start, next_seq - 1, std::nullopt, {}};
    while (reader.offset() < file_len) {
        const std::uint64_t off = reader.offset();
        auto f = reader.read();
        if (!f) return std::unexpected(f.error());
        std::uint64_t tail_from = 0;
        std::string reason;
        switch (f->kind) {
        case frame::Kind::whole: {
            auto patch = reader.accept(f->len);
            if (!patch) return std::unexpected(patch.error());
            if ((patch->seq - 1) % frame::checkpoint_every == 0) out.checkpoints.emplace_back(patch->seq, off);
            out.end = reader.offset();
            out.last_seq = patch->seq;
            out.last_record = off;
            continue;
        }
        case frame::Kind::short_: return out;
        // A bad length tells nothing about where the record ends, so only
        // zeros from its start count as a torn write.
        case frame::Kind::bad_len:
            tail_from = off;
            reason = std::format("bad record length {}", f->len);
            break;
        case frame::Kind::bad_crc:
            tail_from = off + frame::header_len + f->len;
            reason = "checksum mismatch";
            break;
        }
        auto zeros = zeros_from(path, tail_from);
        if (!zeros) return std::unexpected(zeros.error());
        if (*zeros) return out;
        return std::unexpected(reader.corrupt(std::move(reason)));
    }
    return out;
}

JournalResult<std::optional<std::pair<std::uint64_t, std::uint64_t>>> probe(const std::filesystem::path& path,
                                                                             std::uint64_t offset,
                                                                             std::uint64_t file_len) {
    if (offset >= file_len) return std::nullopt;
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::unexpected(io_error(path, errno));
    if (!in.seekg(static_cast<std::streamoff>(offset))) return std::unexpected(io_error(path, EIO));
    frame::Bytes buf;
    auto f = frame::read(in, file_len - offset, buf);
    if (!f) return std::unexpected(io_error(path, f.error()));
    if (f->kind != frame::Kind::whole) return std::nullopt;
    auto patch = proto::decode_patch(buf);
    if (!patch) return std::nullopt;
    return std::pair{patch->seq, offset + frame::header_len + f->len};
}

JournalResult<Recovered> recover(const std::filesystem::path& dir) {
    const auto path = dir / JOURNAL_FILE;
    auto header = read_header(path);
    if (!header) return std::unexpected(header.error());
    const std::uint64_t data_start = header->second;
    std::error_code ec;
    const std::uint64_t file_len = std::filesystem::file_size(path, ec);
    if (ec) return std::unexpected(io_error(path, ec.value()));
    auto metas = snapshot::list(dir);
    if (!metas) return std::unexpected(metas.error());
    std::vector<snapshot::Meta> snapshots;
    for (auto& meta : *metas) {
        // A snapshot counts only when the record it says it follows is
        // really there and ends where the snapshot resumes.
        if (meta.prev_offset < data_start) continue;
        auto p = probe(path, meta.prev_offset, file_len);
        if (!p) return std::unexpected(p.error());
        if (*p == std::pair{meta.seq, meta.offset}) snapshots.push_back(std::move(meta));
    }
    const std::uint64_t start = snapshots.empty() ? data_start : snapshots.back().offset;
    const std::uint64_t next_seq = snapshots.empty() ? 1 : snapshots.back().seq + 1;
    auto s = scan(path, file_len, start, next_seq);
    if (!s) return std::unexpected(s.error());
    std::map<std::uint64_t, std::uint64_t> checkpoints{{1, data_start}};
    for (const auto& m : snapshots) checkpoints.insert_or_assign(m.seq + 1, m.offset);
    for (const auto& [seq, off] : s->checkpoints) checkpoints.insert_or_assign(seq, off);
    auto& h = header->first;
    std::optional<Parent> parent;
    if (h.parent_session && h.forked_at) parent = Parent{std::move(*h.parent_session), *h.forked_at};
    else if (h.parent_session || h.forked_at) return std::unexpected(JournalError(je::CorruptHeader{path}));
    std::optional<std::uint64_t> last_record = s->last_record;
    if (!last_record && !snapshots.empty()) last_record = snapshots.back().prev_offset;
    return Recovered{std::move(h.session_id), std::move(parent), data_start, file_len, s->end, s->last_seq,
                     last_record, std::move(checkpoints), std::move(snapshots)};
}

} // namespace loa::bus::detail
