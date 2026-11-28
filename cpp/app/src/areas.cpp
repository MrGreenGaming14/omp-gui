// The areas and app commands compiled into `loa-qt`. Each area's issue adds its `boot<State>()`
// here (in any order: `Registry::make` boots them in `AREA_ORDER`), and the keys module its
// `AppCommand` table.

#include "areas/sessions/sessions.hpp"
#include "loa/app/app.hpp"
#include "loa/app/registry.hpp"

namespace loa::app {

std::vector<AreaBoot> builtin_areas() { return {boot<areas::sessions::State>()}; }

std::span<const AppCommand> app_commands() { return {}; }

} // namespace loa::app
