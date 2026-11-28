#include "loa/mcp/registry.hpp"

#include <algorithm>

#include "loa/runtime/log.hpp"

namespace loa::mcp {

std::vector<ServerSpec> MemoryStore::servers() const {
    std::lock_guard lock(mutex_);
    return servers_;
}

std::expected<std::vector<ServerSpec>, StoreError> MemoryStore::load() const { return servers(); }

std::expected<void, StoreError> MemoryStore::save(const std::vector<ServerSpec>& servers) const {
    std::lock_guard lock(mutex_);
    servers_ = servers;
    return {};
}

namespace {

Result<std::size_t> position(const std::vector<ServerSpec>& specs, std::string_view name) {
    auto it = std::ranges::find(specs, name, &ServerSpec::name);
    if (it == specs.end()) return std::unexpected(Error::unknown_server(std::string(name)));
    return static_cast<std::size_t>(it - specs.begin());
}

Result<> free(const std::vector<ServerSpec>& specs, std::string_view name) {
    if (std::ranges::find(specs, name, &ServerSpec::name) != specs.end()) return std::unexpected(Error::name_taken(std::string(name)));
    return {};
}

// Where `name` sits, provided renaming it to `next` collides with nothing.
Result<std::size_t> editable(const std::vector<ServerSpec>& specs, std::string_view name, std::string_view next) {
    auto index = position(specs, name);
    if (!index) return index;
    if (next != name)
        if (auto ok = free(specs, next); !ok) return std::unexpected(std::move(ok.error()));
    return index;
}

} // namespace

Result<std::unique_ptr<Registry>> Registry::open(std::shared_ptr<const ServerStore> store, Manager manager) {
    auto specs = store->load();
    if (!specs) return std::unexpected(Error::store(specs.error()));
    for (const auto& spec : *specs)
        if (auto added = manager.add(spec); !added)
            runtime::log::warn("loa_mcp", spec.name + ": not starting a stored server: " + added.error().to_string());
    return std::unique_ptr<Registry>(new Registry(std::move(store), std::move(manager), std::move(*specs)));
}

std::vector<ServerSpec> Registry::specs() const {
    std::lock_guard lock(mutex_);
    return specs_;
}

Result<std::vector<model::Tool>> Registry::add(ServerSpec spec) const {
    if (auto ok = spec.validate(); !ok) return std::unexpected(std::move(ok.error()));
    if (auto ok = free(specs(), spec.name); !ok) return std::unexpected(std::move(ok.error()));
    auto tools = manager_.probe(spec);
    if (!tools) return tools;
    auto saved = commit<void>([&](std::vector<ServerSpec>& specs) -> Result<> {
        if (auto ok = free(specs, spec.name); !ok) return ok;
        specs.push_back(spec);
        return {};
    });
    if (!saved) return std::unexpected(std::move(saved.error()));
    if (auto added = manager_.add(std::move(spec)); !added) return std::unexpected(std::move(added.error()));
    return tools;
}

Result<std::vector<model::Tool>> Registry::edit(std::string_view name, ServerSpec spec) const {
    if (auto ok = spec.validate(); !ok) return std::unexpected(std::move(ok.error()));
    if (auto ok = editable(specs(), name, spec.name); !ok) return std::unexpected(std::move(ok.error()));
    auto tools = manager_.probe(spec);
    if (!tools) return tools;
    auto saved = commit<void>([&](std::vector<ServerSpec>& specs) -> Result<> {
        auto index = editable(specs, name, spec.name);
        if (!index) return std::unexpected(std::move(index.error()));
        specs[*index] = spec;
        return {};
    });
    if (!saved) return std::unexpected(std::move(saved.error()));
    if (auto replaced = manager_.replace(name, std::move(spec)); !replaced) return std::unexpected(std::move(replaced.error()));
    return tools;
}

Result<> Registry::remove(std::string_view name) const {
    auto saved = commit<void>([&](std::vector<ServerSpec>& specs) -> Result<> {
        auto index = position(specs, name);
        if (!index) return std::unexpected(std::move(index.error()));
        specs.erase(specs.begin() + static_cast<std::ptrdiff_t>(*index));
        return {};
    });
    if (!saved) return saved;
    auto removed = manager_.remove(name);
    if (!removed && removed.error().kind != Error::Kind::UnknownServer) return std::unexpected(std::move(removed.error()));
    return {};
}

Result<> Registry::set_enabled(std::string_view name, bool enabled) const {
    auto saved = commit<void>([&](std::vector<ServerSpec>& specs) -> Result<> {
        auto index = position(specs, name);
        if (!index) return std::unexpected(std::move(index.error()));
        specs[*index].enabled = enabled;
        return {};
    });
    if (!saved) return saved;
    return enabled ? manager_.start(name) : manager_.stop(name);
}

Result<import::ImportReport> Registry::import_found(std::vector<import::Found> found) const {
    auto report = commit<import::ImportReport>([&](std::vector<ServerSpec>& specs) -> Result<import::ImportReport> {
        auto planned = import::plan_found(specs, std::move(found));
        for (auto& spec : planned.imported()) specs.push_back(std::move(spec));
        return planned;
    });
    if (!report) return report;
    for (auto& spec : report->imported())
        if (auto added = manager_.add(std::move(spec)); !added) return std::unexpected(std::move(added.error()));
    return report;
}

} // namespace loa::mcp
