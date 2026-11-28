#include "loa/trust/error.hpp"

#include <format>

namespace loa::trust {
namespace {

// Rust `{:?}` of a string: quoted, with quotes, backslashes and controls escaped.
std::string debug_quoted(const std::string& text) {
    std::string out = "\"";
    for (const char c : text) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if (static_cast<unsigned char>(c) < 0x20) out += std::format("\\u{{{:x}}}", static_cast<unsigned>(c));
                else out += c;
        }
    }
    return out + "\"";
}

} // namespace

std::string TrustError::to_string() const {
    switch (kind_) {
        case Kind::store: return "key store: " + message_;
        case Kind::unavailable: return "os keychain unavailable: " + message_;
        case Kind::wrong_length:
            return std::format("stored node key has {} bytes, expected {}", found_, SECRET_KEY_LENGTH);
        case Kind::random: return "os random source: " + message_;
        case Kind::key: return "invalid key: " + message_;
        case Kind::sql: return "grant database: " + message_;
        case Kind::json: return "grant encoding: " + message_;
        case Kind::biscuit: return "biscuit: " + message_;
        case Kind::action: return std::format("{} grants don't support `{}`", detail_, message_);
        case Kind::no_scopes: return "a grant needs at least one scope";
        case Kind::no_actions: return "a scope needs at least one action";
        case Kind::ttl: return std::format("token lifetime must be positive, got {} ms", number_);
        case Kind::node_id: return std::format("node id {} doesn't fit a grant", message_);
        case Kind::folder_path: return "a folder grant needs a normalized absolute path, got " + debug_quoted(message_);
        case Kind::unknown_grant: return "unknown grant " + message_;
        case Kind::revoked: return std::format("grant {} is revoked", message_);
        case Kind::expired: return std::format("grant {} has ended", message_);
        case Kind::not_holder: return "the requester doesn't hold grant " + message_;
        case Kind::reshare_forbidden: return std::format("grant {} may not be re-shared", message_);
        case Kind::parent_expired:
            return std::format("the grant {} this one was re-shared from has no current token", message_);
        case Kind::malformed: return "malformed grant token: " + message_;
    }
    return message_;
}

loa::Error TrustError::to_error() const {
    switch (kind_) {
        case Kind::store:
        case Kind::sql: return loa::Error(ErrorCode::io, to_string());
        case Kind::unavailable: return loa::Error(ErrorCode::unsupported, to_string());
        case Kind::wrong_length:
        case Kind::key:
        case Kind::action:
        case Kind::no_scopes:
        case Kind::no_actions:
        case Kind::ttl:
        case Kind::node_id:
        case Kind::folder_path: return loa::Error(ErrorCode::invalid_argument, to_string());
        case Kind::random:
        case Kind::biscuit: return loa::Error(ErrorCode::internal, to_string());
        case Kind::json:
        case Kind::malformed: return loa::Error(ErrorCode::parse, to_string());
        case Kind::unknown_grant: return loa::Error(ErrorCode::not_found, to_string());
        case Kind::revoked:
        case Kind::not_holder:
        case Kind::reshare_forbidden: return loa::Error(ErrorCode::permission_denied, to_string());
        case Kind::expired:
        case Kind::parent_expired: return loa::Error(ErrorCode::timeout, to_string());
    }
    return loa::Error(ErrorCode::unknown, to_string());
}

} // namespace loa::trust
