#pragma once

// omp agent definitions as `loa::view::settings::agents` views, and the form's fields back onto a
// definition.

#include "loa/settings/models/agents.hpp"
#include "loa/settings/models/error.hpp"
#include "loa/view/settings_agents.hpp"

namespace loa::app::view::settings {

// The parts of `def` the form edits.
[[nodiscard]] ::loa::view::settings::agents::AgentFields fields_to_view(const ::loa::settings::models::agents::AgentDefinition& def);
// `base` with the form's `fields`. Only the keys that differ are rewritten, so an unedited
// definition keeps its text.
[[nodiscard]] ::loa::settings::models::Result<::loa::settings::models::agents::AgentDefinition>
fields_from_view(const ::loa::settings::models::agents::AgentDefinition& base, ::loa::view::settings::agents::AgentFields fields);
[[nodiscard]] ::loa::view::settings::agents::AgentEntry entry_to_view(const ::loa::settings::models::agents::AgentFile& file);
// The view's `field` names the form field (`"form"` for one the form has no field for).
[[nodiscard]] ::loa::view::settings::agents::FieldError field_error_to_view(::loa::settings::models::agents::FieldError error);

} // namespace loa::app::view::settings
