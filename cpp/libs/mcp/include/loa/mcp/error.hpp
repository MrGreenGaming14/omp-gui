#pragma once

#include <cstdint>
#include <expected>
#include <string>

#include "loa/runtime/error.hpp"

namespace loa::mcp {

// Everything the MCP client, registry and stores report. `to_string` is the Rust `Display` text.
struct Error {
    enum class Kind : std::uint8_t {
        InvalidName,    // name
        EmptyCommand,
        UnclosedQuote,
        Url,            // url, reason
        Header,         // name (the header, or `name: value`)
        ReservedHeader, // name
        UnknownServer,  // name
        NameTaken,      // name
        NotRunning,     // name
        NotAllowed,     // server, tool
        Connect,        // server, message
        Service,        // server, message (the JSON-RPC or transport failure)
        Store,          // message
        HttpClient,     // message
        Bridge,         // message
    };

    Kind kind = Kind::Connect;
    std::string name;
    std::string server;
    std::string tool;
    std::string url;
    std::string reason;
    std::string message;

    [[nodiscard]] static Error invalid_name(std::string name) { return {.kind = Kind::InvalidName, .name = std::move(name)}; }
    [[nodiscard]] static Error empty_command() { return {.kind = Kind::EmptyCommand}; }
    [[nodiscard]] static Error unclosed_quote() { return {.kind = Kind::UnclosedQuote}; }
    [[nodiscard]] static Error bad_url(std::string url, std::string reason) {
        return {.kind = Kind::Url, .url = std::move(url), .reason = std::move(reason)};
    }
    [[nodiscard]] static Error header(std::string name) { return {.kind = Kind::Header, .name = std::move(name)}; }
    [[nodiscard]] static Error reserved_header(std::string name) { return {.kind = Kind::ReservedHeader, .name = std::move(name)}; }
    [[nodiscard]] static Error unknown_server(std::string name) { return {.kind = Kind::UnknownServer, .name = std::move(name)}; }
    [[nodiscard]] static Error name_taken(std::string name) { return {.kind = Kind::NameTaken, .name = std::move(name)}; }
    [[nodiscard]] static Error not_running(std::string name) { return {.kind = Kind::NotRunning, .name = std::move(name)}; }
    [[nodiscard]] static Error not_allowed(std::string server, std::string tool) {
        return {.kind = Kind::NotAllowed, .server = std::move(server), .tool = std::move(tool)};
    }
    [[nodiscard]] static Error connect(std::string server, std::string message) {
        return {.kind = Kind::Connect, .server = std::move(server), .message = std::move(message)};
    }
    [[nodiscard]] static Error service(std::string server, std::string message) {
        return {.kind = Kind::Service, .server = std::move(server), .message = std::move(message)};
    }
    [[nodiscard]] static Error store(std::string message) { return {.kind = Kind::Store, .message = std::move(message)}; }
    [[nodiscard]] static Error http_client(std::string message) { return {.kind = Kind::HttpClient, .message = std::move(message)}; }
    [[nodiscard]] static Error bridge(std::string message) { return {.kind = Kind::Bridge, .message = std::move(message)}; }

    [[nodiscard]] std::string to_string() const;
    // As a `loa::Error` for callers that don't care about the kind.
    [[nodiscard]] loa::Error to_error() const;

    friend bool operator==(const Error&, const Error&) = default;
};

template <typename T = void>
using Result = std::expected<T, Error>;

// A store's failure, as text (Rust `Box<dyn Error>`).
using StoreError = std::string;

} // namespace loa::mcp
