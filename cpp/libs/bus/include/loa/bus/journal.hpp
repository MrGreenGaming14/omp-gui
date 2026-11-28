#pragma once

// The on-disk journal of one session (port of `crates/loa-bus/src/journal.rs`).
//
// A session lives in its own directory:
//
// - `journal.loaj`: header, then one framed record per patch (see `frame.hpp`)
// - `snap-<seq>.loas`: DOM snapshots, written atomically every N patches
// - `lock`: held while a `Journal` has the session open, so it has one writer
//
// Header: `LOAJRNL\0` and the format version, then one record holding the
// prost `Header {session_id = 1; optional parent_session = 2; optional
// forked_at = 3}` message.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/bus/error.hpp"
#include "loa/bus/frame.hpp"
#include "loa/bus/snapshot.hpp"
#include "loa/core/dom.hpp"

namespace loa::bus {

// File name of the journal inside a session directory.
inline constexpr std::string_view JOURNAL_FILE = "journal.loaj";
// Version of the journal file layout. Bumped with a migration.
inline constexpr std::uint32_t FORMAT_VERSION = 1;

template <class T>
using JournalResult = std::expected<T, JournalError>;

// The session a fork was made from.
struct Parent {
    std::string session_id;
    // Sequence of the source's last patch that the fork shares.
    std::uint64_t forked_at = 0;
    friend bool operator==(const Parent&, const Parent&) = default;
};

// Tuning of a journal.
struct JournalOptions {
    // Write a snapshot after this many patches since the last one. 0 never
    // snapshots on its own.
    std::uint64_t snapshot_every = 1000;
    // Snapshots kept on disk; older ones are deleted. At least 1 is kept.
    std::size_t keep_snapshots = 2;
};

// Bytes cut from the end of a journal on open because the last append was
// interrupted (a crash or power loss mid-write).
struct Truncation {
    // New length of the journal file.
    std::uint64_t offset = 0;
    std::uint64_t dropped_bytes = 0;
    // Sequence of the last patch that survived.
    std::uint64_t last_seq = 0;
    friend bool operator==(const Truncation&, const Truncation&) = default;
};

// What a session directory holds, read without opening it for writing.
struct SessionInfo {
    std::string id;
    std::optional<Parent> parent;
    // Sequence of the last whole patch. A torn tail is not counted.
    std::uint64_t last_seq = 0;
    friend bool operator==(const SessionInfo&, const SessionInfo&) = default;
};

namespace detail {
// An owned file descriptor, closed on destruction.
class Fd {
  public:
    Fd() = default;
    explicit Fd(int fd) noexcept : fd_(fd) {}
    Fd(Fd&& o) noexcept : fd_(std::exchange(o.fd_, -1)) {}
    Fd& operator=(Fd&& o) noexcept;
    Fd(const Fd&) = delete;
    Fd& operator=(const Fd&) = delete;
    ~Fd();
    [[nodiscard]] int get() const noexcept { return fd_; }
    [[nodiscard]] explicit operator bool() const noexcept { return fd_ >= 0; }

  private:
    int fd_ = -1;
};

class RecordReader;
struct Recovered;
} // namespace detail

// Patches read from a journal in order. Stops after the first error.
class Patches {
  public:
    explicit Patches(std::unique_ptr<detail::RecordReader> reader);
    Patches(Patches&&) noexcept;
    Patches& operator=(Patches&&) noexcept;
    ~Patches();

    // The next patch, an error, or none at the end.
    [[nodiscard]] std::optional<JournalResult<core::Patch>> next();

  private:
    std::unique_ptr<detail::RecordReader> reader_;
};

// An open session journal. Holds the session's lock until destroyed.
//
// Appends go straight to the file (one write per patch) so a process crash
// loses nothing; `sync` makes them survive power loss.
class Journal {
  public:
    // Creates the session directory `dir` with an empty journal for `session_id`.
    [[nodiscard]] static JournalResult<Journal> create(const std::filesystem::path& dir, std::string_view session_id,
                                                       JournalOptions options = {});

    // Opens the session in `dir` for writing.
    //
    // Only records after the newest valid snapshot are read. A torn last
    // record is cut off and reported as a `Truncation`; damage followed by
    // more records is `Corrupt`.
    [[nodiscard]] static JournalResult<std::pair<Journal, std::optional<Truncation>>> open(
        const std::filesystem::path& dir, JournalOptions options = {});

    // Reads the id, fork link and last sequence of the session in `dir`
    // without locking or repairing it.
    [[nodiscard]] static JournalResult<SessionInfo> inspect(const std::filesystem::path& dir);

    // Replays the latest complete state without acquiring the writer lock or
    // repairing the journal.
    [[nodiscard]] static JournalResult<core::Dom> read(const std::filesystem::path& dir);

    Journal(Journal&&) noexcept;
    Journal& operator=(Journal&&) noexcept;
    Journal(const Journal&) = delete;
    Journal& operator=(const Journal&) = delete;
    // Best effort fsync of unsynced appends.
    ~Journal();

