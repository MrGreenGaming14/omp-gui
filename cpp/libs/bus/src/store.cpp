#include "loa/bus/store.hpp"

#include <algorithm>

#include "journal_io.hpp"

namespace loa::bus {

namespace je = journal_error;

namespace {

bool is_file(const std::filesystem::path& p) {
    std::error_code ec;
    return std::filesystem::is_regular_file(p, ec);
}

const std::string& listing_id(const Listing& l) {
    if (const auto* info = std::get_if<SessionInfo>(&l)) return info->id;
    return std::get<listing::Unreadable>(l).id;
}

} // namespace

JournalResult<Store> Store::make(std::filesystem::path root, JournalOptions options) {
    std::error_code ec;
    std::filesystem::create_directories(root, ec);
    if (ec) return std::unexpected(detail::io_error(root, ec.value()));
    return Store(std::move(root), options);
}

JournalResult<Store> Store::with_root(const std::filesystem::path& root) const { return make(root, options_); }

JournalResult<std::filesystem::path> Store::dir(std::string_view id) const {
    if (auto v = validate_session_id(id); !v) return std::unexpected(v.error());
    return root_ / std::string(id);
}

JournalResult<std::vector<Listing>> Store::list() const {
    std::vector<Listing> out;
    std::error_code ec;
    std::filesystem::directory_iterator it(root_, ec), end;
    if (ec) return std::unexpected(detail::io_error(root_, ec.value()));
    for (; it != end; it.increment(ec)) {
        if (ec) return std::unexpected(detail::io_error(root_, ec.value()));
        const auto dir = it->path();
        if (!is_file(dir / JOURNAL_FILE)) continue;
        auto info = Journal::inspect(dir);
        if (info) out.emplace_back(std::move(*info));
        else out.emplace_back(listing::Unreadable{dir.filename().string(), std::move(info.error())});
    }
    if (ec) return std::unexpected(detail::io_error(root_, ec.value()));
    std::stable_sort(out.begin(), out.end(),
                     [](const Listing& a, const Listing& b) { return listing_id(a) < listing_id(b); });
    return out;
}

JournalResult<SessionInfo> Store::inspect(std::string_view id) const {
    auto d = dir(id);
    if (!d) return std::unexpected(d.error());
    if (!is_file(*d / JOURNAL_FILE)) return std::unexpected(JournalError(je::NotFound{std::string(id)}));
    return Journal::inspect(*d);
}

JournalResult<Journal> Store::create(std::string_view id) const {
    auto d = dir(id);
    if (!d) return std::unexpected(d.error());
    return Journal::create(*d, id, options_);
}

JournalResult<std::pair<Journal, std::optional<Truncation>>> Store::open(std::string_view id) const {
    auto d = dir(id);
    if (!d) return std::unexpected(d.error());
    if (!is_file(*d / JOURNAL_FILE)) return std::unexpected(JournalError(je::NotFound{std::string(id)}));
    return Journal::open(*d, options_);
}

JournalResult<core::Dom> Store::read(std::string_view id) const {
    auto d = dir(id);
    if (!d) return std::unexpected(d.error());
    return Journal::read(*d);
}

JournalResult<void> Store::remove(std::string_view id) const {
    auto d = dir(id);
    if (!d) return std::unexpected(d.error());
    if (!is_file(*d / JOURNAL_FILE)) return std::unexpected(JournalError(je::NotFound{std::string(id)}));
    {
        // Released before removal: some platforms can't delete an open file.
        auto lock = detail::lock(*d);
        if (!lock) return std::unexpected(lock.error());
    }
    std::error_code ec;
    std::filesystem::remove_all(*d, ec);
    if (ec) return std::unexpected(detail::io_error(*d, ec.value()));
    return {};
}

// ---- SyncPolicy ----

bool SyncPolicy::after_append(std::size_t pending, std::chrono::steady_clock::time_point oldest) {
    switch (kind_) {
    case Kind::every_patch: return true;
    case Kind::batch:
        return pending >= max_patches_ || std::chrono::steady_clock::now() - oldest >= max_delay_;
    case Kind::manual: break;
    }
    return false;
}

// ---- JournalWriter ----

JournalWriter::JournalWriter(Journal journal, std::unique_ptr<SyncStrategy> strategy)
    : session_id_(journal.session_id()), last_seq_(journal.last_seq()), strategy_(std::move(strategy)) {
    journal_.emplace(std::move(journal));
}

JournalWriter::JournalWriter(Journal journal, SyncPolicy policy)
    : JournalWriter(std::move(journal), std::make_unique<SyncPolicy>(policy)) {}

JournalResult<void> JournalWriter::sync_locked() {
    if (journal_)
        if (auto s = journal_->sync(); !s) return s;
    pending_ = 0;
    oldest_pending_.reset();
    return {};
}

JournalResult<void> JournalWriter::appended_locked() {
    if (!journal_->has_unsynced()) {
        // A snapshot synced everything.
        pending_ = 0;
        oldest_pending_.reset();
        return {};
    }
    ++pending_;
    if (!oldest_pending_) oldest_pending_ = std::chrono::steady_clock::now();
    if (strategy_->after_append(pending_, *oldest_pending_)) return sync_locked();
    return {};
}

JournalResult<void> JournalWriter::append(const core::Patch& patch, const core::Dom& dom) {
    std::scoped_lock lock(mutex_);
    if (!journal_) return std::unexpected(JournalError(je::Closed{}));
    if (auto a = journal_->append_applied(patch, dom); !a) return a;
    return appended_locked();
}

JournalResult<void> JournalWriter::append_patch(const core::Patch& patch) {
    std::scoped_lock lock(mutex_);
    if (!journal_) return std::unexpected(JournalError(je::Closed{}));
    if (auto a = journal_->append(patch); !a) return a;
    return appended_locked();
}

JournalResult<void> JournalWriter::snapshot_if_due(const core::Dom& dom) {
    std::scoped_lock lock(mutex_);
    if (!journal_) return std::unexpected(JournalError(je::Closed{}));
    if (!journal_->snapshot_due()) return {};
    if (auto s = journal_->write_snapshot(dom); !s) return s;
    // A snapshot syncs everything before it.
    pending_ = 0;
    oldest_pending_.reset();
    return {};
}

JournalResult<core::Dom> JournalWriter::load_at(std::uint64_t seq) const {
    std::scoped_lock lock(mutex_);
    if (!journal_) return std::unexpected(JournalError(je::Closed{}));
    return journal_->load_at(seq);
}

JournalResult<void> JournalWriter::sync() {
    std::scoped_lock lock(mutex_);
    return sync_locked();
}

std::size_t JournalWriter::pending() const {
    std::scoped_lock lock(mutex_);
    return pending_;
}

std::uint64_t JournalWriter::last_seq() const {
    std::scoped_lock lock(mutex_);
    return journal_ ? journal_->last_seq() : last_seq_;
}

JournalResult<Patches> JournalWriter::patches(std::uint64_t from_seq) const {
    std::scoped_lock lock(mutex_);
    if (!journal_) return std::unexpected(JournalError(je::Closed{}));
    return journal_->patches(from_seq);
}

JournalResult<Journal> JournalWriter::fork(std::uint64_t at_seq, std::string_view new_session_id) const {
    std::scoped_lock lock(mutex_);
    if (!journal_) return std::unexpected(JournalError(je::Closed{}));
    return journal_->fork(at_seq, new_session_id);
}

JournalResult<void> JournalWriter::close() {
    std::scoped_lock lock(mutex_);
    if (!journal_) return {};
    if (auto s = sync_locked(); !s) return s;
    last_seq_ = journal_->last_seq();
    journal_.reset();
    return {};
}

JournalResult<Journal> JournalWriter::into_journal() && {
    std::scoped_lock lock(mutex_);
    if (auto s = sync_locked(); !s) return std::unexpected(s.error());
    if (!journal_) return std::unexpected(JournalError(je::Closed{}));
    Journal out = std::move(*journal_);
    journal_.reset();
    return out;
}

// ---- JournalPersist ----

std::expected<void, PersistError> JournalPersist::append(const core::Patch& patch) {
    auto a = writer_->append_patch(patch);
    if (!a) return std::unexpected(to_error(a.error()));
    return {};
}

void JournalPersist::applied(const core::Dom& dom) {
    // The patch is already in the journal; a failed snapshot only means the
    // next load replays further. The next due patch retries it.
    (void)writer_->snapshot_if_due(dom);
}

} // namespace loa::bus
