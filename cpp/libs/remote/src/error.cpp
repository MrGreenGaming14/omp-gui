#include "loa/remote/error.hpp"

#include <utility>

#include "loa/remote/config.hpp"

namespace loa::remote {

Error Error::io(std::string path, std::string message) {
    auto text = "reading " + path + ": " + message;
    return {ErrorKind::io, std::move(path), std::move(message), std::move(text)};
}

Error Error::resolve(std::string host, std::string message) {
    auto text = "resolving " + host + ": " + message;
    return {ErrorKind::resolve, std::move(host), std::move(message), std::move(text)};
}

Error Error::unknown_host(std::string host) {
    auto text = "host `" + host + "` is not configured";
    return {ErrorKind::unknown_host, std::move(host), {}, std::move(text)};
}

Error Error::proxy_jump_cycle(std::string host) {
    auto text = "ProxyJump chain for `" + host + "` loops back on itself";
    return {ErrorKind::proxy_jump_cycle, std::move(host), {}, std::move(text)};
}

Error Error::proxy_jump_too_deep(std::string host) {
    auto text = "ProxyJump chain for `" + host + "` is deeper than " + std::to_string(MAX_JUMPS) + " hops";
    return {ErrorKind::proxy_jump_too_deep, std::move(host), {}, std::move(text)};
}

Error Error::no_agent(std::string host) {
    auto text = "no ssh-agent socket for " + host + ": set IdentityAgent or SSH_AUTH_SOCK";
    return {ErrorKind::no_agent, std::move(host), {}, std::move(text)};
}

Error Error::agent(std::string path, std::string message) {
    auto text = "ssh-agent at " + path + ": " + message;
    return {ErrorKind::agent, std::move(path), std::move(message), std::move(text)};
}

Error Error::auth_failed(std::string user, std::string host, std::size_t tried) {
    auto text = "authentication as " + user + "@" + host + " failed: none of the " + std::to_string(tried) +
                " agent identities was accepted";
    Error e{ErrorKind::auth_failed, std::move(host), std::move(user), std::move(text)};
    e.tried_ = tried;
    return e;
}

Error Error::host_key_rejected(std::string host, std::string reason) {
    auto text = "host key for " + host + " refused: " + reason;
    return {ErrorKind::host_key_rejected, std::move(host), std::move(reason), std::move(text)};
}

Error Error::ssh(std::string host, std::string message) {
    auto text = "ssh to " + host + ": " + message;
    return {ErrorKind::ssh, std::move(host), std::move(message), std::move(text)};
}

Error Error::connect(std::string host, std::string message) {
    auto text = "connecting to " + host + ": " + message;
    return {ErrorKind::connect, std::move(host), std::move(message), std::move(text)};
}

Error Error::connect_timeout(std::string host) {
    auto text = "connecting to " + host + " timed out";
    return {ErrorKind::connect_timeout, std::move(host), {}, std::move(text)};
}

Error Error::sftp(std::string host, std::string message) {
    auto text = "sftp on " + host + ": " + message;
    return {ErrorKind::sftp, std::move(host), std::move(message), std::move(text)};
}

Error Error::known_hosts(std::string path, std::string message) {
    auto text = "known_hosts " + path + ": " + message;
    return {ErrorKind::known_hosts, std::move(path), std::move(message), std::move(text)};
}

Error Error::refused(std::string reason) {
    auto text = "refused by host limits: " + reason;
    return {ErrorKind::refused, {}, std::move(reason), std::move(text)};
}

Error Error::denied(std::optional<std::string> reason) {
    std::string text = "denied by the user";
    if (reason) text += ": " + *reason;
    return {ErrorKind::denied, {}, reason.value_or(""), std::move(text)};
}

Error Error::invalid_request(std::string message) {
    auto text = message;
    return {ErrorKind::invalid_request, {}, std::move(message), std::move(text)};
}

Error Error::too_large(std::string path, std::uint64_t size, std::uint64_t max) {
    auto text = "file " + path + " is " + std::to_string(size) + " bytes, over the " + std::to_string(max) + " byte limit";
    return {ErrorKind::too_large, std::move(path), {}, std::move(text)};
}

Error Error::edit(std::string path, std::string message) {
    auto text = "edit of " + path + ": " + message;
    return {ErrorKind::edit, std::move(path), std::move(message), std::move(text)};
}

Error Error::audit(std::string message) {
    auto text = "audit log: " + message;
    return {ErrorKind::audit, {}, std::move(message), std::move(text)};
}

Error Error::cancelled() { return {ErrorKind::cancelled, {}, {}, "cancelled"}; }

Error Error::timed_out() { return {ErrorKind::timed_out, {}, {}, "timed out"}; }

} // namespace loa::remote
