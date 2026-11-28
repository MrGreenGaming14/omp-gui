#pragma once

// Client for `omp --mode rpc`.
//
// `RpcClient::spawn` starts omp, waits for its `ready` frame and negotiates the protocol
// version (`RpcClient::protocol_version`). Commands are typed (`Command`) and correlated with
// their responses by id. Everything else omp writes arrives in order on `Events`: session
// events (`Event`, with `event::Unknown` keeping frames this client doesn't model), UI
// requests (`UiRequest`, answered with `RpcClient::respond_ui`) and the final
// `incoming::Exited`. Host tool callbacks are served by a `HostHandler`.
//
// This module knows nothing about the session DOM, so the models, updater and projects
// libraries can drive omp through it too.

#include "loa/harness/omp/rpc/client.hpp"
#include "loa/harness/omp/rpc/command.hpp"
#include "loa/harness/omp/rpc/event.hpp"
#include "loa/harness/omp/rpc/frames.hpp"
#include "loa/harness/omp/rpc/inbound.hpp"
