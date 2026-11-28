#pragma once

// All sessions under one root directory, one subdirectory per session, and the
// journal side of a session writer: shared, cheap appends with a pluggable
// fsync strategy (port of `crates/loa-bus/src/store.rs`).

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/bus/journal.hpp"
#include "loa/bus/writer.hpp"

namespace loa::bus {

namespace listing {
// A session directory whose journal can't be read.
struct Unreadable {
    std::string id;
    JournalError error;
};
} // namespace listing

// One entry of `Store::list`.
using Listing = std::variant<SessionInfo, listing::Unreadable>;

// The session journals under a root directory.
class Store {
  public:
    // A store over `root`, created when missing. `options` apply to every
    // journal it opens or creates.
    [[nodiscard]] static JournalResult<Store> make(std::filesystem::path root, JournalOptions options = {});

    // A store in another directory with the same journal options.
    [[nodiscard]] JournalResult<Store> with_root(const std::filesystem::path& root) const;

    [[nodiscard]] const std::filesystem::path& root() const noexcept { return root_; }

    // Every session, sorted by id. Reads only each journal's tail after its
    // newest snapshot and never repairs anything.
    [[nodiscard]] JournalResult<std::vector<Listing>> list() const;
    // Inspects a single journal without opening it for writing.
    [[nodiscard]] JournalResult<SessionInfo> inspect(std::string_view id) const;
    // Creates a new empty session.
    [[nodiscard]] JournalResult<Journal> create(std::string_view id) const;
    // Opens a session for writing; see `Journal::open`.
    [[nodiscard]] JournalResult<std::pair<Journal, std::optional<Truncation>>> open(std::string_view id) const;
    // Reads the latest complete state without opening the journal for writing.
    [[nodiscard]] JournalResult<core::Dom> read(std::string_view id) const;
    // Deletes a session and its snapshots. Fails with `Locked` while a journal
    // has it open. Forks of it keep working: they hold their own copy of the
    // shared patches.
    [[nodiscard]] JournalResult<void> remove(std::string_view id) const;

  private:
    Store(std::filesystem::path root, JournalOptions options) : root_(std::move(root)), options_(options) {}
    [[nodiscard]] JournalResult<std::filesystem::path> dir(std::string_view id) const;

    std::filesystem::path root_;
    JournalOptions options_;
};

// Decides after each append whether the pending patches are fsynced now.
class SyncStrategy {
  public:
    virtual ~SyncStrategy() = default;
    // `pending` patches are unsynced, the oldest appended at `oldest`.
    [[nodiscard]] virtual bool after_append(std::size_t pending, std::chrono::steady_clock::time_point oldest) = 0;
};

// When appended patches are fsynced.
class SyncPolicy final : public SyncStrategy {
  public:
    enum class Kind : std::uint8_t {
        // After every patch. Slowest, nothing is ever lost.
        every_patch,
        // Once `max_patches` patches are pending, or when an append finds the
        // oldest pending patch older than `max_delay`.
        batch,
        // Only on `JournalWriter::sync`, a snapshot, or close.
        manual,
    };

    // The default: batches of 64 patches or 200 ms.
    SyncPolicy() : SyncPolicy(batch(64, std::chrono::milliseconds(200))) {}
    [[nodiscard]] static SyncPolicy every_patch() { return SyncPolicy(Kind::every_patch, 0, {}); }
    [[nodiscard]] static SyncPolicy batch(std::uint32_t max_patches, std::chrono::nanoseconds max_delay) {
        return SyncPolicy(Kind::batch, max_patches, max_delay);
    }
    [[nodiscard]] static SyncPolicy manual() { return SyncPolicy(Kind::manual, 0, {}); }

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    [[nodiscard]] std::uint32_t max_patches() const noexcept { return max_patches_; }
    [[nodiscard]] std::chrono::nanoseconds max_delay() const noexcept { return max_delay_; }

    bool after_append(std::size_t pending, std::chrono::steady_clock::time_point oldest) override;

    friend bool operator==(const SyncPolicy&, const SyncPolicy&) = default;

  private:
    SyncPolicy(Kind kind, std::uint32_t max_patches, std::chrono::nanoseconds max_delay)
        : kind_(kind), max_patches_(max_patches), max_delay_(max_delay) {}

    Kind kind_;
    std::uint32_t max_patches_;
    std::chrono::nanoseconds max_delay_;
};

// A `Journal` shared by the session's single writer and its readers.
//
// Every append is written to the OS at once, so a crash of the app loses
// nothing; the strategy only bounds what a power loss can take.
class JournalWriter {
  public:
    JournalWriter(Journal journal, std::unique_ptr<SyncStrategy> strategy);
    JournalWriter(Journal journal, SyncPolicy policy);

    // Journals `patch`, which the writer has already applied to `dom`, writes
    // a snapshot of `dom` when one is due, and fsyncs as the strategy says.
    [[nodiscard]] JournalResult<void> append(const core::Patch& patch, const core::Dom& dom);
    // Journals `patch` before it is applied, for writers that persist first
    // (a `SessionWriter`). Follow up with `snapshot_if_due` once it is applied.
    [[nodiscard]] JournalResult<void> append_patch(const core::Patch& patch);
    // Writes a snapshot of `dom`, which must be at the last journaled patch,
    // when `JournalOptions::snapshot_every` patches followed the last one.
    [[nodiscard]] JournalResult<void> snapshot_if_due(const core::Dom& dom);
    // See `Journal::load_at`.
    [[nodiscard]] JournalResult<core::Dom> load_at(std::uint64_t seq) const;
    [[nodiscard]] const std::string& session_id() const noexcept { return session_id_; }
    // Fsyncs every pending patch.
    [[nodiscard]] JournalResult<void> sync();
    // Patches appended but not fsynced yet.
    [[nodiscard]] std::size_t pending() const;
    [[nodiscard]] std::uint64_t last_seq() const;
    // See `Journal::patches`. The returned reader doesn't hold the lock.
    [[nodiscard]] JournalResult<Patches> patches(std::uint64_t from_seq) const;
    // See `Journal::fork`.
    [[nodiscard]] JournalResult<Journal> fork(std::uint64_t at_seq, std::string_view new_session_id) const;
    // Syncs and releases the journal and its session lock, while others may
    // still hold this writer: later appends fail with `Closed`.
    [[nodiscard]] JournalResult<void> close();
    // Syncs and hands the journal back.
    [[nodiscard]] JournalResult<Journal> into_journal() &&;

  private:
    [[nodiscard]] JournalResult<void> sync_locked();
    [[nodiscard]] JournalResult<void> appended_locked();

    std::string session_id_;
    mutable std::mutex mutex_;
    // None once `close` released it.
    std::optional<Journal> journal_;
    // Last sequence, kept for after the close.
    std::uint64_t last_seq_ = 0;
    std::unique_ptr<SyncStrategy> strategy_;
    std::size_t pending_ = 0;
    std::optional<std::chrono::steady_clock::time_point> oldest_pending_;
};

// The `Persist` adapter of a live session backed by its journal. Patches are
// journaled before the writer applies them; snapshots are written after, from
// the writer's own DOM, so nothing keeps a second copy.
class JournalPersist final : public Persist {
  public:
    explicit JournalPersist(std::shared_ptr<JournalWriter> writer) : writer_(std::move(writer)) {}

    [[nodiscard]] const std::shared_ptr<JournalWriter>& writer() const noexcept { return writer_; }

    std::expected<void, PersistError> append(const core::Patch& patch) override;
    void applied(const core::Dom& dom) override;

  private:
    std::shared_ptr<JournalWriter> writer_;
};

} // namespace loa::bus
