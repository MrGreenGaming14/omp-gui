// The areas compiled into `loa-qt`. Each area's issue adds its `boot<State>()` here (in any
// order: `Registry::make` boots them in `AREA_ORDER`). The app's own commands are in
// `app/keys.cpp`.

#include "areas/workflows/workflows.hpp"
#include "loa/app/app.hpp"
#include "loa/app/registry.hpp"

namespace loa::app {

std::vector<AreaBoot> builtin_areas() { return {boot<areas::workflows::State>()}; }

} // namespace loa::app
