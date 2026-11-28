#pragma once

// Ranks a project's digests by a `Weighted` mix of `Bm25` relevance and `Recency`, and packs them into a
// token budget. Ties go to the most recently active, then the lower session id. A token is estimated as
// four bytes of the rendered section.

#include <cstddef>
#include <cstdint>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_map>
#include <utility>
#include <vector>

#include "loa/core/dom.hpp"
#include "loa/projects/digest.hpp"

namespace loa::projects {

// One session's digest offered for project context.
struct Candidate {
    std::string session;
    std::optional<std::string> title;
    Digest digest;
    // Unix milliseconds of the session's last activity.
    std::int64_t last_active = 0;
    bool excluded = false;

    // None without a digest.
    [[nodiscard]] static std::optional<Candidate> from_dom(const core::Dom& dom);
    // The digest's searchable fields.
    [[nodiscard]] std::vector<std::string_view> fields() const;
    friend bool operator==(const Candidate&, const Candidate&) = default;
};

// Unix milliseconds of the latest turn start or end, else the session's creation.
[[nodiscard]] std::int64_t last_active(const core::Dom& dom);

// Lowercase alphanumeric runs of two or more chars, minus English stop words.
[[nodiscard]] std::vector<std::string> words(std::string_view text);

// A ranking strategy: a score in 0..=1 for a candidate against the query words.
class Scorer {
  public:
    virtual ~Scorer() = default;
    [[nodiscard]] virtual double score(const std::vector<std::string>& query, const Candidate& candidate) const = 0;
};

// BM25 fitted on the candidates, divided by the best candidate's score.
class Bm25 final : public Scorer {
  public:
    static constexpr double K1 = 1.2;
    static constexpr double B = 0.75;

    [[nodiscard]] static Bm25 fit(const std::vector<Candidate>& candidates, const std::vector<std::string>& query);
    [[nodiscard]] double score(const std::vector<std::string>& query, const Candidate& c) const override;

  private:
    [[nodiscard]] double raw(const std::vector<std::string>& query, const std::string& session) const;

    std::unordered_map<std::string, std::unordered_map<std::string, std::size_t>> docs_;
    std::unordered_map<std::string, double> idfs_;
    double avg_len_ = 0;
    double best_ = 1;
};

// Halves every `half_life_ms` since the session was last active.
class Recency final : public Scorer {
  public:
    Recency(std::int64_t now, std::int64_t half_life_ms) : now_(now), half_life_ms_(half_life_ms) {}
    [[nodiscard]] double score(const std::vector<std::string>& query, const Candidate& c) const override;

  private:
    std::int64_t now_;
    std::int64_t half_life_ms_;
};

// The weighted sum of other scorers.
class Weighted final : public Scorer {
  public:
    explicit Weighted(std::vector<std::pair<double, std::unique_ptr<Scorer>>> parts) : parts_(std::move(parts)) {}
    [[nodiscard]] double score(const std::vector<std::string>& query, const Candidate& c) const override;

  private:
    std::vector<std::pair<double, std::unique_ptr<Scorer>>> parts_;
};

// `now` in Unix ms; `relevance_weight` is relevance's share (0 to 1), recency gets the rest.
struct RankParams {
    std::int64_t now = 0;
    std::int64_t half_life_ms = 7LL * 24 * 3600 * 1000;
    double relevance_weight = 0.7;
    std::size_t budget_tokens = 0;

    // A week's half life and 70 % relevance.
    [[nodiscard]] static RankParams make(std::int64_t now, std::size_t budget_tokens) {
        return RankParams{now, 7LL * 24 * 3600 * 1000, 0.7, budget_tokens};
    }
    friend bool operator==(const RankParams&, const RankParams&) = default;
};

// A packed digest, its score and its rendered section.
struct Ranked {
    Candidate candidate;
    double score = 0;
    std::string section;
    friend bool operator==(const Ranked&, const Ranked&) = default;
};

// Four bytes per token.
[[nodiscard]] std::size_t estimate_tokens(std::string_view text) noexcept;

// Ranks `candidates` against `query` and keeps, best first, those that fit the budget. Excluded sessions
// are dropped before fitting, so they don't shift other scores.
[[nodiscard]] std::vector<Ranked> rank_and_pack(std::vector<Candidate> candidates, std::string_view query,
                                                const RankParams& params);

} // namespace loa::projects
