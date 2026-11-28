#pragma once

#include <memory>
#include <string>
#include <string_view>
#include <utility>

namespace loa {

enum class ErrorCode : int {
    unknown = 0,
    invalid_argument,
    parse,
    io,
    not_found,
    already_exists,
    permission_denied,
    timeout,
    cancelled,
    unsupported,
    internal,
};

[[nodiscard]] std::string_view to_string(ErrorCode code) noexcept;

// The error type of every `std::expected<T, loa::Error>` API: a code, a message
// and an optional underlying cause.
class Error {
  public:
    Error() = default;
    Error(ErrorCode code, std::string message) : code_(code), message_(std::move(message)) {}
    Error(ErrorCode code, std::string message, Error source)
        : code_(code), message_(std::move(message)), source_(std::make_shared<const Error>(std::move(source))) {}

    [[nodiscard]] ErrorCode code() const noexcept { return code_; }
    [[nodiscard]] const std::string& message() const noexcept { return message_; }
    [[nodiscard]] const Error* source() const noexcept { return source_.get(); }

    // Wraps this error as the source of a new one.
    [[nodiscard]] Error context(std::string message) const& { return Error(code_, std::move(message), *this); }

    // "message: source message: ..." in the style of anyhow's `{:#}`.
    [[nodiscard]] std::string to_string() const;

    friend bool operator==(const Error& a, const Error& b) noexcept;

  private:
    ErrorCode code_ = ErrorCode::unknown;
    std::string message_;
    std::shared_ptr<const Error> source_;
};

} // namespace loa
