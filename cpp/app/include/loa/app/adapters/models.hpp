#pragma once

// The real model screens' backend, composed from `loa::harness` (port of
// `loa-app/src/adapters/models.rs`).

#include <memory>
#include <vector>

#include "loa/app_runtime/harnesses.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/harness/omp/account.hpp"
#include "loa/models/service.hpp"

namespace loa::app::adapters {

// The model screens' backend parts: omp started from `launch` (one RPC process, owned by omp's
// account), omp's active agent directory, validation by omp, the platform browser and the
// accounts of the other harnesses `factories` know, each on the executable `harnesses` runs it
// with.
[[nodiscard]] models::Parts models_parts(const loa::harness::omp::OmpLaunch& launch, const app_runtime::Harnesses& harnesses,
                                         const std::vector<std::shared_ptr<contracts::HarnessFactory>>& factories);

} // namespace loa::app::adapters
