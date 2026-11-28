#pragma once

// The port to this node's own sessions, which peers may view and drive (Rust `loa_p2p::sessions`).

#include <cstddef>
#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/core/dom.hpp"
#include "loa/trust/scope.hpp"

namespace loa::bus {
class Bus;
class SessionWriter;
} // namespace loa::bus

namespace loa::p2p {

// Characters of a prompt shown when picking it as a grant resource.
inline constexpr std::size_t PREVIEW_CHARS = 60;

// One prompt (`<turn>`) of a local session.
struct LocalTurn {
    core::NodeId id;
    // The start of the user's message, for pickers.
    std::string preview;
    friend bool operator==(const LocalTurn&, const LocalTurn&) = default;
};

// A session this node owns, as grants and peers see it.
struct LocalSession {
    trust::SessionFacts facts;
    std::string title;
    std::string harness;
    std::uint64_t last_seq = 0;
    std::vector<LocalTurn> turns;

    // Reads a session DOM; none when it has no root with an id and creation time.
    [[nodiscard]] static std::optional<LocalSession> from_dom(const core::Dom& dom);
    // The title, or the first prompt when the session has none yet.
    [[nodiscard]] std::string label() const;

    friend bool operator==(const LocalSession&, const LocalSession&) = default;
};

// What the node needs from the app's sessions. `loa-qt` backs it with its
// session registry, which opens sessions lazily from the journal store.
class LocalSessions {
  public:
    virtual ~LocalSessions() = default;
    // Every session this node owns that grants can name.
    [[nodiscard]] virtual std::vector<LocalSession> sessions() const = 0;
    // The live writer of a session this node owns, opening it if needed. Null
    // for unknown sessions and for mirrors of other nodes' sessions.
    [[nodiscard]] virtual std::shared_ptr<bus::SessionWriter> writer(std::string_view id) const = 0;
};

// The sessions open on a `bus::Bus` that this node writes.
class BusSessions final : public LocalSessions {
  public:
    explicit BusSessions(std::shared_ptr<bus::Bus> bus) : bus_(std::move(bus)) {}

    // Newest first, then by id.
    [[nodiscard]] std::vector<LocalSession> sessions() const override;
    [[nodiscard]] std::shared_ptr<bus::SessionWriter> writer(std::string_view id) const override;

  private:
    std::shared_ptr<bus::Bus> bus_;
};

} // namespace loa::p2p
