#pragma once

// `omp/session/1` (Rust `protocols::session`): peers list, view, drive and
// prompt the sessions their grants reach; this node stays their single writer.

#include <cstddef>
#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/core/patch.hpp"
#include "loa/p2p/protocols/host.hpp"
#include "loa/runtime/json.hpp"
#include "loa/trust/scope.hpp"

namespace loa::bus {
class SessionWriter;
}

namespace loa::p2p {

// Events a subscriber may hold before it is cut off and must resubscribe.
inline constexpr std::size_t SUBSCRIBER_QUEUE = 4096;

// A session of a peer that this node's grants reach.
struct RemoteSession {
    NodeId peer;
    std::string id;
    std::string title;
    std::string harness;
    std::int64_t created_ms = 0;
    std::vector<trust::Action> actions;
    // Only some prompts are visible.
    bool partial = false;
    std::uint64_t last_seq = 0;
    friend bool operator==(const RemoteSession&, const RemoteSession&) = default;
};

// What came of a drive request.
namespace drive_outcome {
// The owner applied it as its patch `source_seq`; it reaches the view as an update.
struct Applied {
    std::uint64_t source_seq = 0;
    friend bool operator==(const Applied&, const Applied&) = default;
};
// The owner's session moved past the request's base; retry on the new state.
struct Conflict {
    std::uint64_t current = 0;
    friend bool operator==(const Conflict&, const Conflict&) = default;
};
} // namespace drive_outcome
using DriveOutcome = std::variant<drive_outcome::Applied, drive_outcome::Conflict>;

// The serde JSON form of a draft that `Drive` carries (externally tagged, as Rust `Draft`).
[[nodiscard]] runtime::json::Value draft_to_json(const core::Draft& draft);
[[nodiscard]] Result<core::Draft> draft_from_json(const runtime::json::Value& value);

// Action names to actions, skipping unknown ones (Rust `wire::actions`).
[[nodiscard]] std::vector<trust::Action> wire_actions(const std::vector<std::string>& names);

// A live, read-only mirror of a peer's session. Changes go to the owner as
// drive requests; this node never writes the session itself. Destroying it
// closes the connection.
class RemoteView {
  public:
    struct Shared;

    RemoteView(const RemoteView&) = delete;
    RemoteView& operator=(const RemoteView&) = delete;
    ~RemoteView();

    [[nodiscard]] const NodeId& peer() const noexcept { return peer_; }
    [[nodiscard]] const std::string& session() const noexcept { return session_; }
    // The mirror: subscribe to it like any session. It takes no local commits.
    [[nodiscard]] const std::shared_ptr<bus::SessionWriter>& writer() const noexcept { return writer_; }
    // What the grants allow on the whole session.
    [[nodiscard]] const std::vector<trust::Action>& actions() const noexcept { return actions_; }
    [[nodiscard]] bool can(trust::Action action) const;
    // The owner's sequence this view has caught up with; the base of drive requests.
    [[nodiscard]] std::uint64_t source_seq() const;
    // Why the view stopped following the owner, once it has.
    [[nodiscard]] std::optional<std::string> ended() const;
    // The mirror's patches from 1, for forking.
    [[nodiscard]] std::vector<core::Patch> patches() const;

    // Asks the owner to apply `draft`, written against this view, on the owner state it has seen.
    Result<DriveOutcome> drive(core::Draft draft);
    // `drive` on an explicit owner sequence.
    Result<DriveOutcome> drive_at(std::uint64_t base_seq, core::Draft draft);
    // Asks the owner's harness to run `text` in this session.
    Result<> prompt(std::string_view text);
    // Answers a waiting decision (its id in this view) on the owner, as this node (needs drive).
    Result<> answer(core::NodeId decision, std::vector<std::string> values, std::optional<std::string> reason);

  private:
    friend Result<std::unique_ptr<RemoteView>> open_remote_view(NodeHost&, const NodeId&, std::string_view);
    RemoteView(NodeId peer, std::string session, std::vector<trust::Action> actions,
               std::shared_ptr<bus::SessionWriter> writer, std::shared_ptr<Shared> shared, Connection conn);
    Result<wire::Body> call(wire::Body body);

    NodeId peer_;
    std::string session_;
    std::vector<trust::Action> actions_;
    std::shared_ptr<bus::SessionWriter> writer_;
    std::shared_ptr<Shared> shared_;
    Connection conn_;
};

// Subscribes to a peer's session: its seed, then a pump following the owner.
Result<std::unique_ptr<RemoteView>> open_remote_view(NodeHost& host, const NodeId& peer, std::string_view session);

} // namespace loa::p2p

namespace loa::p2p::session {

// The owner's side of one session stream.
Result<> serve(NodeHost& host, const Connection& conn, Stream stream);
// The entries `peer`'s live grants reach, with partial ones for turn grants.
[[nodiscard]] std::vector<wire::SessionEntry> visible(NodeHost& host, const NodeId& peer);

// The requester side.
Result<std::vector<RemoteSession>> list(NodeHost& host, const NodeId& peer);
Result<> prompt(NodeHost& host, const NodeId& peer, std::string_view session, std::string_view text);
inline Result<std::unique_ptr<RemoteView>> open(NodeHost& host, const NodeId& peer, std::string_view session) {
    return open_remote_view(host, peer, session);
}

} // namespace loa::p2p::session
