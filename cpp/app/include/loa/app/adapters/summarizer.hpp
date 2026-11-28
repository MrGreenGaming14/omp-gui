#pragma once

// Digest summaries by omp's one-shot model calls (port of `loa-app/src/adapters/summarizer.rs`).

#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>

#include "loa/harness/omp/oneshot.hpp"
#include "loa/projects/digester.hpp"

namespace loa::app::adapters {

// `omp -p` on the given model, `@smol` by default.
class OmpSummarizers final : public projects::Summarizers {
  public:
    explicit OmpSummarizers(loa::harness::omp::OneShot oneshot) : oneshot_(std::move(oneshot)) {}
    [[nodiscard]] std::pair<std::shared_ptr<const projects::Summarizer>, std::optional<std::string>> on(
        std::optional<std::string_view> model) const override;

  private:
    loa::harness::omp::OneShot oneshot_;
};

} // namespace loa::app::adapters
