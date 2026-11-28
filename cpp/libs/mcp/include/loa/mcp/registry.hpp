#pragma once

// Adding, editing and removing servers, persisted through a port.

#include <expected>
#include <functional>
#include <memory>
#include <mutex>
#include <string_view>
#include <vector>

#include "loa/mcp/error.hpp"
#include "loa/mcp/import.hpp"
#include "loa/mcp/manager.hpp"
#include "loa/mcp/spec.hpp"

namespace loa::mcp {

// Where the server list lives; the central config implements it. Calls may block.
class ServerStore {
  public:
    virtual ~ServerStore() = default;
    [[nodiscard]] virtual std::expected<std::vector<ServerSpec>, StoreError> load() const = 0;
    // Replaces the stored list.
    [[nodiscard]] virtual std::expected<void, StoreError> save(const std::vector<ServerSpec>& servers) const = 0;
};

// The list in memory, for throwaway profiles and tests.
class MemoryStore final : public ServerStore {
  public:
    MemoryStore() = default;
    explicit MemoryStore(std::vector<ServerSpec> servers) : servers_(std::move(servers)) {}

    [[nodiscard]] std::vector<ServerSpec> servers() const;

    [[nodiscard]] std::expected<std::vector<ServerSpec>, StoreError> load() const override;
    [[nodiscard]] std::expected<void, StoreError> save(const std::vector<ServerSpec>& servers) const override;

  private:
    mutable std::mutex mutex_;
    mutable std::vector<ServerSpec> servers_;
};

// The saved server list kept in step with the running `Manager`.
class Registry {
  public:
    // Loads the stored servers and starts the enabled ones.
    [[nodiscard]] static Result<std::unique_ptr<Registry>> open(std::shared_ptr<const ServerStore> store, Manager manager);

    [[nodiscard]] const Manager& manager() const noexcept { return manager_; }
    [[nodiscard]] std::vector<ServerSpec> specs() const;

    // Applies `change` to a copy of the list and saves it; the list only changes when both
    // succeed, so a failed save rolls back.
    template <typename T>
    [[nodiscard]] Result<T> commit(const std::function<Result<T>(std::vector<ServerSpec>&)>& change) const {
        std::lock_guard lock(mutex_);
        auto next = specs_;
        auto out = change(next);
        if (!out) return out;
        if (auto saved = store_->save(next); !saved) return std::unexpected(Error::store(saved.error()));
        specs_ = std::move(next);
        return out;
    }

    // Saves and starts a new server once it answers; returns its tools.
    [[nodiscard]] Result<std::vector<model::Tool>> add(ServerSpec spec) const;
    // Replaces the server `name` with `spec` (which may rename it), after the same check as `add`.
    [[nodiscard]] Result<std::vector<model::Tool>> edit(std::string_view name, ServerSpec spec) const;
    // Stops the server and deletes it from the store.
    [[nodiscard]] Result<> remove(std::string_view name) const;
    // Turns a server on or off and saves the choice.
    [[nodiscard]] Result<> set_enabled(std::string_view name, bool enabled) const;
    // Imports entries an `import::scan` found and the user picked, saving and starting the new ones.
    [[nodiscard]] Result<import::ImportReport> import_found(std::vector<import::Found> found) const;

  private:
    Registry(std::shared_ptr<const ServerStore> store, Manager manager, std::vector<ServerSpec> specs)
        : store_(std::move(store)), manager_(std::move(manager)), specs_(std::move(specs)) {}

    std::shared_ptr<const ServerStore> store_;
    Manager manager_;
    mutable std::mutex mutex_;
    // Everything stored, including entries the manager can't run, so saving never drops them.
    mutable std::vector<ServerSpec> specs_;
};

} // namespace loa::mcp
