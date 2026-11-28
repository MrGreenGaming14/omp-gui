// The areas and app commands compiled into `loa-qt`. Each area's issue adds its `boot<State>()`
// here (in any order: `Registry::make` boots them in `AREA_ORDER`), and the keys module its
// `AppCommand` table.

#include "loa/app/app.hpp"
#include "loa/app/registry.hpp"
#include "areas/agents/agents.hpp"
#include "areas/projects/projects.hpp"

namespace loa::app {

std::vector<AreaBoot> builtin_areas() { return {boot<areas::agents::State>(), boot<areas::projects::State>()}; }

std::span<const AppCommand> app_commands() { return {}; }

} // namespace loa::app
