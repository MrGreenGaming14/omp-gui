#include "loa/mcp/error.hpp"

namespace loa::mcp {

std::string Error::to_string() const {
    switch (kind) {
    case Kind::InvalidName: return "`" + name + "` isn't a valid server name: use letters, digits, `-`, `_` or `.`";
    case Kind::EmptyCommand: return "the command line is empty";
    case Kind::UnclosedQuote: return "the command line has an unclosed quote";
    case Kind::Url: return "`" + url + "` isn't a usable MCP url: " + reason;
    case Kind::Header: return "`" + name + "` isn't a valid HTTP header";
    case Kind::ReservedHeader: return "the `" + name + "` header is set by the MCP client and can't be overridden";
    case Kind::UnknownServer: return "no server named `" + name + "`";
    case Kind::NameTaken: return "a server named `" + name + "` already exists";
    case Kind::NotRunning: return "server `" + name + "` isn't running";
    case Kind::NotAllowed: return "tool `" + tool + "` of server `" + server + "` isn't allowed here";
    case Kind::Connect: return "couldn't connect to `" + server + "`: " + message;
    case Kind::Service: return "server `" + server + "`: " + message;
    case Kind::Store: return "server config store: " + message;
    case Kind::HttpClient: return "building the HTTP client: " + message;
    case Kind::Bridge: return "omp bridge: " + message;
    }
    return message;
}

loa::Error Error::to_error() const {
    ErrorCode code = ErrorCode::unknown;
    switch (kind) {
    case Kind::InvalidName:
    case Kind::EmptyCommand:
    case Kind::UnclosedQuote:
    case Kind::Url:
    case Kind::Header:
    case Kind::ReservedHeader: code = ErrorCode::invalid_argument; break;
    case Kind::UnknownServer: code = ErrorCode::not_found; break;
    case Kind::NameTaken: code = ErrorCode::already_exists; break;
    case Kind::NotAllowed: code = ErrorCode::permission_denied; break;
    case Kind::Store: code = ErrorCode::io; break;
    default: code = ErrorCode::unknown; break;
    }
    return loa::Error(code, to_string());
}

} // namespace loa::mcp
