// A port of `similar` 3.2.1's Myers diff (algorithms/myers.rs, preflight.rs, compact.rs,
// replace.rs) without deadlines. Comments name the Rust function each piece mirrors.
// Index arithmetic keeps Rust's `usize` wrap-around where the original relies on it.

#include "loa/runtime/diff.hpp"

#include <algorithm>
#include <cassert>
#include <cmath>
#include <limits>
#include <optional>
#include <string_view>
#include <unordered_map>
#include <utility>
#include <variant>

#include "utf8.hpp"

namespace loa::runtime::diff {

namespace {

using Seq = std::span<const std::uint32_t>;
using isize = std::ptrdiff_t;
constexpr std::size_t kMax = std::numeric_limits<std::size_t>::max();

std::size_t sat_add(std::size_t a, std::size_t b) { return a > kMax - b ? kMax : a + b; }
std::size_t sat_sub(std::size_t a, std::size_t b) { return a > b ? a - b : 0; }
std::size_t sat_mul(std::size_t a, std::size_t b) { return b != 0 && a > kMax / b ? kMax : a * b; }
std::size_t isqrt(std::size_t v) {
    auto r = static_cast<std::size_t>(std::sqrt(static_cast<long double>(v)));
    while (r * r > v) --r;
    while ((r + 1) * (r + 1) <= v) ++r;
    return r;
}
std::size_t uabs(isize v) { return v < 0 ? static_cast<std::size_t>(-v) : static_cast<std::size_t>(v); }

struct Range {
    std::size_t start, end;
    [[nodiscard]] std::size_t len() const { return end - start; }
    [[nodiscard]] bool empty() const { return !(start < end); }
};

// The hook chain's input: every op the algorithm reports, in order.
class Sink {
  public:
    virtual ~Sink() = default;
    virtual void equal(std::size_t old_index, std::size_t new_index, std::size_t len) = 0;
    virtual void del(std::size_t old_index, std::size_t old_len, std::size_t new_index) = 0;
    virtual void ins(std::size_t old_index, std::size_t new_index, std::size_t new_len) = 0;
    virtual void finish() {}
};

// Compact's collecting half: ops are cleaned up once the whole diff is known.
class Collect final : public Sink {
  public:
    std::vector<Op> ops;
    void equal(std::size_t o, std::size_t n, std::size_t len) override { ops.push_back({Tag::equal, o, len, n, len}); }
    void del(std::size_t o, std::size_t len, std::size_t n) override { ops.push_back({Tag::delete_, o, len, n, 0}); }
    void ins(std::size_t o, std::size_t n, std::size_t len) override { ops.push_back({Tag::insert, o, 0, n, len}); }
};

// utils::common_prefix_len / common_suffix_len
std::size_t common_prefix_len(Seq old, Range o, Seq nw, Range n) {
    if (o.empty() || n.empty()) return 0;
    std::size_t k = 0;
    while (o.start + k < o.end && n.start + k < n.end && nw[n.start + k] == old[o.start + k]) ++k;
    return k;
}
std::size_t common_suffix_len(Seq old, Range o, Seq nw, Range n) {
    if (o.empty() || n.empty()) return 0;
    std::size_t k = 0;
    while (k < o.len() && k < n.len() && nw[n.end - 1 - k] == old[o.end - 1 - k]) ++k;
    return k;
}

// ---- preflight.rs ----

constexpr std::size_t DISJOINT_FAST_PATH_MIN_LEN = 512;
constexpr std::size_t DISJOINT_FAST_PATH_MIN_WORK = 128 * 1024;
constexpr std::size_t DISJOINT_FAST_PATH_BOUNDARY_PROBE = 8;
constexpr std::size_t CONFUSING_RECORD_SCAN_WINDOW = 100;
constexpr std::size_t CONFUSING_RECORD_RATIO = 4;
constexpr std::size_t MAX_MATCH_FREQUENCY_THRESHOLD = 1024;
constexpr std::size_t NO_MATCH_RECORD_ID = kMax;

struct Trimmed {
    Range old_range, new_range, original_old_range, original_new_range;
    std::size_t prefix_len, suffix_len;
};

struct Reduced {
    std::vector<std::uint32_t> old_values;
    std::vector<std::size_t> old_indices;
    std::vector<std::uint32_t> new_values;
    std::vector<std::size_t> new_indices;
    Trimmed trimmed;
};

enum class Action { discard, keep, investigate };

bool should_scan(Seq old, Range o, Seq nw, Range n) {
    const auto ol = o.len(), nl = n.len();
    if (ol < DISJOINT_FAST_PATH_MIN_LEN || nl < DISJOINT_FAST_PATH_MIN_LEN || sat_mul(ol, nl) < DISJOINT_FAST_PATH_MIN_WORK) return false;
    if (nw[n.start] == old[o.start] || nw[n.end - 1] == old[o.end - 1]) return false;
    const auto probe = std::min({ol, nl, DISJOINT_FAST_PATH_BOUNDARY_PROBE + 1});
    for (std::size_t skip = 1; skip < probe; ++skip) {
        if (nw[n.start + skip] == old[o.start] || nw[n.start] == old[o.start + skip] || nw[n.end - 1 - skip] == old[o.end - 1] ||
            nw[n.end - 1] == old[o.end - 1 - skip])
            return false;
    }
    return true;
}

std::vector<Action> record_actions(const std::vector<std::size_t>& ids, const std::vector<std::size_t>& opposite) {
    const auto limit = std::clamp<std::size_t>(isqrt(ids.size()), 1, MAX_MATCH_FREQUENCY_THRESHOLD);
    std::vector<Action> out;
    out.reserve(ids.size());
    for (const auto id : ids) {
        if (id == NO_MATCH_RECORD_ID || opposite[id] == 0) out.push_back(Action::discard);
        else out.push_back(opposite[id] < limit ? Action::keep : Action::investigate);
    }
    return out;
}

std::pair<std::size_t, std::size_t> scan_confusing_run(const std::vector<Action>& actions, std::size_t from, bool backwards) {
    std::size_t discards = 0, investigates = 0;
    for (std::size_t step = 0; step < CONFUSING_RECORD_SCAN_WINDOW; ++step) {
        std::size_t index;
        if (backwards) {
            if (step >= from) break;
            index = from - 1 - step;
        } else {
            index = from + step;
            if (index >= actions.size()) break;
        }
        if (actions[index] == Action::discard) ++discards;
        else if (actions[index] == Action::investigate) ++investigates;
        else break;
    }
    return {discards, investigates};
}

bool discard_confusing_record(const std::vector<Action>& actions, std::size_t index) {
    const auto [ld, li] = scan_confusing_run(actions, index, true);
    if (ld == 0) return false;
    const auto [rd, ri] = scan_confusing_run(actions, index + 1, false);
    if (rd == 0) return false;
    const auto discards = ld + rd;
    const auto investigates = li + ri + 1;
    return sat_mul(investigates, CONFUSING_RECORD_RATIO) < investigates + discards;
}

bool keep_record(const std::vector<Action>& actions, std::size_t index) {
    switch (actions[index]) {
    case Action::discard: return false;
    case Action::keep: return true;
    case Action::investigate: return !discard_confusing_record(actions, index);
    }
    return false;
}

// reduce_for_myers: `Reduced` when records can be dropped, else only the trim.
std::variant<Trimmed, Reduced> reduce_for_myers(Seq old, Range oor, Seq nw, Range onr) {
    const auto prefix = common_prefix_len(old, oor, nw, onr);
    const Range oap{oor.start + prefix, oor.end}, nap{onr.start + prefix, onr.end};
    const auto suffix = common_suffix_len(old, oap, nw, nap);
    const Range o{oap.start, oap.end - suffix}, n{nap.start, nap.end - suffix};
    Trimmed trimmed{o, n, oor, onr, prefix, suffix};
    if (!should_scan(old, o, nw, n)) return trimmed;

    std::unordered_map<std::uint32_t, std::size_t> ids;
    std::vector<std::size_t> old_values, new_values, old_counts, new_counts;
    old_values.reserve(o.len());
    new_values.reserve(n.len());
    for (auto i = o.start; i < o.end; ++i) {
        auto [it, fresh] = ids.try_emplace(old[i], old_counts.size());
        if (fresh) {
            old_counts.push_back(0);
            new_counts.push_back(0);
        }
        ++old_counts[it->second];
        old_values.push_back(it->second);
    }
    for (auto i = n.start; i < n.end; ++i) {
        const auto it = ids.find(nw[i]);
        const auto id = it == ids.end() ? NO_MATCH_RECORD_ID : it->second;
        if (id != NO_MATCH_RECORD_ID) ++new_counts[id];
        new_values.push_back(id);
    }

    Reduced r;
    const auto old_actions = record_actions(old_values, new_counts);
    for (std::size_t k = 0; k < old_values.size(); ++k)
        if (keep_record(old_actions, k)) {
            r.old_values.push_back(static_cast<std::uint32_t>(old_values[k]));
            r.old_indices.push_back(o.start + k);
        }
    const auto new_actions = record_actions(new_values, old_counts);
    for (std::size_t k = 0; k < new_values.size(); ++k)
        if (keep_record(new_actions, k)) {
            r.new_values.push_back(static_cast<std::uint32_t>(new_values[k]));
            r.new_indices.push_back(n.start + k);
        }
    if (r.old_indices.size() == o.len() && r.new_indices.size() == n.len()) return trimmed;
    r.trimmed = trimmed;
    return r;
}

// ---- myers.rs ----

enum class Mode { heuristic, raw };

class V {
  public:
    explicit V(std::size_t max_d) : offset_(static_cast<isize>(max_d)), v_(2 * max_d, 0) {}
    std::size_t& operator[](isize k) { return v_[static_cast<std::size_t>(k + offset_)]; }
    [[nodiscard]] std::size_t size() const { return v_.size(); }

