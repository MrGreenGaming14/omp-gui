#pragma once

// `DomWriter`: the drafts every harness, sandbox and hub writes, in one place.

#include <cstdint>
#include <expected>
#include <optional>
#include <string>
#include <string_view>

#include "loa/core/sink.hpp"

namespace loa::core {

// Commits convention-shaped drafts to one session's sink.
class DomWriter {
  public:
    explicit DomWriter(PatchSink& sink) : sink_(&sink) {}

    // A writer that stamps every patch, and `ended`, with `ts` (unix ms).
    [[nodiscard]] DomWriter at(std::int64_t ts) const {
        DomWriter w = *this;
        w.ts_ = ts;
        return w;
    }

    [[nodiscard]] std::int64_t now() const { return ts_ ? *ts_ : now_ms(); }

    // Commits any draft, for writes the helpers below don't cover.
    std::expected<NodeId, CommitError> commit(Draft draft) const;
    // Appends a `kind` node under `parent`. Returns the new node.
    std::expected<NodeId, CommitError> insert(NodeId parent, ElementKind kind, Attrs attrs = {},
                                              std::string text = {}) const;
    std::expected<void, CommitError> set(NodeId node, std::string_view name, Value value) const;
    std::expected<void, CommitError> clear(NodeId node, std::string_view name) const;
    std::expected<void, CommitError> remove(NodeId node) const;
    std::expected<void, CommitError> append(NodeId node, std::string text) const;
    std::expected<void, CommitError> set_text(NodeId node, std::string text) const;
    // Ends a turn, call, job or decision: `status`, then `ended`.
    std::expected<void, CommitError> close(NodeId node, Value status) const;
    // An `<ext>` keeping harness data the schema has no place for; `raw` is
    // written as text (compact JSON for JSON payloads).
    std::expected<NodeId, CommitError> ext(NodeId parent, std::string_view harness, std::string_view kind,
                                           std::string raw) const;

  private:
    PatchSink* sink_;
    // Fixed timestamp for imports; `nullopt` stamps the current time.
    std::optional<std::int64_t> ts_;
};

} // namespace loa::core
