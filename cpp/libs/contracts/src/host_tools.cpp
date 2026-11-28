#include "loa/contracts/host_tools.hpp"

#include <asio.hpp>

namespace loa::contracts {

std::expected<void, DuplicateHostTool> HostToolRouter::add(HostToolSpec spec, std::shared_ptr<const HostTools> handler) {
    if (handlers_.contains(spec.name)) return std::unexpected(DuplicateHostTool{std::move(spec.name)});
    handlers_.emplace(spec.name, std::move(handler));
    specs_.push_back(std::move(spec));
    return {};
}

std::expected<void, DuplicateHostTool> HostToolRouter::add_all(std::vector<HostToolSpec> specs,
                                                                const std::shared_ptr<const HostTools>& handler) {
    for (auto& spec : specs) {
        if (auto added = add(std::move(spec), handler); !added) return added;
    }
    return {};
}

void HostToolRouter::install(SessionConfig& config) && {
    if (empty()) return;
    config.host_tools.insert(config.host_tools.end(), specs_.begin(), specs_.end());
    config.host_tool_handler = std::make_shared<HostToolRouter>(std::move(*this));
}

Awaitable<std::expected<std::string, std::string>> HostToolRouter::call(std::string session_id, std::string tool,
                                                                        std::string args) const {
    auto it = handlers_.find(tool);
    if (it == handlers_.end()) co_return std::unexpected("unknown host tool `" + tool + "`");
    co_return co_await it->second->call(std::move(session_id), std::move(tool), std::move(args));
}

Awaitable<std::expected<std::vector<OutputBlock>, std::string>> HostToolRouter::call_content(
    std::string session_id, std::string tool, std::string args) const {
    auto it = handlers_.find(tool);
    if (it == handlers_.end()) co_return std::unexpected("unknown host tool `" + tool + "`");
    co_return co_await it->second->call_content(std::move(session_id), std::move(tool), std::move(args));
}

} // namespace loa::contracts
