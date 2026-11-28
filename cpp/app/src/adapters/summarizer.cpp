#include "loa/app/adapters/summarizer.hpp"

namespace loa::app::adapters {

std::pair<std::shared_ptr<const projects::Summarizer>, std::optional<std::string>> OmpSummarizers::on(
    std::optional<std::string_view> model) const {
    std::string picked(oneshot_.pick(model));
    std::string name = picked;
    while (name.starts_with('@')) name.erase(0, 1);
    auto summarizer = loa::harness::omp::OneShot(oneshot_).with_model(std::move(picked));
    return {std::make_shared<loa::harness::omp::OneShot>(std::move(summarizer)), std::move(name)};
}

} // namespace loa::app::adapters