  private:
    isize offset_;
    std::vector<std::size_t> v_;
};

std::size_t max_d(std::size_t a, std::size_t b) { return (a + b + 1) / 2 + 1; }

constexpr std::size_t SMALL_SIDE_EXACT_MAX = 64;
constexpr std::size_t SMALL_SIDE_EXACT_MIN_LARGE = 512;
constexpr std::size_t SMALL_SIDE_EXACT_MAX_WORK = 64'000'000;
constexpr std::size_t HEURISTIC_MIN_COST = 256;
constexpr std::size_t HEURISTIC_MIN_SNAKE = 20;
constexpr std::size_t HEURISTIC_MIN_MAX_COST = 256;
constexpr std::size_t HEURISTIC_PROGRESS_FACTOR = 4;

std::size_t prefix_at(Seq old, std::size_t os, std::size_t oe, Seq nw, std::size_t ns, std::size_t ne) {
    const auto max_len = std::min(oe - os, ne - ns);
    std::size_t m = 0;
    while (m < max_len && nw[ns + m] == old[os + m]) ++m;
    return m;
}
std::size_t suffix_at(Seq old, std::size_t os, std::size_t oe, Seq nw, std::size_t ns, std::size_t ne) {
    const auto max_len = std::min(oe - os, ne - ns);
    std::size_t m = 0;
    while (m < max_len && nw[ne - 1 - m] == old[oe - 1 - m]) ++m;
    return m;
}

// emit_small_old_exact (old is the short side) and emit_small_new_exact, merged: `a` indexes
// the short side's sequence, `b` the long side's.
void emit_small_exact(Sink& d, Seq old, Range o, Seq nw, Range nr, bool old_small) {
    const auto n = old_small ? o.len() : nr.len();  // short side length
    const auto m = old_small ? nr.len() : o.len();  // long side length
    const auto short_at = [&](std::size_t i) { return old_small ? old[o.start + i] : nw[nr.start + i]; };
    const auto long_at = [&](std::size_t j) { return old_small ? nw[nr.start + j] : old[o.start + j]; };
    const auto width = m + 1;
    std::vector<std::uint8_t> dp((n + 1) * width, 0);
    for (std::size_t i = n; i-- > 0;) {
        const auto row = i * width, next = (i + 1) * width;
        for (std::size_t j = m; j-- > 0;)
            dp[row + j] = long_at(j) == short_at(i) ? static_cast<std::uint8_t>(dp[next + j + 1] + 1) : std::max(dp[next + j], dp[row + j + 1]);
    }
    const auto diag = [&](std::size_t i, std::size_t j) {
        return long_at(j) == short_at(i) && dp[i * width + j] == dp[(i + 1) * width + j + 1] + 1;
    };
    // In Rust's old-small variant `i` walks old and `j` new; in the new-small variant `i`
    // walks new and `j` old, and deletes/inserts swap roles.
    const auto eq = [&](std::size_t si, std::size_t li, std::size_t len) {
        if (old_small) d.equal(o.start + si, nr.start + li, len);
        else d.equal(o.start + li, nr.start + si, len);
    };
    std::size_t i = 0, j = 0;
    while (i < n && j < m) {
        if (diag(i, j)) {
            const auto si = i, sj = j;
            while (i < n && j < m && diag(i, j)) {
                ++i;
                ++j;
            }
            if (old_small) eq(si, sj, i - si);
            else d.equal(o.start + sj, nr.start + si, j - sj);
        } else if (dp[(i + 1) * width + j] >= dp[i * width + j + 1]) {
            const auto si = i;
            while (i < n) {
                if (diag(i, j)) break;
                if (dp[(i + 1) * width + j] >= dp[i * width + j + 1]) ++i;
                else break;
            }
            if (old_small) d.del(o.start + si, i - si, nr.start + j);
            else d.ins(o.start + j, nr.start + si, i - si);
        } else {
            const auto sj = j;
            while (j < m) {
                if (diag(i, j)) break;
                if (dp[(i + 1) * width + j] < dp[i * width + j + 1]) ++j;
                else break;
            }
            if (old_small) d.ins(o.start + i, nr.start + sj, j - sj);
            else d.del(o.start + sj, j - sj, nr.start + i);
        }
    }
    if (old_small) {
        if (i < n) d.del(o.start + i, n - i, nr.start + j);
        if (j < m) d.ins(o.start + i, nr.start + j, m - j);
    } else {
        if (j < m) d.del(o.start + j, m - j, nr.start + i);
        if (i < n) d.ins(o.start + j, nr.start + i, n - i);
    }
}

bool maybe_emit_small_side_exact(Sink& d, Seq old, Range o, Seq nw, Range n) {
    const auto ol = o.len(), nl = n.len();
    const auto small = std::min(ol, nl), large = std::max(ol, nl);
    if (small == 0 || small > SMALL_SIDE_EXACT_MAX || large < SMALL_SIDE_EXACT_MIN_LARGE || sat_mul(ol, nl) > SMALL_SIDE_EXACT_MAX_WORK)
        return false;
    emit_small_exact(d, old, o, nw, n, ol <= nl);
    return true;
}

void try_emit_front_anchor(Sink& d, Seq old, Range& o, Seq nw, Range& n) {
    constexpr std::size_t MAX_SKIP = 4, MIN_ANCHOR_COMMON = 96;
    const auto ol = o.len(), nl = n.len();
    if (ol <= MIN_ANCHOR_COMMON || nl <= MIN_ANCHOR_COMMON) return;
    const auto small = std::min(ol, nl), large = std::max(ol, nl);
    if (large < sat_mul(small, 2)) return;
    const auto max_old_skip = std::min(sat_sub(ol, 1), MAX_SKIP), max_new_skip = std::min(sat_sub(nl, 1), MAX_SKIP);
    const bool prefer_insert = nl > ol, prefer_delete = ol > nl;
    struct Best {
        std::size_t old_skip, new_skip, common;
    };
    std::optional<Best> best;
    for (std::size_t os = 0; os <= max_old_skip; ++os) {
        bool done = false;
        for (std::size_t ns = 0; ns <= max_new_skip; ++ns) {
            if ((os == 0 && ns == 0) || (os != 0 && ns != 0)) continue;
            if ((prefer_insert && os != 0) || (prefer_delete && ns != 0)) continue;
            if (nw[n.start + ns] != old[o.start + os]) continue;
            const auto common = prefix_at(old, o.start + os, o.end, nw, n.start + ns, n.end);
            if (common < MIN_ANCHOR_COMMON) continue;
            if (common >= MIN_ANCHOR_COMMON * 8) {
                best = Best{os, ns, common};
                done = true;
                break;
            }
            // (common, Reverse(span), old_skip, new_skip) compared lexicographically.
            bool better = !best;
            if (best) {
                const auto span = std::max(os, ns), best_span = std::max(best->old_skip, best->new_skip);
                if (common != best->common) better = common > best->common;
                else if (span != best_span) better = span < best_span;
                else if (os != best->old_skip) better = os > best->old_skip;
                else better = ns > best->new_skip;
            }
            if (better) best = Best{os, ns, common};
        }
        if (done) break;
    }
    if (!best) return;
    if (best->old_skip == 0 && best->new_skip != 0) d.ins(o.start, n.start, best->new_skip);
    else if (best->old_skip != 0 && best->new_skip == 0) d.del(o.start, best->old_skip, n.start);
    d.equal(o.start + best->old_skip, n.start + best->new_skip, best->common);
    o.start += best->old_skip + best->common;
    n.start += best->new_skip + best->common;
}

enum class Direction { forward, backward };
struct HeuristicSplit {
    std::size_t old_index, new_index, progress;
    Direction direction;
};
struct MiddleSnakeSplit {
    std::size_t old_index, new_index;
    Mode left_mode, right_mode;
};

MiddleSnakeSplit heuristic_split(const HeuristicSplit& s, std::size_t os, std::size_t ns) {
    return s.direction == Direction::forward ? MiddleSnakeSplit{os + s.old_index, ns + s.new_index, Mode::raw, Mode::heuristic}
                                             : MiddleSnakeSplit{os + s.old_index, ns + s.new_index, Mode::heuristic, Mode::raw};
}

void update_best_split(std::optional<HeuristicSplit>& best, const HeuristicSplit& c) {
    if (!best || c.progress > best->progress) best = c;
}

bool is_interior_split(std::size_t oi, std::size_t ni, std::size_t ol, std::size_t nl) {
    return oi <= ol && ni <= nl && (oi != 0 || ni != 0) && (oi != ol || ni != nl);
}

std::optional<std::pair<std::size_t, std::size_t>> clamp_diagonal_point(std::size_t oi, isize k, std::size_t ol, std::size_t nl) {
    if (k < 0 && uabs(k) > nl) return std::nullopt;
    const auto min_old = static_cast<std::size_t>(std::max<isize>(k, 0));
    const auto max_old = k >= 0 ? std::min(ol, sat_add(nl, static_cast<std::size_t>(k))) : std::min(ol, sat_sub(nl, uabs(k)));
    if (min_old > max_old) return std::nullopt;
    const auto x = std::clamp(oi, min_old, max_old);
    return std::pair{x, static_cast<std::size_t>(static_cast<isize>(x) - k)};
}

std::size_t heuristic_max_cost(std::size_t ol, std::size_t nl) { return std::max(isqrt(sat_add(ol, nl)), HEURISTIC_MIN_MAX_COST); }

std::optional<MiddleSnakeSplit> find_middle_snake(Seq old, Range o, Seq nw, Range nr, V& vf, V& vb, Mode mode) {
    const auto n = o.len(), m = nr.len();
    const isize delta = static_cast<isize>(n) - static_cast<isize>(m);
    const bool odd = (delta & 1) == 1;
    vf[1] = 0;
    vb[1] = 0;
    const auto d_max = max_d(n, m);
    assert(vf.size() >= d_max && vb.size() >= d_max);
    const auto max_cost = mode == Mode::heuristic ? heuristic_max_cost(n, m) : kMax;

    for (isize d = 0; d < static_cast<isize>(d_max); ++d) {
        const auto du = static_cast<std::size_t>(d);
        std::optional<HeuristicSplit> good, fwd, bwd;
        const bool track_limit = mode == Mode::heuristic && du >= max_cost;
        const bool track_good = mode == Mode::heuristic && du > HEURISTIC_MIN_COST;

        for (isize k = d; k >= -d; k -= 2) {
            std::size_t x = (k == -d || (k != d && vf[k - 1] < vf[k + 1])) ? vf[k + 1] : vf[k - 1] + 1;
            const std::size_t y = x - static_cast<std::size_t>(k);
            const auto x0 = x, y0 = y;
            if (x < n && y < m) x += prefix_at(old, o.start + x, o.end, nw, nr.start + y, nr.end);
            const auto snake = x - x0;
            vf[k] = x;
            if (odd && std::abs(k - delta) <= d - 1 && vf[k] + vb[-(k - delta)] >= n)
                return MiddleSnakeSplit{x0 + o.start, y0 + nr.start, Mode::raw, Mode::raw};
            if (track_limit)
                if (auto p = clamp_diagonal_point(x, k, n, m); p && is_interior_split(p->first, p->second, n, m))
                    update_best_split(fwd, {p->first, p->second, sat_add(p->first, p->second), Direction::forward});
            if (track_good && snake >= HEURISTIC_MIN_SNAKE && x < n && y < m) {
                const auto useful = sat_sub(sat_add(x, y), uabs(k));
                if (useful > sat_mul(HEURISTIC_PROGRESS_FACTOR, du) && is_interior_split(x0, y0, n, m))
                    update_best_split(good, {x0, y0, useful, Direction::forward});
            }
        }

        for (isize k = d; k >= -d; k -= 2) {
            std::size_t x = (k == -d || (k != d && vb[k - 1] < vb[k + 1])) ? vb[k + 1] : vb[k - 1] + 1;
            std::size_t y = x - static_cast<std::size_t>(k);
            const auto x0 = x;
            if (x < n && y < m) {
                const auto adv = suffix_at(old, o.start, o.start + n - x, nw, nr.start, nr.start + m - y);
                x += adv;
                y += adv;
            }
            const auto snake = x - x0;
            vb[k] = x;
            if (!odd && std::abs(k - delta) <= d && vb[k] + vf[-(k - delta)] >= n)
                return MiddleSnakeSplit{n - x + o.start, m - y + nr.start, Mode::raw, Mode::raw};
            if (track_limit)
                if (auto p = clamp_diagonal_point(x, k, n, m)) {
                    const auto so = n - p->first, sn = m - p->second;
                    if (is_interior_split(so, sn, n, m))
                        update_best_split(bwd, {so, sn, sat_add(p->first, p->second), Direction::backward});
                }
            if (track_good && snake >= HEURISTIC_MIN_SNAKE && x <= n && y <= m) {
                const auto so = n - x, sn = m - y;
                const auto useful = sat_sub(sat_add(x, y), uabs(k));
                if (so > 0 && sn > 0 && useful > sat_mul(HEURISTIC_PROGRESS_FACTOR, du))
                    update_best_split(good, {so, sn, useful, Direction::backward});
            }
        }

        if (good) return heuristic_split(*good, o.start, nr.start);
        if (du >= max_cost) {
            if (fwd && bwd) return heuristic_split(fwd->progress > bwd->progress ? *fwd : *bwd, o.start, nr.start);
            if (fwd) return heuristic_split(*fwd, o.start, nr.start);
            if (bwd) return heuristic_split(*bwd, o.start, nr.start);
            break;
        }
    }
    return std::nullopt;
}

void conquer(Sink& d, Seq old, Range o, Seq nw, Range n, V& vf, V& vb, Mode mode) {
    const auto prefix = common_prefix_len(old, o, nw, n);
    if (prefix > 0) d.equal(o.start, n.start, prefix);
    o.start += prefix;
    n.start += prefix;
    const auto suffix = common_suffix_len(old, o, nw, n);
    const auto suffix_at_old = o.end - suffix, suffix_at_new = n.end - suffix;
    o.end -= suffix;
    n.end -= suffix;

    while (o.start < o.end && n.start < n.end) {
        const auto ob = o.start, nb = n.start;
        try_emit_front_anchor(d, old, o, nw, n);
        if (o.start == ob && n.start == nb) break;
    }

    if (o.empty() && n.empty()) {
    } else if (n.empty()) {
        d.del(o.start, o.len(), n.start);
    } else if (o.empty()) {
        d.ins(o.start, n.start, n.len());
    } else if (maybe_emit_small_side_exact(d, old, o, nw, n)) {
    } else if (auto split = find_middle_snake(old, o, nw, n, vf, vb, mode)) {
        conquer(d, old, {o.start, split->old_index}, nw, {n.start, split->new_index}, vf, vb, split->left_mode);
        conquer(d, old, {split->old_index, o.end}, nw, {split->new_index, n.end}, vf, vb, split->right_mode);
    } else {
        d.del(o.start, o.end - o.start, n.start);
        d.ins(o.start, n.start, n.end - n.start);
    }

    if (suffix > 0) d.equal(suffix_at_old, suffix_at_new, suffix);
}

// diff_deadline_pretrimmed
void diff_pretrimmed(Sink& d, Seq old, Seq nw, const Trimmed& t, Mode mode) {
    if (t.prefix_len > 0) d.equal(t.original_old_range.start, t.original_new_range.start, t.prefix_len);
    if (t.old_range.empty() && t.new_range.empty()) {
    } else if (t.old_range.empty()) {
        d.ins(t.old_range.start, t.new_range.start, t.new_range.len());
    } else if (t.new_range.empty()) {
        d.del(t.old_range.start, t.old_range.len(), t.new_range.start);
    } else {
        const auto md = max_d(t.old_range.len(), t.new_range.len());
        V vb(md), vf(md);
        conquer(d, old, t.old_range, nw, t.new_range, vf, vb, mode);
    }
    if (t.suffix_len > 0) d.equal(t.original_old_range.end - t.suffix_len, t.original_new_range.end - t.suffix_len, t.suffix_len);
    d.finish();
}

// diff_deadline_impl
void diff_impl(Sink& d, Seq old, Range o, Seq nw, Range n, Mode mode) {
    const auto prefix = common_prefix_len(old, o, nw, n);
    const Range oap{o.start + prefix, o.end}, nap{n.start + prefix, n.end};
    const auto suffix = common_suffix_len(old, oap, nw, nap);
    diff_pretrimmed(d, old, nw, {{oap.start, oap.end - suffix}, {nap.start, nap.end - suffix}, o, n, prefix, suffix}, mode);
}

// Maps ops over the reduced sequences back to original indices; only equal runs are
// trusted, the gaps between them become deletes and inserts.
class ReducedSink final : public Sink {
  public:
    ReducedSink(Sink& d, const Reduced& r)
        : d_(d), old_indices_(r.old_indices), new_indices_(r.new_indices), old_cursor_(r.trimmed.old_range.start),
          new_cursor_(r.trimmed.new_range.start), old_end_(r.trimmed.old_range.end), new_end_(r.trimmed.new_range.end) {}

