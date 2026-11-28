#pragma once

// `Authorize`/`Persist` ports over the `loa::core::testkit` fixtures. Header
// only; link `loa::core_testkit`.

#include <expected>
#include <mutex>

#include "loa/bus/writer.hpp"
#include "loa/core/testkit.hpp"

namespace loa::bus::testkit {

class AllowAll final : public Authorize {
  public:
    std::expected<void, Denied> authorize(const SessionId&, const NodeKey&, const core::Draft&,
                                          const core::Dom&) const override {
        return {};
    }
};

class DenyAll final : public Authorize {
  public:
    std::expected<void, Denied> authorize(const SessionId&, const NodeKey&, const core::Draft&,
                                          const core::Dom&) const override {
        return std::unexpected(Denied{"denied by test"});
    }
};

// Persists into a `core::testkit::MemoryPersist`; copies of it share the list.
class MemoryPersist final : public Persist {
  public:
    explicit MemoryPersist(core::testkit::MemoryPersist log = {}) : log_(std::move(log)) {}

    std::expected<void, PersistError> append(const core::Patch& patch) override {
        if (log_.fail) return std::unexpected(loa::Error(ErrorCode::io, *log_.fail));
        std::scoped_lock lock(log_.patches->mutex);
        log_.patches->patches.push_back(patch);
        return {};
    }

  private:
    core::testkit::MemoryPersist log_;
};

} // namespace loa::bus::testkit
