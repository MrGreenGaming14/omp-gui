#pragma once

// Protocols the app adds to the node (e.g. `loa-sandbox/1`, Rust
// `protocols::extra`): each stream passes the grant handshake, and only a
// requester whose grants cover the required resource and action reaches the
// app's handler with the raw stream.

#include <cstdint>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/p2p/protocols/host.hpp"
#include "loa/trust/scope.hpp"

namespace loa::p2p {

// The app's side of an added protocol: one call per stream a covering grant let through.
class PeerProtocol {
  public:
    virtual ~PeerProtocol() = default;
    virtual void serve(const NodeId& peer, Stream stream) = 0;
};

// The exact protocol label, `<name>/<version>`.
[[nodiscard]] std::string protocol_label(std::string_view name, std::uint32_t version);

// A protocol added with `Node::add_protocol`.
struct AddedProtocol {
    std::string name;
    std::uint32_t version = 0;
    // The grant a requester must prove: an action on a resource.
    std::pair<trust::Resource, trust::Action> required;
    std::shared_ptr<PeerProtocol> handler;

    [[nodiscard]] std::string label() const { return protocol_label(name, version); }
};

// The node the app runs now plus the protocols areas added; every node set
// here serves them from its next start. `N` has `void add(AddedProtocol)`.
template <class N>
class NodeSlot {
  public:
    [[nodiscard]] std::shared_ptr<N> get() const {
        std::scoped_lock lock(state_->mutex);
        return state_->node;
    }

    void set(std::shared_ptr<N> node) {
        std::scoped_lock lock(state_->mutex);
        if (node)
            for (const auto& added : state_->added) node->add(added);
        state_->node = std::move(node);
    }

    // Adds a protocol to the current node and every later one.
    void add_protocol(AddedProtocol protocol) {
        std::scoped_lock lock(state_->mutex);
        if (state_->node) state_->node->add(protocol);
        std::erase_if(state_->added, [&](const AddedProtocol& added) { return added.label() == protocol.label(); });
        state_->added.push_back(std::move(protocol));
    }

  private:
    struct State {
        std::mutex mutex;
        std::shared_ptr<N> node;
        std::vector<AddedProtocol> added;
    };
    // Copies share one slot.
    std::shared_ptr<State> state_ = std::make_shared<State>();
};

} // namespace loa::p2p

namespace loa::p2p::extra {

// The owner's side of one stream of an added protocol.
Result<> serve(NodeHost& host, const Connection& conn, Stream stream, const AddedProtocol& protocol);
// The requester side: a stream past the grant handshake; its halves keep the connection open.
Result<Stream> open(NodeHost& host, const NodeId& peer, std::string_view name, std::uint32_t version);

} // namespace loa::p2p::extra
