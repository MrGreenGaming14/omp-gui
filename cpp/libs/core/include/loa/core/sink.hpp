#pragma once

// The write side of a session: everything that changes a session hands a
// `Draft` to its one `PatchSink`, which orders, applies and returns it.

#include <cstdint>
#include <expected>
#include <mutex>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/core/dom.hpp"
#include "loa/core/patch.hpp"
#include "loa/runtime/error.hpp"
#include "loa/runtime/time.hpp"

namespace loa::core {

namespace commit_error {
struct Apply {
    ApplyError error;
    friend bool operator==(const Apply&, const Apply&) = default;
};
// This node doesn't own the session, so it can't order patches for it.
struct NotWriter {
    friend bool operator==(const NotWriter&, const NotWriter&) = default;
};
struct Closed {
    friend bool operator==(const Closed&, const Closed&) = default;
};
struct Journal {
    std::string message;
    friend bool operator==(const Journal&, const Journal&) = default;
};
} // namespace commit_error

using CommitError = std::variant<commit_error::Apply, commit_error::NotWriter, commit_error::Closed,
                                 commit_error::Journal>;

// The Rust `Display` text.
[[nodiscard]] std::string to_string(const CommitError& e);
[[nodiscard]] loa::Error to_error(const CommitError& e);

// Unix milliseconds.
using loa::runtime::time::now_ms;

// Commits drafts to one session. Implementations are cheap to call from any
// thread and never block on subscribers.
class PatchSink {
  public:
    virtual ~PatchSink() = default;

    // Commits `draft` with timestamp `ts` (unix milliseconds). Imports use it
    // to keep the original times.
    [[nodiscard]] virtual std::expected<Patch, CommitError> commit_at(Draft draft, std::int64_t ts) = 0;

    // Commits `draft` stamped with the current time.
    [[nodiscard]] std::expected<Patch, CommitError> commit(Draft draft) { return commit_at(std::move(draft), now_ms()); }
};

// A sink that keeps the DOM and every patch in memory. For tests and for
// building a DOM without a journal (imports, previews).
class MemorySink final : public PatchSink {
  public:
    MemorySink() = default;
    // Continues from an existing DOM.
    explicit MemorySink(Dom dom) : dom_(std::move(dom)) {}

    // A copy of the current DOM.
    [[nodiscard]] Dom dom() const;
    // Every patch committed through this sink, in order.
    [[nodiscard]] std::vector<Patch> patches() const;
    // Reads the current DOM without copying it.
    template <class F>
    decltype(auto) with_dom(F&& read) const {
        std::scoped_lock lock(mutex_);
        return std::forward<F>(read)(static_cast<const Dom&>(dom_));
    }

    [[nodiscard]] std::expected<Patch, CommitError> commit_at(Draft draft, std::int64_t ts) override;

  private:
    mutable std::mutex mutex_;
    Dom dom_;
    std::vector<Patch> patches_;
};

} // namespace loa::core
