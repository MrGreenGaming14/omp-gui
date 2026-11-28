// Diff model: numbered lines with intra-line ranges, hunks, folds of unchanged lines, and the row
// lists the view virtualizes. Port of `crates/loa-ui-panel/src/diff/model.rs`.

#include <algorithm>
#include <array>
#include <limits>

#include "diff_builder.hpp"
#include "loa/runtime/diff.hpp"

namespace loa::ui::panel::diff {

namespace {

// Unchanged lines kept around each change, like `diff -U3`.
constexpr std::size_t CONTEXT = 3;
// Folds hide at least this many lines; a one-line fold takes the line's own room.
constexpr std::size_t MIN_FOLD = 2;
// Paired lines longer than this, or less similar than `MIN_INTRA_RATIO`, get no word ranges.
constexpr std::size_t MAX_INTRA_LINE = 4'000;
constexpr float MIN_INTRA_RATIO = 0.4F;

// Counts above `u32::MAX` saturate; no text the panel shows is that long.
std::uint32_t count(std::size_t n) {
    return n > std::numeric_limits<std::uint32_t>::max() ? std::numeric_limits<std::uint32_t>::max() : static_cast<std::uint32_t>(n);
}

// Changed byte ranges of two paired lines, old then new; none when too long, too different or wholly changed.
std::optional<std::array<std::vector<Range>, 2>> intra_line(std::string_view old, std::string_view new_) {
    if (old.size() > MAX_INTRA_LINE || new_.size() > MAX_INTRA_LINE || old.empty() || new_.empty()) return std::nullopt;
    const auto d = loa::runtime::diff::diff_words(old, new_);
    if (d.ratio() < MIN_INTRA_RATIO) return std::nullopt;
    auto offsets = [](const std::vector<std::string_view>& tokens) {
        std::vector<std::size_t> at{0};
        at.reserve(tokens.size() + 1);
        for (const auto t : tokens) at.push_back(at.back() + t.size());
        return at;
    };
    const std::array<std::vector<std::size_t>, 2> at{offsets(d.old_tokens), offsets(d.new_tokens)};
    std::array<std::vector<Range>, 2> ranges;
    for (const auto& op : d.ops) {
        if (op.tag == loa::runtime::diff::Tag::equal) continue;
        const std::array<Range, 2> sides{Range{op.old_index, op.old_end()}, Range{op.new_index, op.new_end()}};
        for (std::size_t s = 0; s < 2; ++s) {
            const auto r = sides[s];
            if (r.empty()) continue;
            auto& out = ranges[s];
            if (!out.empty() && out.back().end == at[s][r.start]) out.back().end = at[s][r.end];
            else out.push_back({at[s][r.start], at[s][r.end]});
        }
    }
    auto whole = [](const std::vector<Range>& r, std::size_t len) { return r.size() == 1 && r[0] == Range{0, len}; };
    if ((ranges[0].empty() && ranges[1].empty()) || whole(ranges[0], old.size()) || whole(ranges[1], new_.size())) return std::nullopt;
    return ranges;
}

} // namespace

std::string_view FileDiff::path() const noexcept {
    if (new_path) return *new_path;
    if (old_path) return *old_path;
    return {};
}

FileStatus FileDiff::status() const noexcept {
    if (!old_path) return FileStatus::Added;
    if (!new_path) return FileStatus::Deleted;
    return *old_path != *new_path ? FileStatus::Renamed : FileStatus::Modified;
}

std::string_view FileDiff::line_text(std::size_t i) const noexcept {
    if (i >= lines_.size()) return {};
    const auto r = lines_[i].text;
    return std::string_view(text_).substr(r.start, r.size());
}

std::span<const Range> FileDiff::emphasis(std::size_t i) const noexcept {
    if (i >= lines_.size()) return {};
    const auto r = lines_[i].emph;
    return std::span<const Range>(emph_).subspan(r.start, r.size());
}

std::string_view FileDiff::section(const Hunk& h) const noexcept {
    if (!h.section || *h.section >= breaks_.size()) return {};
    const auto r = breaks_[*h.section].section;
    return std::string_view(text_).substr(r.start, r.size());
}

bool FileDiff::expand(std::size_t fold) {
    if (fold >= folds_.size()) return false;
    return !std::exchange(folds_[fold].expanded, true);
}

// Feeds a hunk header or fold row with the visible lines after it (none after a fold), runs within hunks.
template <typename F> void FileDiff::walk(F&& emit) const {
    const auto n = lines_.size();
    std::size_t fold = 0;
    auto skip_open = [&] {
        while (fold < folds_.size() && folds_[fold].expanded) ++fold;
    };
    skip_open();
    std::size_t brk = 0;
    std::size_t i = 0;
    std::uint32_t last_old = 0, last_new = 0;
    while (i < n) {
        std::size_t end = 0;
        if (fold < folds_.size() && folds_[fold].lines.start == i) {
            emit(Row{row::Fold{fold, folds_[fold].lines.size()}}, Range{i, i});
            end = folds_[fold].lines.end;
            ++fold;
            skip_open();
        } else {
            while (brk < breaks_.size() && breaks_[brk].line < i) ++brk;
            std::optional<std::size_t> section;
            if (brk < breaks_.size() && breaks_[brk].line == i) section = brk++;
            end = std::min(fold < folds_.size() ? folds_[fold].lines.start : n, brk < breaks_.size() ? breaks_[brk].line : n);
            Hunk h{.section = section};
            std::size_t olds = 0, news = 0;
            std::optional<std::uint32_t> first_old, first_new;
            for (std::size_t k = i; k < end; ++k) {
                if (const auto o = lines_[k].old_no) {
                    if (!first_old) first_old = o;
                    ++olds;
                }
                if (const auto w = lines_[k].new_no) {
                    if (!first_new) first_new = w;
                    ++news;
                }
            }
            h.old_start = first_old.value_or(last_old);
            h.old_len = count(olds);
            h.new_start = first_new.value_or(last_new);
            h.new_len = count(news);
            emit(Row{row::Hunk{h}}, Range{i, end});
        }
        // The last old and new numbers before `end`, for the empty side of a hunk.
        bool got_old = false, got_new = false;
        for (std::size_t k = end; k-- > 0 && !(got_old && got_new);) {
            if (!got_old && lines_[k].old_no) last_old = *lines_[k].old_no, got_old = true;
            if (!got_new && lines_[k].new_no) last_new = *lines_[k].new_no, got_new = true;
        }
        i = end;
    }
}

std::vector<Row> FileDiff::unified_rows() const {
    std::vector<Row> rows;
    rows.reserve(lines_.size() + breaks_.size() + 1);
    walk([&](Row head, Range lines) {
        rows.push_back(head);
        for (auto k = lines.start; k < lines.end; ++k) rows.push_back(row::Line{k});
    });
    return rows;
}

std::vector<Row> FileDiff::split_rows() const {
    std::vector<Row> rows;
    walk([&](Row head, Range lines) {
        rows.push_back(head);
        auto k = lines.start;
        while (k < lines.end) {
            const bool equal = lines_[k].tag == Tag::Equal;
            auto stop = k;
            while (stop < lines.end && (lines_[stop].tag == Tag::Equal) == equal) ++stop;
            std::vector<std::size_t> old, ins;
            for (auto j = k; j < stop; ++j) (lines_[j].tag != Tag::Insert ? old : ins).push_back(j);
            const auto& nw = equal ? old : ins;
            for (std::size_t p = 0; p < std::max(old.size(), nw.size()); ++p) {
                row::Pair pair;
                if (p < old.size()) pair.old_line = old[p];
                if (p < nw.size()) pair.new_line = nw[p];
                rows.push_back(pair);
            }
            k = stop;
        }
    });
    return rows;
}

void Builder::push(Tag tag, std::optional<std::uint32_t> old_no, std::optional<std::uint32_t> new_no, std::string_view text) {
    if (tag == Tag::Equal || (tag == Tag::Delete && !ins_.empty())) flush_block();
    auto& f = file_;
    const auto start = f.text_.size(), i = f.lines_.size(), e = f.emph_.size();
    f.text_.append(text);
    f.lines_.push_back(Line{tag, old_no, new_no, Range{start, f.text_.size()}, Range{e, e}});
    if (tag == Tag::Delete) dels_.push_back(i);
    else if (tag == Tag::Insert) ins_.push_back(i);
}

void Builder::hunk_break(std::string_view section) {
    flush_block();
    auto& f = file_;
    const auto start = f.text_.size();
    f.text_.append(section);
    f.breaks_.push_back({f.lines_.size(), Range{start, f.text_.size()}});
}

void Builder::flush_block() {
    auto& f = file_;
    for (std::size_t k = 0; k < std::min(dels_.size(), ins_.size()); ++k) {
        const std::array<std::size_t, 2> pair{dels_[k], ins_[k]};
        auto ranges = intra_line(f.line_text(pair[0]), f.line_text(pair[1]));
        if (!ranges) continue;
        for (std::size_t s = 0; s < 2; ++s) {
            const auto start = f.emph_.size();
            f.emph_.insert(f.emph_.end(), (*ranges)[s].begin(), (*ranges)[s].end());
            f.lines_[pair[s]].emph = Range{start, f.emph_.size()};
        }
    }
    dels_.clear();
    ins_.clear();
}

FileDiff Builder::finish(std::optional<std::string> old_path, std::optional<std::string> new_path, bool binary) {
    flush_block();
    auto f = std::move(file_);
    f.old_path = std::move(old_path);
    f.new_path = std::move(new_path);
    f.binary = binary;
    const auto& lines = f.lines_;
    // Folds every run of unchanged lines farther than `CONTEXT` from a change or a hunk edge.
    std::size_t brk = 0, i = 0;
    while (i < lines.size()) {
        const auto start = i;
        while (brk < f.breaks_.size() && f.breaks_[brk].line <= start) ++brk;
        // A run of unchanged lines also ends at a hunk break.
        const auto limit = brk < f.breaks_.size() ? f.breaks_[brk].line : lines.size();
        while (i < limit && lines[i].tag == Tag::Equal) ++i;
        const Range hidden{start + CONTEXT, i > CONTEXT ? i - CONTEXT : 0};
        if (hidden.size() >= MIN_FOLD) f.folds_.push_back({hidden, false});
        i = std::max(i, start + 1);
    }
    for (const auto& l : lines) {
        if (l.tag == Tag::Insert) ++f.added_;
        if (l.tag == Tag::Delete) ++f.removed_;
        f.max_line_no_ = std::max({f.max_line_no_, l.old_no.value_or(0), l.new_no.value_or(0)});
    }
    return f;
}

} // namespace loa::ui::panel::diff