    void equal(std::size_t oi, std::size_t ni, std::size_t len) override {
        std::size_t off = 0;
        while (off < len) {
            const auto oa = old_indices_[oi + off], na = new_indices_[ni + off];
            emit_gaps(oa, na);
            std::size_t run = 1;
            while (off + run < len && old_indices_[oi + off + run] == oa + run && new_indices_[ni + off + run] == na + run) ++run;
            d_.equal(oa, na, run);
            old_cursor_ = oa + run;
            new_cursor_ = na + run;
            off += run;
        }
    }
    void del(std::size_t, std::size_t, std::size_t) override {}
    void ins(std::size_t, std::size_t, std::size_t) override {}
    void finish() override { emit_gaps(old_end_, new_end_); }

  private:
    void emit_gaps(std::size_t oa, std::size_t na) {
        if (old_cursor_ < oa) {
            d_.del(old_cursor_, oa - old_cursor_, new_cursor_);
            old_cursor_ = oa;
        }
        if (new_cursor_ < na) {
            d_.ins(old_cursor_, new_cursor_, na - new_cursor_);
            new_cursor_ = na;
        }
    }

    Sink& d_;
    const std::vector<std::size_t>& old_indices_;
    const std::vector<std::size_t>& new_indices_;
    std::size_t old_cursor_, new_cursor_, old_end_, new_end_;
};

// myers::diff_deadline with no deadline.
void myers(Sink& d, Seq old, Seq nw) {
    auto pre = reduce_for_myers(old, {0, old.size()}, nw, {0, nw.size()});
    if (auto* t = std::get_if<Trimmed>(&pre)) return diff_pretrimmed(d, old, nw, *t, Mode::heuristic);
    const auto& r = std::get<Reduced>(pre);
    const auto& t = r.trimmed;
    if (t.prefix_len > 0) d.equal(t.original_old_range.start, t.original_new_range.start, t.prefix_len);
    ReducedSink hook(d, r);
    diff_impl(hook, r.old_values, {0, r.old_values.size()}, r.new_values, {0, r.new_values.size()}, Mode::heuristic);
    if (t.suffix_len > 0) d.equal(t.original_old_range.end - t.suffix_len, t.original_new_range.end - t.suffix_len, t.suffix_len);
    d.finish();
}

// ---- compact.rs ----

bool is_empty(const Op& op) { return op.old_len == 0 && op.new_len == 0; }

// DiffOp::adjust: indices move by `offset`; lengths change by `len`.
void adjust(Op& op, isize offset, isize len) {
    op.old_index = static_cast<std::size_t>(static_cast<isize>(op.old_index) + offset);
    op.new_index = static_cast<std::size_t>(static_cast<isize>(op.new_index) + offset);
    if (op.tag != Tag::insert) op.old_len = static_cast<std::size_t>(static_cast<isize>(op.old_len) + len);
    if (op.tag != Tag::delete_) op.new_len = static_cast<std::size_t>(static_cast<isize>(op.new_len) + len);
}
void shift_left(Op& op, std::size_t a) { adjust(op, -static_cast<isize>(a), 0); }
void shift_right(Op& op, std::size_t a) { adjust(op, static_cast<isize>(a), 0); }
void grow_left(Op& op, std::size_t a) { adjust(op, -static_cast<isize>(a), static_cast<isize>(a)); }
void grow_right(Op& op, std::size_t a) { adjust(op, 0, static_cast<isize>(a)); }
void shrink_left(Op& op, std::size_t a) { adjust(op, 0, -static_cast<isize>(a)); }
void shrink_right(Op& op, std::size_t a) { adjust(op, static_cast<isize>(a), -static_cast<isize>(a)); }

Range old_range(const Op& op) { return {op.old_index, op.old_end()}; }
Range new_range(const Op& op) { return {op.new_index, op.new_end()}; }

void swap_adjacent_insert_delete(std::vector<Op>& ops, std::size_t left) {
    const auto right = left + 1;
    const auto old_start = ops[left].old_index, old_end = ops[right].old_end();
    const auto new_start = ops[left].new_index, new_end = ops[right].new_end();
    std::swap(ops[left], ops[right]);
    if (ops[left].tag == Tag::insert) ops[left].old_index = old_start;
    else ops[left].new_index = new_start;
    if (ops[right].tag == Tag::insert) ops[right].old_index = old_end;
    else ops[right].new_index = new_end;
}

std::size_t shift_up(std::vector<Op>& ops, Seq old, Seq nw, std::size_t p) {
    while (p >= 1 && p - 1 < ops.size()) {
        const auto prev = ops[p - 1];
        const auto cur = ops[p];
        const bool cur_ins = cur.tag == Tag::insert, cur_del = cur.tag == Tag::delete_;
        if ((cur_ins || cur_del) && prev.tag == Tag::equal) {
            if (cur_del && !(p >= 2 && ops[p - 2].tag == Tag::delete_)) break;
            const auto suffix = cur_ins ? common_suffix_len(old, old_range(prev), nw, new_range(cur))
                                        : common_suffix_len(old, old_range(cur), nw, new_range(prev));
            if (suffix > 0) {
                if (p + 1 < ops.size() && ops[p + 1].tag == Tag::equal) {
                    grow_left(ops[p + 1], suffix);
                } else {
                    const auto oe = cur_ins ? prev.old_end() : cur.old_end();
                    const auto ne = cur_ins ? cur.new_end() : prev.new_end();
                    ops.insert(ops.begin() + static_cast<isize>(p + 1), Op{Tag::equal, oe - suffix, suffix, ne - suffix, suffix});
                }
                shift_left(ops[p], suffix);
                shrink_left(ops[p - 1], suffix);
                if (is_empty(ops[p - 1])) {
                    ops.erase(ops.begin() + static_cast<isize>(p - 1));
                    --p;
                }
            } else if (is_empty(ops[p - 1])) {
                ops.erase(ops.begin() + static_cast<isize>(p - 1));
                --p;
            } else {
                break;
            }
        } else if ((cur_ins && prev.tag == Tag::delete_) || (cur_del && prev.tag == Tag::insert)) {
            swap_adjacent_insert_delete(ops, p - 1);
            --p;
        } else if (cur_ins && prev.tag == Tag::insert) {
            grow_right(ops[p - 1], cur.new_len);
            ops.erase(ops.begin() + static_cast<isize>(p));
            --p;
        } else if (cur_del && prev.tag == Tag::delete_) {
            grow_right(ops[p - 1], cur.old_len);
            ops.erase(ops.begin() + static_cast<isize>(p));
            --p;
        } else {
            assert(false && "unexpected tag");
            break;
        }
    }
    return p;
}

std::size_t shift_down(std::vector<Op>& ops, Seq old, Seq nw, std::size_t p) {
    while (p + 1 < ops.size()) {
        const auto next = ops[p + 1];
        const auto cur = ops[p];
        const bool cur_ins = cur.tag == Tag::insert, cur_del = cur.tag == Tag::delete_;
        if ((cur_ins || cur_del) && next.tag == Tag::equal) {
            if (cur_del && !(p + 2 < ops.size() && ops[p + 2].tag == Tag::delete_)) break;
            const auto prefix = cur_ins ? common_prefix_len(old, old_range(next), nw, new_range(cur))
                                        : common_prefix_len(old, old_range(cur), nw, new_range(next));
            if (prefix > 0) {
                if (p >= 1 && ops[p - 1].tag == Tag::equal) {
                    grow_right(ops[p - 1], prefix);
                } else {
                    const auto oi = cur_ins ? next.old_index : cur.old_index;
                    ops.insert(ops.begin() + static_cast<isize>(p), Op{Tag::equal, oi, prefix, cur.new_index, prefix});
                    ++p;
                }
                shift_right(ops[p], prefix);
                shrink_right(ops[p + 1], prefix);
                if (is_empty(ops[p + 1])) ops.erase(ops.begin() + static_cast<isize>(p + 1));
            } else if (is_empty(ops[p + 1])) {
                ops.erase(ops.begin() + static_cast<isize>(p + 1));
            } else {
                break;
            }
        } else if ((cur_ins && next.tag == Tag::delete_) || (cur_del && next.tag == Tag::insert)) {
            swap_adjacent_insert_delete(ops, p);
            ++p;
        } else if (cur_ins && next.tag == Tag::insert) {
            grow_right(ops[p], next.new_len);
            ops.erase(ops.begin() + static_cast<isize>(p + 1));
        } else if (cur_del && next.tag == Tag::delete_) {
            grow_right(ops[p], next.old_len);
            ops.erase(ops.begin() + static_cast<isize>(p + 1));
        } else {
            assert(false && "unexpected tag");
            break;
        }
    }
    return p;
}

void normalize_cursors(std::vector<Op>& ops) {
    std::size_t oc = 0, nc = 0;
    if (!ops.empty()) {
        oc = ops.front().old_index;
        nc = ops.front().new_index;
    }
    for (auto& op : ops) {
        switch (op.tag) {
        case Tag::equal:
        case Tag::replace:
            oc = op.old_end();
            nc = op.new_end();
            break;
        case Tag::delete_:
            op.new_index = nc;
            oc = op.old_end();
            break;
        case Tag::insert:
            op.old_index = oc;
            nc = op.new_end();
            break;
        }
    }
}

void cleanup(Seq old, Seq nw, std::vector<Op>& ops) {
    for (auto tag : {Tag::delete_, Tag::insert}) {
        for (std::size_t p = 0; p < ops.size(); ++p) {
            if (ops[p].tag != tag) continue;
            p = shift_up(ops, old, nw, p);
            p = shift_down(ops, old, nw, p);
        }
    }
    normalize_cursors(ops);
}

// ---- replace.rs ----

std::vector<Op> group_replaces(const std::vector<Op>& in) {
    std::vector<Op> out;
    std::optional<Op> del, ins, eq;
    const auto flush_eq = [&] {
        if (eq) out.push_back(*std::exchange(eq, std::nullopt));
    };
    const auto flush_del_ins = [&] {
        if (del) {
            if (ins) out.push_back({Tag::replace, del->old_index, del->old_len, ins->new_index, ins->new_len});
            else out.push_back(*del);
        } else if (ins) {
            out.push_back(*ins);
        }
        del.reset();
        ins.reset();
    };
    for (const auto& op : in) {
        switch (op.tag) {
        case Tag::equal:
            flush_del_ins();
            if (eq) {
                eq->old_len += op.old_len;
                eq->new_len += op.new_len;
            } else {
                eq = op;
            }
            break;
        case Tag::delete_:
            flush_eq();
            if (del) del->old_len += op.old_len;
            else del = op;
            break;
        case Tag::insert:
            flush_eq();
            if (ins) ins->new_len += op.new_len;
            else ins = op;
            break;
        case Tag::replace:
            flush_eq();
            out.push_back(op);
            break;
        }
    }
    flush_eq();
    flush_del_ins();
    return out;
}

// ---- tokenizers (text/abstraction.rs) ----

std::vector<std::string_view> tokenize_lines(std::string_view s) {
    std::vector<std::string_view> out;
    std::size_t last = 0;
    for (std::size_t i = 0; i < s.size(); ++i) {
        if (s[i] != '\n' && s[i] != '\r') continue;
        const auto end = (s[i] == '\r' && i + 1 < s.size() && s[i + 1] == '\n') ? i + 2 : i + 1;
        out.push_back(s.substr(last, end - last));
        last = end;
        i = end - 1;
    }
    if (last < s.size()) out.push_back(s.substr(last));
    return out;
}

std::vector<std::string_view> tokenize_words(std::string_view s) {
    std::vector<std::string_view> out;
    std::size_t start = 0, pos = 0;
    bool current_ws = false;
    while (pos < s.size()) {
        std::size_t width = 1;
        const bool ws = detail::is_whitespace(detail::decode(s, pos, width));
        if (ws != current_ws) {
            if (pos > start) out.push_back(s.substr(start, pos - start));
            start = pos;
            current_ws = ws;
        }
        pos += width;
    }
    if (start < s.size()) out.push_back(s.substr(start));
    return out;
}

TextDiff diff_tokens(std::vector<std::string_view> old_tokens, std::vector<std::string_view> new_tokens) {
    std::unordered_map<std::string_view, std::uint32_t> ids;
    std::vector<std::uint32_t> a, b;
    a.reserve(old_tokens.size());
    b.reserve(new_tokens.size());
    for (const auto t : old_tokens) a.push_back(ids.try_emplace(t, static_cast<std::uint32_t>(ids.size())).first->second);
    for (const auto t : new_tokens) b.push_back(ids.try_emplace(t, static_cast<std::uint32_t>(ids.size())).first->second);
    TextDiff out{std::move(old_tokens), std::move(new_tokens), diff_slices(a, b)};
    return out;
}

} // namespace

std::vector<Op> diff_slices(std::span<const std::uint32_t> old, std::span<const std::uint32_t> new_) {
    Collect collect;
    myers(collect, old, new_);
    cleanup(old, new_, collect.ops);
    return group_replaces(collect.ops);
}

float TextDiff::ratio() const noexcept {
    std::size_t matches = 0;
    for (const auto& op : ops)
        if (op.tag == Tag::equal) matches += op.old_len;
    const auto len = old_tokens.size() + new_tokens.size();
    return len == 0 ? 1.0f : 2.0f * static_cast<float>(matches) / static_cast<float>(len);
}

TextDiff diff_lines(std::string_view old, std::string_view new_) { return diff_tokens(tokenize_lines(old), tokenize_lines(new_)); }
TextDiff diff_words(std::string_view old, std::string_view new_) { return diff_tokens(tokenize_words(old), tokenize_words(new_)); }

} // namespace loa::runtime::diff
