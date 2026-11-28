#pragma once

// Protocol machinery every harness shares: stdio RPC with pluggable framing, process
// handling, turn bookkeeping, the generic driven session, native codecs and residual
// round-trip helpers. `DrivenSession` bodies: `loa/harness/kit/mapper_impl.hpp`.

#include "loa/harness/kit/jsonrpc.hpp"
#include "loa/harness/kit/ledger.hpp"
#include "loa/harness/kit/mapper.hpp"
#include "loa/harness/kit/native.hpp"
#include "loa/harness/kit/proc.hpp"
#include "loa/harness/kit/residual.hpp"
#include "loa/harness/kit/rpc.hpp"