    [[nodiscard]] const std::string& session_id() const noexcept { return session_id_; }
    // The session this one was forked from.
    [[nodiscard]] const std::optional<Parent>& parent() const noexcept { return parent_; }
    // Sequence of the last appended patch, 0 when empty.
    [[nodiscard]] std::uint64_t last_seq() const noexcept { return last_seq_; }
    // The session directory.
    [[nodiscard]] const std::filesystem::path& dir() const noexcept { return dir_; }
    // Sequences of the snapshots on disk, oldest first.
    [[nodiscard]] std::vector<std::uint64_t> snapshot_seqs() const;
    // True when appended patches have not been fsynced yet.
    [[nodiscard]] bool has_unsynced() const noexcept { return dirty_; }

    // Appends the next patch. Its sequence must follow `last_seq`. The bytes
    // reach the OS before this returns; call `sync` for durability.
    [[nodiscard]] JournalResult<void> append(const core::Patch& patch);
    // Appends a patch the caller has already applied to `dom`, then writes a
    // snapshot of `dom` when one is due.
    [[nodiscard]] JournalResult<void> append_applied(const core::Patch& patch, const core::Dom& dom);
    // True when `JournalOptions::snapshot_every` patches followed the last snapshot.
    [[nodiscard]] bool snapshot_due() const noexcept;
    // Fsyncs appended patches.
    [[nodiscard]] JournalResult<void> sync();
    // Writes `dom`, which must be at `last_seq`, as a snapshot. Syncs the
    // journal first so a snapshot never covers patches that could still be lost.
    [[nodiscard]] JournalResult<void> write_snapshot(const core::Dom& dom);

    // Patches from `from_seq` (1 when 0) to the current end, read from disk.
    // Later appends are not included.
    [[nodiscard]] JournalResult<Patches> patches(std::uint64_t from_seq) const;
    // The DOM after the last patch: the newest valid snapshot plus the
    // patches after it. A damaged snapshot falls back to the one before it,
    // and finally to a replay from the first patch.
    [[nodiscard]] JournalResult<core::Dom> load() const;
    // The DOM right after patch `seq` (the empty DOM for 0).
    [[nodiscard]] JournalResult<core::Dom> load_at(std::uint64_t seq) const;

    // Forks the session after patch `at_seq` into a sibling directory named
    // `new_session_id`. The fork holds a copy of patches `1..=at_seq` and links
    // the source in its header. When the DOM has a root, the fork's first own
    // patches set the root's `id`, `parent_session` and `forked_at`. The source
    // is only read.
    [[nodiscard]] JournalResult<Journal> fork(std::uint64_t at_seq, std::string_view new_session_id) const;

  private:
    Journal() = default;
    static JournalResult<Journal> create_with(const std::filesystem::path& dir, std::string_view session_id,
                                              std::optional<Parent> parent, JournalOptions options);
    static Journal assemble(const std::filesystem::path& dir, detail::Fd file, detail::Fd lock,
                            detail::Recovered found, JournalOptions options);
    static JournalResult<core::Dom> replay_at(const std::filesystem::path& path,
                                              const std::vector<snapshot::Meta>& snapshots,
                                              std::uint64_t data_start, std::uint64_t end, std::uint64_t seq);
    static JournalResult<core::Dom> replay(const std::filesystem::path& path, std::uint64_t end, core::Dom dom,
                                           std::uint64_t offset, std::uint64_t target);
    [[nodiscard]] JournalResult<std::uint64_t> offset_of(std::uint64_t seq) const;
    JournalResult<void> fill_fork(Journal& fork, core::Dom dom, std::uint64_t cut,
                                  std::optional<std::uint64_t> at_record) const;

    std::filesystem::path dir_;
    std::filesystem::path path_;
    detail::Fd file_;
    detail::Fd lock_;
    std::string session_id_;
    std::optional<Parent> parent_;
    JournalOptions options_;
    // Offset of the first record.
    std::uint64_t data_start_ = 0;
    // Offset after the last record, the file length.
    std::uint64_t end_ = 0;
    std::uint64_t last_seq_ = 0;
    // Offset of the record at `last_seq`.
    std::optional<std::uint64_t> last_record_;
    // Sparse index: record sequence to its offset. Always holds `(1, data_start)`.
    std::map<std::uint64_t, std::uint64_t> checkpoints_;
    // Snapshots whose header and journal anchor check out, oldest first.
    std::vector<snapshot::Meta> snapshots_;
    // Appends not yet fsynced.
    bool dirty_ = false;
    // A failed write left bytes that could not be cut off again.
    bool damaged_ = false;
    frame::Bytes buf_;
};

// Checks that `id` is safe as a directory name on every platform.
[[nodiscard]] JournalResult<void> validate_session_id(std::string_view id);

} // namespace loa::bus
