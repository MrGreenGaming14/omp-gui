#pragma once

// Rebuilds missing or stale digests, debounced per session so a burst of turns costs one rebuild. A failed
// rebuild keeps the old digest, flagged stale.

#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <variant>

#include "loa/core/dom.hpp"
#include "loa/core/patch.hpp"
#include "loa/projects/digester.hpp"
#include "loa/projects/settings.hpp"
#include "loa/projects/source.hpp"

namespace asio {
class io_context;
}

namespace loa::projects {

enum class Decision : std::uint8_t {
    Rebuild,
    UpToDate,
    // No turns yet.
    Empty,
    // Excluded sessions get no digest.
    Excluded,
};

[[nodiscard]] Decision decide(const core::Dom& dom);

// Commits drafts to a session.
class SessionWriter {
  public:
    virtual ~SessionWriter() = default;
    [[nodiscard]] virtual Result<core::Patch> commit(std::string_view session, core::Draft draft) const = 0;
};

namespace rebuild_outcome {
struct Rebuilt {
    std::string session;
    std::uint64_t source_seq = 0;
    friend bool operator==(const Rebuilt&, const Rebuilt&) = default;
};
struct Skipped {
    std::string session;
    Decision decision = Decision::UpToDate;
    friend bool operator==(const Skipped&, const Skipped&) = default;
};
// The old digest, if any, is now flagged stale.
struct Failed {
    std::string session;
    std::string error;
    friend bool operator==(const Failed&, const Failed&) = default;
};
} // namespace rebuild_outcome

// Reported once per handled session.
using RebuildOutcome = std::variant<rebuild_outcome::Rebuilt, rebuild_outcome::Skipped, rebuild_outcome::Failed>;

// The receiving end of a driver's outcomes; thread-safe.
class Outcomes {
  public:
    void push(RebuildOutcome outcome);
    // Marks the driver finished: `recv` returns none once drained.
    void finish();
    // Blocks until an outcome arrives, the driver finishes, or `timeout` passes.
    [[nodiscard]] std::optional<RebuildOutcome> recv(std::chrono::milliseconds timeout = std::chrono::hours(24));
    [[nodiscard]] std::optional<RebuildOutcome> try_recv();

  private:
    std::mutex mutex_;
    std::condition_variable ready_;
    std::deque<RebuildOutcome> queue_;
    bool finished_ = false;
};

// Queues sessions for rebuild. Dropping the last copy closes the queue: pending sessions then run right away
// and the driver stops.
class RebuildSender {
  public:
    // Queues `session`'s rebuild; false once the driver stopped.
    bool send(std::string session) const;

  private:
    friend class RebuildDriver;
    struct Handle;
    explicit RebuildSender(std::shared_ptr<Handle> handle) : handle_(std::move(handle)) {}
    std::shared_ptr<Handle> handle_;
};

// Runs the rebuild policy for many sessions, one rebuild at a time.
struct RebuildDriver {
    std::shared_ptr<const SessionSource> source;
    std::shared_ptr<const SessionWriter> writer;
    Digester digester;
    std::chrono::steady_clock::duration debounce;
    // Supplies each project's `digest_model`.
    std::shared_ptr<const SettingsStore> settings;

    // Runs on `context` until every sender is dropped and pending sessions are handled.
    [[nodiscard]] std::pair<RebuildSender, std::shared_ptr<Outcomes>> spawn(asio::io_context& context) &&;
};

} // namespace loa::projects
