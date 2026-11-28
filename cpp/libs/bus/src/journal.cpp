#include "loa/bus/journal.hpp"

#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#include <algorithm>
#include <cerrno>
#include <limits>

#include "journal_io.hpp"
#include "loa/core/schema.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/runtime/time.hpp"

namespace loa::bus {

namespace je = journal_error;
using detail::Fd;
using detail::io_error;

namespace {

// Writes all of `bytes` to `fd`. Returns errno on failure.
int write_all(int fd, std::span<const std::uint8_t> bytes) {
    while (!bytes.empty()) {
        const ssize_t n = ::write(fd, bytes.data(), bytes.size());
        if (n < 0) {
            if (errno == EINTR) continue;
            return errno;
        }
        if (n == 0) return EIO;
        bytes = bytes.subspan(static_cast<std::size_t>(n));
    }
    return 0;
}

JournalResult<void> sync_dir(const std::filesystem::path& dir) {
    auto ok = loa::runtime::fs::sync_dir(dir);
    if (!ok) return std::unexpected(io_error(dir, ok.error()));
    return {};
}

JournalResult<void> remove_temp_files(const std::filesystem::path& dir) {
    std::error_code ec;
    std::filesystem::directory_iterator it(dir, ec), end;
    if (ec) return std::unexpected(io_error(dir, ec.value()));
    std::vector<std::filesystem::path> doomed;
    for (; it != end; it.increment(ec)) {
        if (ec) return std::unexpected(io_error(dir, ec.value()));
        const auto name = it->path().filename().string();
        if (name.ends_with(".tmp") || name.starts_with(".tmp")) doomed.push_back(it->path());
    }
    if (ec) return std::unexpected(io_error(dir, ec.value()));
    for (const auto& p : doomed)
        if (std::filesystem::remove(p, ec); ec) return std::unexpected(io_error(p, ec.value()));
    return {};
}

} // namespace

JournalResult<void> validate_session_id(std::string_view id) {
    const bool ok = !id.empty() && id.size() <= 128 && id.front() != '.' &&
                    std::all_of(id.begin(), id.end(), [](char c) {
                        return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') ||
                               c == '-' || c == '_' || c == '.';
                    });
    if (ok) return {};
    return std::unexpected(JournalError(je::InvalidSessionId{std::string(id)}));
}

// ---- Patches ----

Patches::Patches(std::unique_ptr<detail::RecordReader> reader) : reader_(std::move(reader)) {}
Patches::Patches(Patches&&) noexcept = default;
Patches& Patches::operator=(Patches&&) noexcept = default;
Patches::~Patches() = default;

std::optional<JournalResult<core::Patch>> Patches::next() {
    if (!reader_) return std::nullopt;
    auto p = reader_->next_patch();
    if (!p) {
        reader_.reset();
        return JournalResult<core::Patch>(std::unexpected(std::move(p.error())));
    }
    if (!*p) return std::nullopt;
    return JournalResult<core::Patch>(std::move(**p));
}

// ---- Journal ----

Journal::Journal(Journal&&) noexcept = default;
Journal& Journal::operator=(Journal&& other) noexcept {
    if (this != &other) {
        if (dirty_ && file_) ::fdatasync(file_.get());
        dir_ = std::move(other.dir_);
        path_ = std::move(other.path_);
        file_ = std::move(other.file_);
        lock_ = std::move(other.lock_);
        session_id_ = std::move(other.session_id_);
        parent_ = std::move(other.parent_);
        options_ = other.options_;
        data_start_ = other.data_start_;
        end_ = other.end_;
        last_seq_ = other.last_seq_;
        last_record_ = other.last_record_;
        checkpoints_ = std::move(other.checkpoints_);
        snapshots_ = std::move(other.snapshots_);
        dirty_ = std::exchange(other.dirty_, false);
        damaged_ = other.damaged_;
        buf_ = std::move(other.buf_);
    }
    return *this;
}

Journal::~Journal() {
    // Best effort: a destructor can't report errors. Callers that need to
    // know call `sync` first.
    if (dirty_ && file_) ::fdatasync(file_.get());
}

JournalResult<Journal> Journal::create(const std::filesystem::path& dir, std::string_view session_id,
                                       JournalOptions options) {
    return create_with(dir, session_id, std::nullopt, options);
}

JournalResult<Journal> Journal::create_with(const std::filesystem::path& dir, std::string_view session_id,
                                            std::optional<Parent> parent, JournalOptions options) {
    if (auto v = validate_session_id(session_id); !v) return std::unexpected(v.error());
    if (::mkdir(dir.c_str(), 0777) != 0) {
        if (errno == EEXIST) return std::unexpected(JournalError(je::Exists{std::string(session_id)}));
        return std::unexpected(io_error(dir, errno));
    }
    auto lock = detail::lock(dir);
    if (!lock) return std::unexpected(lock.error());
    const auto path = dir / JOURNAL_FILE;
    detail::Header header{std::string(session_id), std::nullopt, std::nullopt};
    if (parent) {
        header.parent_session = parent->session_id;
        header.forked_at = parent->forked_at;
    }
    const auto bytes = detail::write_header(header);
    Fd file(::open(path.c_str(), O_RDWR | O_APPEND | O_CREAT | O_EXCL | O_CLOEXEC, 0666));
    if (!file) return std::unexpected(io_error(path, errno));
    if (int err = write_all(file.get(), bytes)) return std::unexpected(io_error(path, err));
    if (::fsync(file.get()) != 0) return std::unexpected(io_error(path, errno));
    if (auto s = sync_dir(dir); !s) return std::unexpected(s.error());
    if (const auto root = dir.parent_path(); !root.empty())
        if (auto s = sync_dir(root); !s) return std::unexpected(s.error());
    const std::uint64_t data_start = bytes.size();
    detail::Recovered found{std::string(session_id), std::move(parent), data_start, data_start, data_start, 0,
                            std::nullopt, {{1, data_start}}, {}};
    return assemble(dir, std::move(file), std::move(*lock), std::move(found), options);
}

Journal Journal::assemble(const std::filesystem::path& dir, Fd file, Fd lock, detail::Recovered found,
                          JournalOptions options) {
    Journal j;
    j.dir_ = dir;
    j.path_ = dir / JOURNAL_FILE;
    j.file_ = std::move(file);
    j.lock_ = std::move(lock);
    j.session_id_ = std::move(found.session_id);
    j.parent_ = std::move(found.parent);
    j.options_ = options;
    j.data_start_ = found.data_start;
    j.end_ = found.end;
    j.last_seq_ = found.last_seq;
    j.last_record_ = found.last_record;
    j.checkpoints_ = std::move(found.checkpoints);
    j.snapshots_ = std::move(found.snapshots);
    return j;
}

JournalResult<std::pair<Journal, std::optional<Truncation>>> Journal::open(const std::filesystem::path& dir,
                                                                           JournalOptions options) {
    auto lock = detail::lock(dir);
    if (!lock) return std::unexpected(lock.error());
    auto found = detail::recover(dir);
    if (!found) return std::unexpected(found.error());
    const auto path = dir / JOURNAL_FILE;
    std::optional<Truncation> truncation;
    if (found->end < found->file_len) {
        if (int err = detail::truncate_file(path, found->end)) return std::unexpected(io_error(path, err));
        truncation = Truncation{found->end, found->file_len - found->end, found->last_seq};
    }
    Fd file(::open(path.c_str(), O_RDWR | O_APPEND | O_CLOEXEC));
    if (!file) return std::unexpected(io_error(path, errno));
    if (auto r = remove_temp_files(dir); !r) return std::unexpected(r.error());
    return std::pair{assemble(dir, std::move(file), std::move(*lock), std::move(*found), options), truncation};
}

JournalResult<SessionInfo> Journal::inspect(const std::filesystem::path& dir) {
    auto found = detail::recover(dir);
    if (!found) return std::unexpected(found.error());
    return SessionInfo{std::move(found->session_id), std::move(found->parent), found->last_seq};
}

JournalResult<core::Dom> Journal::read(const std::filesystem::path& dir) {
    auto found = detail::recover(dir);
    if (!found) return std::unexpected(found.error());
    return replay_at(dir / JOURNAL_FILE, found->snapshots, found->data_start, found->end, found->last_seq);
}

std::vector<std::uint64_t> Journal::snapshot_seqs() const {
    std::vector<std::uint64_t> out;
    out.reserve(snapshots_.size());
    for (const auto& m : snapshots_) out.push_back(m.seq);
    return out;
}

JournalResult<void> Journal::append(const core::Patch& patch) {
    if (damaged_)
        return std::unexpected(JournalError(
            je::Corrupt{path_, end_, "an earlier failed write left a partial record; reopen the journal"}));
    if (patch.seq != last_seq_ + 1) return std::unexpected(JournalError(je::SequenceGap{last_seq_, patch.seq}));
    if (auto e = detail::encode_record(patch, buf_); !e) return e;
    if (int err = write_all(file_.get(), buf_)) {
        // Cut a partial record so the file ends on a record boundary. If that
        // fails too, refuse further appends: they would land after the partial
        // bytes and turn a torn tail into mid-file damage.
        damaged_ = detail::truncate_file(path_, end_) != 0;
        return std::unexpected(io_error(path_, err));
    }
    if ((patch.seq - 1) % frame::checkpoint_every == 0) checkpoints_.insert_or_assign(patch.seq, end_);
    last_record_ = end_;
    end_ += buf_.size();
    last_seq_ = patch.seq;
    dirty_ = true;
    return {};
}

JournalResult<void> Journal::append_applied(const core::Patch& patch, const core::Dom& dom) {
    if (dom.seq() != patch.seq) return std::unexpected(JournalError(je::DomMismatch{dom.seq(), patch.seq}));
    if (auto a = append(patch); !a) return a;
    if (snapshot_due()) return write_snapshot(dom);
    return {};
}

bool Journal::snapshot_due() const noexcept {
    const std::uint64_t every = options_.snapshot_every;
    const std::uint64_t last = snapshots_.empty() ? 0 : snapshots_.back().seq;
    return every > 0 && last_seq_ - last >= every;
}

JournalResult<void> Journal::sync() {
    if (dirty_) {
        if (::fdatasync(file_.get()) != 0) return std::unexpected(io_error(path_, errno));
        dirty_ = false;
    }
    return {};
}

JournalResult<void> Journal::write_snapshot(const core::Dom& dom) {
    // `last_record` is set whenever `last_seq` is above 0.
    if (!last_record_ || dom.seq() != last_seq_)
        return std::unexpected(JournalError(je::DomMismatch{dom.seq(), last_seq_}));
    const std::uint64_t prev = *last_record_;
    if (!snapshots_.empty() && snapshots_.back().seq == last_seq_) return {};
    if (auto s = sync(); !s) return s;
    auto meta = snapshot::write(dir_, dom, end_, prev);
    if (!meta) return std::unexpected(meta.error());
    checkpoints_.insert_or_assign(meta->seq + 1, meta->offset);
    snapshots_.push_back(std::move(*meta));
    const std::size_t keep = std::max<std::size_t>(options_.keep_snapshots, 1);
    while (snapshots_.size() > keep) {
        auto old = std::move(snapshots_.front());
        snapshots_.erase(snapshots_.begin());
        if (::unlink(old.path.c_str()) != 0 && errno != ENOENT) return std::unexpected(io_error(old.path, errno));
    }
    return {};
}

JournalResult<Patches> Journal::patches(std::uint64_t from_seq) const {
    const std::uint64_t from = std::max<std::uint64_t>(from_seq, 1);
    if (from > last_seq_ + 1) return std::unexpected(JournalError(je::OutOfRange{from, last_seq_}));
    auto offset = offset_of(from);
    if (!offset) return std::unexpected(offset.error());
    auto reader = detail::RecordReader::open(path_, *offset, end_, from);
    if (!reader) return std::unexpected(reader.error());
    return Patches(std::move(*reader));
}

JournalResult<core::Dom> Journal::load() const { return load_at(last_seq_); }

JournalResult<core::Dom> Journal::load_at(std::uint64_t seq) const {
    if (seq > last_seq_) return std::unexpected(JournalError(je::OutOfRange{seq, last_seq_}));
    return replay_at(path_, snapshots_, data_start_, end_, seq);
}

JournalResult<core::Dom> Journal::replay_at(const std::filesystem::path& path,
                                            const std::vector<snapshot::Meta>& snapshots, std::uint64_t data_start,
                                            std::uint64_t end, std::uint64_t seq) {
    for (auto it = snapshots.rbegin(); it != snapshots.rend(); ++it) {
        if (it->seq > seq) continue;
        auto dom = snapshot::read_dom(*it);
        if (!dom) continue;
        if (auto replayed = replay(path, end, std::move(*dom), it->offset, seq)) return replayed;
    }
    return replay(path, end, core::Dom{}, data_start, seq);
}

JournalResult<core::Dom> Journal::replay(const std::filesystem::path& path, std::uint64_t end, core::Dom dom,
                                         std::uint64_t offset, std::uint64_t target) {
    auto reader = detail::RecordReader::open(path, offset, end, dom.seq() + 1);
    if (!reader) return std::unexpected(reader.error());
    while (dom.seq() < target) {
        auto patch = (*reader)->expect_patch(target);
        if (!patch) return std::unexpected(patch.error());
        if (auto a = dom.apply(*patch); !a) return std::unexpected(JournalError(je::Apply{patch->seq, a.error()}));
    }
    return dom;
}

JournalResult<std::uint64_t> Journal::offset_of(std::uint64_t seq) const {
    if (seq == last_seq_ + 1) return end_;
    if (seq == last_seq_ && last_record_) return *last_record_;
    auto it = checkpoints_.upper_bound(seq);
    if (it == checkpoints_.begin()) return std::unexpected(JournalError(je::OutOfRange{seq, last_seq_}));
    --it;
    auto reader = detail::RecordReader::open(path_, it->second, end_, it->first);
    if (!reader) return std::unexpected(reader.error());
    while ((*reader)->next_seq() < seq)
        if (auto p = (*reader)->expect_patch(seq); !p) return std::unexpected(p.error());
    return (*reader)->offset();
}

JournalResult<Journal> Journal::fork(std::uint64_t at_seq, std::string_view new_session_id) const {
    if (auto v = validate_session_id(new_session_id); !v) return std::unexpected(v.error());
    auto dom = load_at(at_seq);
    if (!dom) return std::unexpected(dom.error());
    auto cut = offset_of(at_seq + 1);
    if (!cut) return std::unexpected(cut.error());
    std::optional<std::uint64_t> at_record;
    if (at_seq > 0) {
        auto r = offset_of(at_seq);
        if (!r) return std::unexpected(r.error());
        at_record = *r;
    }
    const auto dir = dir_.parent_path() / std::string(new_session_id);
    Parent parent{session_id_, at_seq};
    JournalResult<void> filled;
    {
        auto fork = create_with(dir, new_session_id, std::move(parent), options_);
        if (!fork) return fork;
        filled = fill_fork(*fork, std::move(*dom), *cut, at_record);
        if (filled) return fork;
    }
    // Leave no half made session behind. The fork was closed above.
    std::error_code ec;
    std::filesystem::remove_all(dir, ec);
    return std::unexpected(filled.error());
}

JournalResult<void> Journal::fill_fork(Journal& fork, core::Dom dom, std::uint64_t cut,
                                       std::optional<std::uint64_t> at_record) const {
    const std::uint64_t len = cut - data_start_;
    Fd src(::open(path_.c_str(), O_RDONLY | O_CLOEXEC));
    if (!src) return std::unexpected(io_error(path_, errno));
    std::vector<std::uint8_t> chunk(64 * 1024);
    std::uint64_t copied = 0;
    while (copied < len) {
        const auto want = static_cast<std::size_t>(std::min<std::uint64_t>(chunk.size(), len - copied));
        const ssize_t n = ::pread(src.get(), chunk.data(), want, static_cast<off_t>(data_start_ + copied));
        if (n < 0) {
            if (errno == EINTR) continue;
            return std::unexpected(io_error(path_, errno));
        }
        if (n == 0) break;
        if (int err = write_all(fork.file_.get(), std::span(chunk).first(static_cast<std::size_t>(n))))
            return std::unexpected(io_error(fork.path_, err));
        copied += static_cast<std::uint64_t>(n);
    }
    if (copied != len)
        return std::unexpected(JournalError(je::Corrupt{path_, data_start_ + copied, "journal shrank while forking"}));
    const auto shift = [&](std::uint64_t offset) { return offset - data_start_ + fork.data_start_; };
    const std::uint64_t at_seq = dom.seq();
    for (auto it = checkpoints_.begin(); it != checkpoints_.end() && it->first <= at_seq; ++it)
        fork.checkpoints_.insert_or_assign(it->first, shift(it->second));
    fork.end_ = shift(cut);
    fork.last_seq_ = at_seq;
    fork.last_record_ = at_record ? std::optional(shift(*at_record)) : std::nullopt;
    fork.dirty_ = true;
    if (const core::Node* root = dom.root()) {
        const core::NodeId id = root->id;
        const auto forked_at =
            static_cast<std::int64_t>(std::min<std::uint64_t>(at_seq, std::numeric_limits<std::int64_t>::max()));
        core::Draft drafts[] = {
            core::Draft::set(id, std::string(core::attr::ID), core::Value(fork.session_id_)),
            core::Draft::set(id, std::string(core::attr::PARENT_SESSION), core::Value(session_id_)),
            core::Draft::set(id, std::string(core::attr::FORKED_AT), core::Value(forked_at)),
        };
        const std::int64_t ts = loa::runtime::time::now_ms();
        for (auto& draft : drafts) {
            auto patch = dom.commit(std::move(draft), ts);
            if (!patch) return std::unexpected(JournalError(je::Apply{dom.seq() + 1, patch.error()}));
            if (auto a = fork.append(*patch); !a) return a;
        }
    }
    if (fork.last_seq_ > 0) return fork.write_snapshot(dom);
    return fork.sync();
}

} // namespace loa::bus
