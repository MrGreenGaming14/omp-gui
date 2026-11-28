#pragma once

// Every error of the node, its transports and its protocols (Rust `NodeError`).

#include <expected>
#include <string>
#include <utility>

namespace loa::p2p {

class NodeError {
  public:
    enum class Kind {
        io,
        net,
        connect,
        wrong_peer,
        protocol,
        decode,
        invalid_key,
        invalid_address,
        refused,
        timeout,
        unavailable,
    };

    NodeError(Kind kind, std::string message) : kind_(kind), message_(std::move(message)) {}

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // The payload: for `refused` the peer's refusal text, for `protocol` the problem.
    [[nodiscard]] const std::string& message() const noexcept { return message_; }
    // Rust `Display`.
    [[nodiscard]] std::string to_string() const;

  private:
    Kind kind_;
    std::string message_;
};

template <class T = void>
using Result = std::expected<T, NodeError>;

[[nodiscard]] inline std::unexpected<NodeError> fail(NodeError::Kind kind, std::string message) {
    return std::unexpected(NodeError(kind, std::move(message)));
}

} // namespace loa::p2p
