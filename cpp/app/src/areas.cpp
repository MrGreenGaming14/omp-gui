// The areas compiled into `loa-qt`. Each area's issue adds its `boot<State>()` here (in any
// order: `Registry::make` boots them in `AREA_ORDER`). The app's own commands are in
// `app/keys.cpp`.

// First: `loa/p2p/node.hpp` declares a member named `emit`, which Qt headers define away.
#include "areas/peers/peers.hpp"

#include "areas/agents/agents.hpp"
#include "areas/decisions/decisions.hpp"
#include "areas/harness/harness.hpp"
#include "areas/models/models.hpp"
#include "areas/projects/projects.hpp"
#include "areas/sessions/sessions.hpp"
#include "areas/workflows/workflows.hpp"
#include "loa/app/app.hpp"
#include "loa/app/registry.hpp"

namespace loa::app {

std::vector<AreaBoot> builtin_areas() {
    return {boot<areas::agents::State>(),   boot<areas::decisions::State>(), boot<areas::harness::State>(),
            boot<areas::models::State>(),   boot<areas::peers::State>(),     boot<areas::projects::State>(),
            boot<areas::sessions::State>(), boot<areas::workflows::State>()};
}

} // namespace loa::app
