#include "loa/app/view/session.hpp"

#include <optional>
#include <utility>

namespace loa::app::view {

namespace domain = ::loa::settings::app;
namespace shown = ::loa::view::session::decisions;

shown::UnattendedView unattended_to_view(const domain::UnattendedSettings& settings) {
    const auto to = [](const std::optional<domain::DecisionDefault>& d) -> std::optional<shown::DecisionDefaultView> {
        if (!d) return std::nullopt;
        return shown::DecisionDefaultView{d->timeout_secs, d->answer, d->reason};
    };
    return shown::UnattendedView{to(settings.approval), to(settings.question), to(settings.plan), to(settings.form)};
}

domain::UnattendedSettings unattended_from_view(shown::UnattendedView view) {
    const auto from = [](std::optional<shown::DecisionDefaultView>& d) -> std::optional<domain::DecisionDefault> {
        if (!d) return std::nullopt;
        return domain::DecisionDefault{d->timeout_secs, std::move(d->answer), std::move(d->reason)};
    };
    return domain::UnattendedSettings{from(view.approval), from(view.question), from(view.plan), from(view.form)};
}

} // namespace loa::app::view
