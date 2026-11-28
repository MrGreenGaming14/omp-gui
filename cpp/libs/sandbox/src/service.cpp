#include "loa/sandbox/service.hpp"

#include <asio.hpp>

#include <format>

#include "loa/runtime/fs.hpp"
#include "loa/sandbox/worker.hpp"

namespace loa::sandbox {

namespace json = runtime::json;
using contracts::Awaitable;

namespace {

json::Value path_json(const std::optional<std::filesystem::path>& path) {
    return path ? json::Value(path->string()) : json::Value(nullptr);
}

std::string type_name(const json::Value& v) {
    if (v.is_null()) return "null";
    if (v.is_bool()) return std::format("boolean `{}`", *v.as_bool());
    if (v.is_number()) return "number";
    if (v.is_string()) return std::format("string \"{}\"", *v.as_string());
    if (v.is_array()) return "sequence";
    return "map";
}

loa::Error mistyped(const json::Value& v, std::string_view expected, std::string_view field) {
    return error::settings(std::format("invalid type: {}, expected {} (field `{}`)", type_name(v), expected, field));
}

Awaitable<std::vector<ProviderStatus>> probe_all(Machine machine, SandboxConfig config, ProviderRegistry registry);

} // namespace

json::Value SandboxConfig::to_json() const {
    json::Array hosts;
    for (const auto& host : ssh_hosts) hosts.emplace_back(host);
    return json::Object{
        {"docker_image", docker_image},
        {"docker_network", docker_network},
        {"firecracker_kernel", path_json(firecracker_kernel)},
        {"firecracker_rootfs", path_json(firecracker_rootfs)},
        {"vm_disk", path_json(vm_disk)},
        {"vm_kernel", path_json(vm_kernel)},
        {"ssh_hosts", std::move(hosts)},
    };
}

Result<SandboxConfig> SandboxConfig::from_json(const json::Value& value) {
    const auto* object = value.as_object();
    if (!object) return std::unexpected(error::settings(std::format("invalid type: {}, expected struct SandboxConfig", type_name(value))));
    SandboxConfig config;
    for (const auto& [key, v] : *object) {
        if (key == "docker_image") {
            if (!v.is_string()) return std::unexpected(mistyped(v, "a string", key));
            config.docker_image = *v.as_string();
        } else if (key == "docker_network") {
            if (!v.is_bool()) return std::unexpected(mistyped(v, "a boolean", key));
            config.docker_network = *v.as_bool();
        } else if (key == "firecracker_kernel" || key == "firecracker_rootfs" || key == "vm_disk" || key == "vm_kernel") {
            std::optional<std::filesystem::path> path;
            if (!v.is_null()) {
                if (!v.is_string()) return std::unexpected(mistyped(v, "path string", key));
                path = *v.as_string();
            }
            if (key == "firecracker_kernel") config.firecracker_kernel = std::move(path);
            else if (key == "firecracker_rootfs") config.firecracker_rootfs = std::move(path);
            else if (key == "vm_disk") config.vm_disk = std::move(path);
            else config.vm_kernel = std::move(path);
        } else if (key == "ssh_hosts") {
            const auto* hosts = v.as_array();
            if (!hosts) return std::unexpected(mistyped(v, "a sequence", key));
            config.ssh_hosts.clear();
            for (const auto& host : *hosts) {
                if (!host.is_string()) return std::unexpected(mistyped(host, "a string", key));
                config.ssh_hosts.push_back(*host.as_string());
            }
        }
    }
    return config;
}

Result<SandboxConfig> SandboxConfig::load(const std::filesystem::path& path) {
    auto bytes = runtime::fs::read_optional(path);
    if (!bytes) return std::unexpected(bytes.error());
    if (!*bytes) return SandboxConfig{};
    auto parsed = json::parse(**bytes);
    if (!parsed) return std::unexpected(error::settings(parsed.error().message()));
    return from_json(*parsed);
}

Result<> SandboxConfig::save(const std::filesystem::path& path) const { return runtime::fs::write_json_atomic(path, to_json()); }

SandboxService::SandboxService(SandboxConfig config, std::shared_ptr<const WorkerJournal> journal)
    : config_(std::move(config)), journal_(std::move(journal)), machine_(Machine::self()), registry_(ProviderRegistry::builtin()) {}

SandboxService& SandboxService::with_stub(std::optional<std::filesystem::path> stub) {
    machine_.stub = std::move(stub);
    return *this;
}

SandboxService& SandboxService::with_peers(std::shared_ptr<PeerLink> peers) {
    machine_.peers = std::move(peers);
    return *this;
}

std::vector<std::shared_ptr<SandboxProvider>> SandboxService::providers() const {
    std::vector<std::shared_ptr<SandboxProvider>> out;
    for (const auto& factory : registry_.factories()) {
        if (auto provider = factory->build(machine_, config_)) out.push_back(std::move(provider));
    }
    return out;
}

std::optional<std::pair<contracts::HostToolSpec, std::shared_ptr<const contracts::HostTools>>> SandboxService::host_tool(
    std::shared_ptr<const contracts::Adapter> adapter, const contracts::SessionConfig& templ) const {
    auto providers = this->providers();
    if (providers.empty()) return std::nullopt;
    // The worker gets the sandbox tools instead; nothing of the parent's host tools may reach it.
    auto worker_template = templ;
    worker_template.host_tools.clear();
    worker_template.host_tool_handler = nullptr;
    auto tool = std::make_shared<SandboxTask>(std::move(adapter), std::move(worker_template), journal_, std::move(providers), events_);
    // Only the kinds this machine runs are offered.
    auto spec = tool->spec();
    if (auto schema = json::parse(spec.input_schema)) {
        json::Array kinds;
        for (const auto kind : tool->kinds()) kinds.emplace_back(as_str(kind));
        if (auto* root = schema->as_object()) {
            for (auto& [key, properties] : *root) {
                if (key != "properties" || !properties.as_object()) continue;
                for (auto& [name, property] : *properties.as_object()) {
                    if (name == "kind") property.set("enum", std::move(kinds));
                }
            }
        }
        spec.input_schema = json::dump(*schema);
    }
    auto tools = std::make_shared<contracts::ToolSet>();
    tools->add(std::move(tool));
    return std::pair{std::move(spec), std::shared_ptr<const contracts::HostTools>(std::move(tools))};
}

Awaitable<std::vector<ProviderStatus>> SandboxService::probe() const { return probe_all(machine_, config_, registry_); }

namespace {
Awaitable<std::vector<ProviderStatus>> probe_all(Machine machine, SandboxConfig config, ProviderRegistry registry) {
    std::vector<ProviderStatus> statuses;
    for (const auto& factory : registry.factories()) statuses.push_back(co_await factory->probe(machine, config));
    co_return statuses;
}
} // namespace

} // namespace loa::sandbox
