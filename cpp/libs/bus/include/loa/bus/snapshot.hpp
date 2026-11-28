#pragma once

// Snapshot files: the whole DOM at one sequence, beside the journal (port of
// `crates/loa-bus/src/snapshot.rs`).
//
// Layout: a fixed 52 byte header (the frame preamble `LOASNAP\0` + version 1,
// then seq, journal offset of the next record, offset of the record at `seq`,
// body length, body crc32 and header crc32, all LE) followed by a `proto::Dom`
// body. The header alone tells the journal where to resume, so opening reads
// no DOM. Files are named `snap-<20-digit seq>.loas`.

#include <cstdint>
#include <expected>
#include <filesystem>
#include <vector>

#include "loa/bus/error.hpp"
#include "loa/core/dom.hpp"

namespace loa::bus::snapshot {

// Header of one snapshot file whose header checksum holds.
struct Meta {
    std::filesystem::path path;
    std::uint64_t seq = 0;
    // Journal offset of the record after `seq`.
    std::uint64_t offset = 0;
    // Journal offset of the record at `seq`.
    std::uint64_t prev_offset = 0;
    std::uint64_t body_len = 0;
    std::uint32_t body_crc = 0;

    friend bool operator==(const Meta&, const Meta&) = default;
};

// `snap-<seq:020>.loas`.
[[nodiscard]] std::string file_name(std::uint64_t seq);

// Every snapshot in `dir` with a valid header, oldest first. Files that are
// not snapshots, half written temp files and damaged headers are skipped.
[[nodiscard]] std::expected<std::vector<Meta>, JournalError> list(const std::filesystem::path& dir);

// Reads and checks the DOM of `meta`.
[[nodiscard]] std::expected<core::Dom, JournalError> read_dom(const Meta& meta);

// Writes `dom` as the snapshot at `dom.seq()`: temp file, fsync, rename,
// fsync of the directory. A crash leaves either no snapshot or a whole one.
[[nodiscard]] std::expected<Meta, JournalError> write(const std::filesystem::path& dir, const core::Dom& dom,
                                                      std::uint64_t offset, std::uint64_t prev_offset);

} // namespace loa::bus::snapshot
