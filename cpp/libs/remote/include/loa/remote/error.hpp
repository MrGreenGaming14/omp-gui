#pragma once

// Everything that can go wrong in the remote server (Rust `loa_remote::Error`).

#include <cstddef>
#include <cstdint>
#include <expected>
#include <optional>
#include <string>
#include <utility>

namespace loa::remote {

enum class ErrorKind {
    io,
    resolve,
    unknown_host,
    proxy_jump_cycle,
    proxy_jump_too_deep,
    no_agent,
    agent,
    auth_failed,
    host_key_rejected,
    ssh,
    connect,
    connect_timeout,
    sftp,
    known_hosts,
    refused,
    denied,
    invalid_request,
    too_large,
    edit,
    audit,
    cancelled,
    timed_out,
};

// One error: its kind, the variant's main field (`host`, `path` or the refusal reason) and
// its `Display` text, which is what the model and the audit log see.
class Error {
  public:
    [[nodiscard]] static Error io(std::string path, std::string message);
    [[nodiscard]] static Error resolve(std::string host, std::string message);
    [[nodiscard]] static Error unknown_host(std::string host);
    [[nodiscard]] static Error proxy_jump_cycle(std::string host);
    [[nodiscard]] static Error proxy_jump_too_deep(std::string host);
    [[nodiscard]] static Error no_agent(std::string host);
    [[nodiscard]] static Error agent(std::string path, std::string message);
    [[nodiscard]] static Error auth_failed(std::string user, std::string host, std::size_t tried);
    [[nodiscard]] static Error host_key_rejected(std::string host, std::string reason);
    [[nodiscard]] static Error ssh(std::string host, std::string message);
    [[nodiscard]] static Error connect(std::string host, std::string message);
    [[nodiscard]] static Error connect_timeout(std::string host);
    // `message` starting with `status:` is the server answering (the channel still works).
    [[nodiscard]] static Error sftp(std::string host, std::string message);
    [[nodiscard]] static Error known_hosts(std::string path, std::string message);
    [[nodiscard]] static Error refused(std::string reason);
    [[nodiscard]] static Error denied(std::optional<std::string> reason);
    [[nodiscard]] static Error invalid_request(std::string message);
    [[nodiscard]] static Error too_large(std::string path, std::uint64_t size, std::uint64_t max);
    [[nodiscard]] static Error edit(std::string path, std::string message);
    [[nodiscard]] static Error audit(std::string message);
    [[nodiscard]] static Error cancelled();
    [[nodiscard]] static Error timed_out();

    [[nodiscard]] ErrorKind kind() const noexcept { return kind_; }
    // `host`, `path` or the subject the variant names.
    [[nodiscard]] const std::string& subject() const noexcept { return subject_; }
    // The variant's `reason`/`message` field.
    [[nodiscard]] const std::string& reason() const noexcept { return reason_; }
    // `AuthFailed::tried`.
    [[nodiscard]] std::size_t tried() const noexcept { return tried_; }
    // The `Display` text.
    [[nodiscard]] const std::string& message() const noexcept { return message_; }

    friend bool operator==(const Error&, const Error&) = default;

  private:
    Error(ErrorKind kind, std::string subject, std::string reason, std::string message)
        : kind_(kind), subject_(std::move(subject)), reason_(std::move(reason)), message_(std::move(message)) {}

    ErrorKind kind_;
    std::string subject_;
    std::string reason_;
    std::string message_;
    std::size_t tried_ = 0;
};

template <typename T = void>
using Result = std::expected<T, Error>;

} // namespace loa::remote
