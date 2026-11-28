#include "loa/projects/rank.hpp"

#include <algorithm>
#include <array>
#include <cmath>
#include <limits>

#include "loa/core/schema.hpp"
#include "loa/projects/context.hpp"
#include "loa/projects/settings.hpp"
#include "loa/projects/text.hpp"

namespace loa::projects {

namespace attr = core::attr;

std::optional<Candidate> Candidate::from_dom(const core::Dom& dom) {
    const auto* root = dom.root();
    if (!root) return std::nullopt;
    auto id = root->str_attr(attr::ID);
    if (!id) return std::nullopt;
    auto digest = digest_of(dom);
    if (!digest) return std::nullopt;
    std::optional<std::string> title;
    if (auto t = root->str_attr(attr::TITLE)) title = std::string(*t);
    return Candidate{std::string(*id), std::move(title), std::move(*digest), projects::last_active(dom), is_excluded(dom)};
}

std::vector<std::string_view> Candidate::fields() const {
    const auto& d = digest.content;
    std::vector<std::string_view> out;
    if (title) out.emplace_back(*title);
    out.emplace_back(d.summary);
    for (const auto& s : d.decisions) out.emplace_back(s);
    for (const auto& s : d.open_items) out.emplace_back(s);
    for (const auto& f : d.files) {
        out.emplace_back(f.path);
        out.emplace_back(f.note);
    }
    return out;
}

std::int64_t last_active(const core::Dom& dom) {
    const auto* root = dom.root();
    if (!root) return 0;
    auto latest = root->int_attr(attr::CREATED).value_or(0);
    for (const auto* t : dom.children_of_kind(root->id, core::ElementKind::Turn)) {
        if (auto s = t->int_attr(attr::STARTED)) latest = std::max(latest, *s);
        if (auto e = t->int_attr(attr::ENDED)) latest = std::max(latest, *e);
    }
    return latest;
}

std::vector<std::string> words(std::string_view s) {
    static constexpr std::array<std::string_view, 24> STOP = {
        "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "into", "is", "it", "of", "on", "or", "that",
        "the", "this", "to", "was", "were", "with",
    };
    std::vector<std::string> out;
    const auto flush = [&](std::size_t from, std::size_t to, std::size_t chars) {
        if (chars < 2) return;
        auto w = text::to_lowercase(s.substr(from, to - from));
        if (std::ranges::find(STOP, w) == STOP.end()) out.push_back(std::move(w));
    };
    std::size_t start = 0, chars = 0;
    for (std::size_t i = 0; i < s.size();) {
        const std::size_t at = i;
        if (!text::is_alphanumeric(text::decode(s, i))) {
            flush(start, at, chars);
            start = i;
            chars = 0;
        } else {
            ++chars;
        }
    }
    flush(start, s.size(), chars);
    return out;
}

Bm25 Bm25::fit(const std::vector<Candidate>& candidates, const std::vector<std::string>& query) {
    Bm25 bm;
    for (const auto& c : candidates) {
        std::unordered_map<std::string, std::size_t> counts;
        for (auto field : c.fields())
            for (auto& w : words(field)) ++counts[std::move(w)];
        bm.docs_.insert_or_assign(c.session, std::move(counts));
    }
    std::size_t total = 0;
    std::unordered_map<std::string, std::size_t> frequencies;
    for (const auto& [_, doc] : bm.docs_) {
        for (const auto& [word, n] : doc) {
            total += n;
            ++frequencies[word];
        }
    }
    const auto n = static_cast<double>(bm.docs_.size());
    for (const auto& [word, df] : frequencies) {
        const auto d = static_cast<double>(df);
        bm.idfs_.emplace(word, std::log(1.0 + (n - d + 0.5) / (d + 0.5)));
    }
    bm.avg_len_ = static_cast<double>(total) / static_cast<double>(std::max<std::size_t>(bm.docs_.size(), 1));
    double best = 0.0;
    for (const auto& c : candidates) best = std::max(best, bm.raw(query, c.session));
    bm.best_ = best;
    return bm;
}

double Bm25::raw(const std::vector<std::string>& query, const std::string& session) const {
    auto it = docs_.find(session);
    if (it == docs_.end()) return 0.0;
    const auto& doc = it->second;
    std::size_t len_n = 0;
    for (const auto& [_, n] : doc) len_n += n;
    const auto len = static_cast<double>(len_n);
    double sum = 0.0;
    for (const auto& t : query) {
        auto f = doc.find(t);
        const double tf = f == doc.end() ? 0.0 : static_cast<double>(f->second);
        auto i = idfs_.find(t);
        const double idf = i == idfs_.end() ? 0.0 : i->second;
        sum += idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len / std::max(avg_len_, 1.0)));
    }
    return sum;
}

double Bm25::score(const std::vector<std::string>& query, const Candidate& c) const {
    return best_ > 0.0 ? raw(query, c.session) / best_ : 0.0;
}

double Recency::score(const std::vector<std::string>&, const Candidate& c) const {
    // `saturating_sub` then `max(0)`.
    std::int64_t age = 0;
    if (__builtin_sub_overflow(now_, c.last_active, &age)) age = c.last_active > 0 ? std::numeric_limits<std::int64_t>::min() : std::numeric_limits<std::int64_t>::max();
    age = std::max<std::int64_t>(age, 0);
    return std::pow(0.5, static_cast<double>(age) / static_cast<double>(std::max<std::int64_t>(half_life_ms_, 1)));
}

double Weighted::score(const std::vector<std::string>& query, const Candidate& c) const {
    double sum = 0.0;
    for (const auto& [w, s] : parts_) sum += w * s->score(query, c);
    return sum;
}

std::size_t estimate_tokens(std::string_view s) noexcept { return (s.size() + 3) / 4; }

std::vector<Ranked> rank_and_pack(std::vector<Candidate> candidates, std::string_view query, const RankParams& params) {
    std::erase_if(candidates, [](const Candidate& c) { return c.excluded; });
    auto q = words(query);
    std::ranges::sort(q);
    q.erase(std::unique(q.begin(), q.end()), q.end());
    const double w = params.relevance_weight;
    std::vector<std::pair<double, std::unique_ptr<Scorer>>> parts;
    parts.emplace_back(w, std::make_unique<Bm25>(Bm25::fit(candidates, q)));
    parts.emplace_back(1.0 - w, std::make_unique<Recency>(params.now, params.half_life_ms));
    const Weighted scorer(std::move(parts));
    std::vector<Ranked> ranked;
    for (auto& c : candidates) {
        const double score = scorer.score(q, c);
        auto section = render_digest_section(c);
        ranked.push_back(Ranked{std::move(c), score, std::move(section)});
    }
    std::ranges::stable_sort(ranked, [](const Ranked& a, const Ranked& b) {
        if (auto o = std::strong_order(b.score, a.score); o != 0) return o < 0;
        if (a.candidate.last_active != b.candidate.last_active) return a.candidate.last_active > b.candidate.last_active;
        return a.candidate.session < b.candidate.session;
    });
    std::size_t left = params.budget_tokens;
    std::erase_if(ranked, [&](const Ranked& r) {
        const auto tokens = estimate_tokens(r.section);
        if (tokens > left) return true;
        left -= tokens;
        return false;
    });
    return ranked;
}

} // namespace loa::projects
