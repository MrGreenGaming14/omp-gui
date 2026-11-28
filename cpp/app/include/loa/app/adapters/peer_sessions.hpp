#pragma once

// This node's sessions as peers see them, read from the app's `session::Library`
// (port of `loa-app/src/adapters/peer_sessions.rs`).

#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>
#include <vector>

#include "loa/p2p/sessions.hpp"

namespace loa::session {
class Library;
} // namespace loa::session

namespace loa::app::adapters {

// Every session in the app's library, live or only journaled. Peers' requests
// open a session's writer through the library, so its changes are journaled
// like the user's own.
class LibrarySessions final : public p2p::LocalSessions {
  public:
    explicit LibrarySessions(std::shared_ptr<session::Library> library);

    // Newest first, then by id.
    [[nodiscard]] std::vector<p2p::LocalSession> sessions() const override;
    [[nodiscard]] std::shared_ptr<bus::SessionWriter> writer(std::string_view id) const override;

  private:
    // `id` at journal sequence `seq`, read again only when the journal moved.
    [[nodiscard]] std::optional<p2p::LocalSession> closed(const std::string& id, std::uint64_t seq) const;

    std::shared_ptr<session::Library> library_;
    // Sessions read from their journals, by id, with the sequence they were read at.
    mutable std::mutex mutex_;
    mutable std::map<std::string, p2p::LocalSession, std::less<>> cache_;
};

} // namespace loa::app::adapters
