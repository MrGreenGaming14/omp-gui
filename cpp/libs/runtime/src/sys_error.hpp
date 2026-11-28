#pragma once

// Private: OS errors as `loa::Error`, worded like Rust's `io::Error` display.

#include <cerrno>
#include <cstring>
#include <string>
#include <system_error>

#include "loa/runtime/error.hpp"

namespace loa::runtime::detail {

[[nodiscard]] inline ErrorCode code_of(int err) noexcept {
    switch (err) {
    case ENOENT: return ErrorCode::not_found;
    case EEXIST: return ErrorCode::already_exists;
    case EACCES:
    case EPERM: return ErrorCode::permission_denied;
    case ETIMEDOUT: return ErrorCode::timeout;
    case EINVAL: return ErrorCode::invalid_argument;
    default: return ErrorCode::io;
    }
}

// "<what>: No such file or directory (os error 2)".
[[nodiscard]] inline Error os_error(int err, const std::string& what) {
    std::string msg = std::strerror(err);
    msg += " (os error " + std::to_string(err) + ")";
    return Error(code_of(err), what.empty() ? msg : what + ": " + msg);
}

[[nodiscard]] inline Error os_error(const std::error_code& ec, const std::string& what) {
    return ec.category() == std::generic_category() || ec.category() == std::system_category()
               ? os_error(ec.value(), what)
               : Error(ErrorCode::io, what + ": " + ec.message());
}

} // namespace loa::runtime::detail
