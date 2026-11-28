#include "loa/app/view/panel.hpp"

namespace loa::app::view {

namespace domain = ::loa::settings::app;
namespace shown = ::loa::view::panel;

shown::RemoteHost host_to_view(std::string_view alias, const domain::RemoteHost& entry) {
    return shown::RemoteHost{
        .alias = std::string(alias),
        .hostname = entry.host,
        .user = entry.user,
        .port = entry.port,
        .proxy_jump = entry.proxy_jump,
        .limits =
            shown::HostLimits{
                .allowed_paths = entry.allowed_paths,
                .read_only = entry.read_only,
                .commands_allowed = entry.commands,
                .auto_approve = entry.auto_approve,
            },
    };
}

std::pair<std::string, domain::RemoteHost> host_from_view(shown::RemoteHost host) {
    domain::RemoteHost entry{
        .host = std::move(host.hostname),
        .user = std::move(host.user),
        .port = host.port,
        .proxy_jump = std::move(host.proxy_jump),
        .allowed_paths = std::move(host.limits.allowed_paths),
        .read_only = host.limits.read_only,
        .commands = host.limits.commands_allowed,
        .auto_approve = host.limits.auto_approve,
    };
    return {std::move(host.alias), std::move(entry)};
}

} // namespace loa::app::view
