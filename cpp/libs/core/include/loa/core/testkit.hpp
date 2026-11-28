#pragma once

// Shared test fixtures (CMake option `LOA_CORE_TESTKIT`, target `loa::core_testkit`).
// `loa::bus` implements its `Authorize`/`Persist` ports for `AllowAll`,
// `DenyAll` and `MemoryPersist` in its own testkit.

#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/core/dom.hpp"

namespace loa::core::testkit {

// Grants every request.
struct AllowAll {};
// Refuses every request.
struct DenyAll {};

// Keeps every persisted patch in memory; copies share the list.
struct MemoryPersist {
    struct Shared {
        std::mutex mutex;
        std::vector<Patch> patches;
    };
    std::shared_ptr<Shared> patches = std::make_shared<Shared>();
    // When set, appends fail with this message.
    std::optional<std::string> fail;
};

// Fluent session tree builder. Each call adds one node under the latest
// fitting parent and records it as `last`. Fixture errors abort the test.
class DomFixture {
  public:
    // A `<session id="s1" harness="omp" node="k" created=1>`.
    [[nodiscard]] static DomFixture new_session();

    // Inserts any node; aborts when the schema refuses it.
    NodeId add(NodeId parent, ElementKind kind, Attrs attrs = {}, std::string_view text = {});
    DomFixture& commit(Draft draft);
    DomFixture& turn();
    DomFixture& message(Role role, std::string_view text);
    DomFixture& call(std::string_view tool, Attrs attrs = {});
    // A decision under the latest call, else the latest turn.
    DomFixture& decision(DecisionKind kind);
    DomFixture& job(JobKind kind);
    [[nodiscard]] Dom build() const { return dom; }

    Dom dom;
    NodeId session;
    NodeId last;

  private:
    NodeId need_turn();
    std::optional<NodeId> turn_;
    std::optional<NodeId> call_;
};

} // namespace loa::core::testkit
