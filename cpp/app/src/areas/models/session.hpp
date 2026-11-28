#pragma once

// The session's model as its `<session>` element records it (`model`, `model_role`), and the
// draft recording which role it follows (port of `loa-app/src/areas/models/session.rs`).

#include <optional>
#include <string_view>
#include <utility>

#include "loa/core/dom.hpp"
#include "loa/core/patch.hpp"
#include "loa/view/settings_models.hpp"

namespace loa::app::areas::models {

// The root `<session>` of `dom` and its model; `nullopt` without a session.
[[nodiscard]] std::optional<std::pair<core::NodeId, loa::view::settings::models::SessionModel>> session_model(const core::Dom& dom);

// The draft recording that the session in `dom` follows `role` (or no role); `nullopt` when that
// is recorded already or there is no session. The harness records the model itself.
[[nodiscard]] std::optional<core::Draft> role_draft(const core::Dom& dom, std::optional<std::string_view> role);

} // namespace loa::app::areas::models
