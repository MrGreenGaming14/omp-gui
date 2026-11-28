#pragma once

// File access shared by the journal, its snapshots and the store (the parts of
// `crates/loa-bus/src/frame.rs` that touch files).

#include <cstdint>
#include <filesystem>
#include <fstream>
#include <memory>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "loa/bus/journal.hpp"

namespace loa::bus::detail {

// `Io` for the OS error `err` at `path`, displayed as Rust's `io::Error`.
[[nodiscard]] JournalError io_error(const std::filesystem::path& path, int err);
[[nodiscard]] JournalError io_error(const std::filesystem::path& path, const loa::Error& err);
[[nodiscard]] JournalError proto_error(const loa::Error& err);

// The session's exclusive lock (`flock` on `dir/lock`), retried briefly so a
// forked child that has not exec'd yet doesn't read as another writer.
[[nodiscard]] JournalResult<Fd> lock(const std::filesystem::path& dir);

// Truncates `path` to `len` and fsyncs it. Returns errno on failure.
[[nodiscard]] int truncate_file(const std::filesystem::path& path, std::uint64_t len);

// The journal header record.
struct Header {
    std::string session_id;
    std::optional<std::string> parent_session;
    std::optional<std::uint64_t> forked_at;
};

// Preamble plus the framed header record.
[[nodiscard]] frame::Bytes write_header(const Header& h);
// Reads the header of the journal at `path` and the offset after it.
[[nodiscard]] JournalResult<std::pair<Header, std::uint64_t>> read_header(const std::filesystem::path& path);

// Encodes `patch` as one framed record into `buf`, replacing its contents.
[[nodiscard]] JournalResult<void> encode_record(const core::Patch& patch, frame::Bytes& buf);

// Reads records forward from a known record boundary up to `end`.
class RecordReader {
  public:
    // A reader at `offset`, which must start the record with sequence `next_seq`.
    [[nodiscard]] static JournalResult<std::unique_ptr<RecordReader>> open(const std::filesystem::path& path,
                                                                           std::uint64_t offset, std::uint64_t end,
                                                                           std::uint64_t next_seq);

    [[nodiscard]] std::uint64_t offset() const noexcept { return offset_; }
    [[nodiscard]] std::uint64_t next_seq() const noexcept { return next_seq_; }
    [[nodiscard]] std::uint64_t end() const noexcept { return end_; }

    [[nodiscard]] JournalError corrupt(std::string reason) const;
    // One frame within the room left.
    [[nodiscard]] JournalResult<frame::Frame> read();
    // Decodes the whole record of `len` bytes just read and steps past it.
    [[nodiscard]] JournalResult<core::Patch> accept(std::uint32_t len);
    // Reads, checks and decodes the next record. None at `end`.
    [[nodiscard]] JournalResult<std::optional<core::Patch>> next_patch();
    // The next patch, which must be there for the journal to reach `target`.
    [[nodiscard]] JournalResult<core::Patch> expect_patch(std::uint64_t target);

  private:
    RecordReader() = default;
    std::filesystem::path path_;
    std::unique_ptr<char[]> iobuf_;
    std::ifstream in_;
    std::uint64_t offset_ = 0;
    std::uint64_t end_ = 0;
    std::uint64_t next_seq_ = 0;
    frame::Bytes buf_;
};

// What `scan` found between its start and the end of the file.
struct Scan {
    // Offset after the last good record.
    std::uint64_t end = 0;
    // Sequence of the last good record, or the one before the start.
    std::uint64_t last_seq = 0;
    // Start offset of the last good record found by the scan.
    std::optional<std::uint64_t> last_record;
    // `(seq, offset)` of every `checkpoint_every`th record.
    std::vector<std::pair<std::uint64_t, std::uint64_t>> checkpoints;
};

// Checks every record from `start` (which holds `next_seq`) to `file_len`. A
// damaged record is a torn tail when nothing but zeros follows it, or when its
// declared length runs past the end of the file; the scan stops before it.
// Damage with real data after it is `Corrupt`.
[[nodiscard]] JournalResult<Scan> scan(const std::filesystem::path& path, std::uint64_t file_len,
                                       std::uint64_t start, std::uint64_t next_seq);

// The patch sequence and end offset of the record at `offset`, or none when it
// isn't a whole, valid record inside `file_len`.
[[nodiscard]] JournalResult<std::optional<std::pair<std::uint64_t, std::uint64_t>>> probe(
    const std::filesystem::path& path, std::uint64_t offset, std::uint64_t file_len);

// State of a journal file as found on disk.
struct Recovered {
    std::string session_id;
    std::optional<Parent> parent;
    std::uint64_t data_start = 0;
    std::uint64_t file_len = 0;
    std::uint64_t end = 0;
    std::uint64_t last_seq = 0;
    std::optional<std::uint64_t> last_record;
    std::map<std::uint64_t, std::uint64_t> checkpoints;
    std::vector<snapshot::Meta> snapshots;
};

[[nodiscard]] JournalResult<Recovered> recover(const std::filesystem::path& dir);

} // namespace loa::bus::detail
