#pragma once

// Server side of `omp/surface/1` (port of `loa-surfaces/src/proto/server.rs`).

#include <cstddef>
#include <memory>

#include "loa/contracts/async.hpp"
#include "loa/contracts/surface.hpp"
#include "loa/surfaces/proto/error.hpp"
#include "loa/surfaces/proto/transport.hpp"

namespace loa::surfaces::proto {

// Bytes per `DocumentChunk`.
inline constexpr std::size_t DOCUMENT_CHUNK = 64 * 1024;

// Serves `surface` to the viewer on `transport` until either side closes.
[[nodiscard]] Awaitable<Result<void>> serve(std::shared_ptr<contracts::surface::Surface> surface, Transport transport);

} // namespace loa::surfaces::proto
