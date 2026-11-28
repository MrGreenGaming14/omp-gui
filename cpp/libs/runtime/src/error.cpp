#include "loa/runtime/error.hpp"

namespace loa {

std::string_view to_string(ErrorCode code) noexcept {
    switch (code) {
    case ErrorCode::unknown: return "unknown";
    case ErrorCode::invalid_argument: return "invalid_argument";
    case ErrorCode::parse: return "parse";
    case ErrorCode::io: return "io";
    case ErrorCode::not_found: return "not_found";
    case ErrorCode::already_exists: return "already_exists";
    case ErrorCode::permission_denied: return "permission_denied";
    case ErrorCode::timeout: return "timeout";
    case ErrorCode::cancelled: return "cancelled";
    case ErrorCode::unsupported: return "unsupported";
    case ErrorCode::internal: return "internal";
    }
    return "unknown";
}

std::string Error::to_string() const {
    std::string out = message_;
    for (const Error* e = source(); e != nullptr; e = e->source()) {
        out += ": ";
        out += e->message();
    }
    return out;
}

bool operator==(const Error& a, const Error& b) noexcept {
    if (a.code_ != b.code_ || a.message_ != b.message_) return false;
    if (!a.source_ || !b.source_) return !a.source_ && !b.source_;
    return *a.source_ == *b.source_;
}

} // namespace loa
