#pragma once

// `<surface>` elements: a surface opened during a session, recorded under the
// turn or tool call that opened it, or the session itself. Port of
// `loa-surfaces/src/hub/dom.rs`.

#include "loa/contracts/surface.hpp"
#include "loa/core/patch.hpp"

namespace loa::surfaces::hub {

class Origin;

namespace dom {

// Inserts `<surface uri mode name width height server status="open">` under
// `parent` (a session, turn or call node).
[[nodiscard]] core::Draft opened(core::NodeId parent, const loa::contracts::surface::SurfaceInfo& info, const Origin& origin);

// Marks the `<surface>` element `node` closed.
[[nodiscard]] core::Draft closed(core::NodeId node);

} // namespace dom
} // namespace loa::surfaces::hub
