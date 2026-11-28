#pragma once

// What the node protocols need from the node that serves them (the Rust
// protocols reach into `Node` directly). The node facade implements `NodeHost`
// and wires each protocol's `serve` into its `Router` through `served`.
//
// Like the transports, every protocol call blocks; servers run one thread per
// accepted stream.

#include <cstdint>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/core/value.hpp"
#include "loa/p2p/transport.hpp"
#include "loa/p2p/wire.hpp"

namespace loa::p2p {

class GrantService;
class LocalSessions;

// A peer asked this node's harness to run a prompt in one of its sessions (Rust `NodeEvent::PromptRequested`).
struct PromptRequested {
    NodeId peer;
    std::string session;
    std::string text;
    friend bool operator==(const PromptRequested&, const PromptRequested&) = default;
};

// A peer answered a waiting decision, by its id here (Rust `NodeEvent::AnswerRequested`).
struct AnswerRequested {
    NodeId peer;
    std::string session;
    core::NodeId decision;
    std::vector<std::string> values;
    std::optional<std::string> reason;
    friend bool operator==(const AnswerRequested&, const AnswerRequested&) = default;
};

// Port: the node as its protocols see it.
class NodeHost {
  public:
    virtual ~NodeHost() = default;

    [[nodiscard]] virtual NodeId key() const = 0;
    [[nodiscard]] virtual GrantService& grants() const = 0;
    [[nodiscard]] virtual LocalSessions& local_sessions() const = 0;

    // The hello this node sends on `omp/control/1`.
    [[nodiscard]] virtual wire::Hello hello() const = 0;
    // A peer proved its key and sent a valid hello over `kind`.
    virtual void observe(const NodeId& peer, const wire::Hello& hello, TransportKind kind) = 0;
    // Peers or grants changed (Rust `NodeEvent::Changed`).
    virtual void changed() = 0;
    virtual void prompt_requested(PromptRequested request) = 0;
    virtual void answer_requested(AnswerRequested request) = 0;

    // A connection to `peer` on `alpn`, through the node's transports and its address hints.
    virtual Result<Connection> dial(const NodeId& peer, std::string_view alpn) = 0;
    // Whatever node answers at `addr`, for probing.
    virtual Result<Connection> dial_any(const SocketAddr& addr, std::string_view alpn) = 0;
};

// Serves one accepted stream of a protocol.
using ServeStream = std::function<Result<>(NodeHost& host, const Connection& conn, Stream stream)>;

// A handler that runs `serve` on its own thread for every stream accepted on
// a connection, while `host` lives (Rust `protocols::Served`).
[[nodiscard]] std::shared_ptr<ProtocolHandler> served(std::weak_ptr<NodeHost> host, ServeStream serve);

} // namespace loa::p2p
