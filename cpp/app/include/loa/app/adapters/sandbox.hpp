#pragma once

// The sandbox ports over the app's node and session library (port of
// `loa-app/src/adapters/sandbox.rs`): this device runs `sandbox_task` on peers
// that granted it `resource::Sandbox`, serves its own stub to peers it granted
// the same, and journals worker sessions.
//
// `loa/p2p/node.hpp` declares a member named `emit`, a Qt macro, so `Node` is
// only forward-declared here.

#include <cstdint>
#include <filesystem>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/p2p/protocols/extra.hpp"
#include "loa/sandbox/provider.hpp"
#include "loa/sandbox/worker.hpp"
#include "loa/trust/scope.hpp"

namespace loa::session {
class Library;
} // namespace loa::session

namespace loa::p2p {
class Node;
} // namespace loa::p2p

namespace loa::app::adapters {

// Keeps worker sessions in the app's session library, so they are journaled, listed and opened like any other.
class LibraryJournal final : public sandbox::WorkerJournal {
  public:
    explicit LibraryJournal(std::shared_ptr<session::Library> library) : library_(std::move(library)) {}
    [[nodiscard]] sandbox::Result<std::shared_ptr<core::PatchSink>> open(std::string_view session_id) const override;

  private:
    std::shared_ptr<session::Library> library_;
};

// Version of `sandbox::PEER_PROTOCOL` on the node.
inline constexpr std::uint32_t VERSION = 1;
// What a peer's grant must cover to run a sandbox here.
inline const std::pair<trust::Resource, trust::Action> REQUIRED{trust::resource::Sandbox{}, trust::Action::exec};

// Streams to the peers that granted this device sandbox access, through whichever node the app runs now.
class NodeLink final : public sandbox::PeerLink {
  public:
    explicit NodeLink(p2p::NodeSlot<p2p::Node> slot) : slot_(std::move(slot)) {}

    [[nodiscard]] bool running() const override;
    [[nodiscard]] std::vector<std::string> peers() const override;
    [[nodiscard]] std::optional<std::string> name(std::string_view peer) const override;
    [[nodiscard]] contracts::Awaitable<sandbox::Result<sandbox::Channel>> open(std::string peer) const override;

  private:
    p2p::NodeSlot<p2p::Node> slot_;
};

// A peer's sandbox run on this device that failed, for a notice.
struct Served {
    std::string peer;
    std::string error;
    friend bool operator==(const Served&, const Served&) = default;
};

// Runs the stub for a peer whose grant the node verified.
class ServeStub final : public p2p::PeerProtocol {
  public:
    // `failures` gets every failed run (the Rust `broadcast::Sender<Served>`); it may run on any thread.
    ServeStub(std::optional<std::filesystem::path> stub, std::function<void(Served)> failures)
        : stub_(std::move(stub)), failures_(std::move(failures)) {}

    void serve(const p2p::NodeId& peer, p2p::Stream stream) override;

  private:
    std::optional<std::filesystem::path> stub_;
    std::function<void(Served)> failures_;
};

// Adds the sandbox protocol to every node the app runs.
void register_(p2p::NodeSlot<p2p::Node>& slot, std::shared_ptr<ServeStub> handler);

} // namespace loa::app::adapters
