#pragma once

// Errors of the session features (port of `loa_session::Error`).

#include <cstdint>
#include <expected>
#include <optional>
#include <string>

#include "loa/bus/bus.hpp"
#include "loa/bus/error.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/core/sink.hpp"

namespace loa::session {

class Error {
  public:
    enum class Kind : std::uint8_t {
        Adapter,
        Commit,
        NoSession,
        Busy,
        InvalidSequence,
        ModelFixed,
        Port,
        Journal,
        Bus,
    };

    [[nodiscard]] static Error adapter(contracts::AdapterError error);
    [[nodiscard]] static Error commit(const core::CommitError& error);
    [[nodiscard]] static Error no_session() { return Error(Kind::NoSession, {}); }
    [[nodiscard]] static Error busy() { return Error(Kind::Busy, {}); }
    [[nodiscard]] static Error invalid_sequence() { return Error(Kind::InvalidSequence, {}); }
    [[nodiscard]] static Error model_fixed() { return Error(Kind::ModelFixed, {}); }
    // "feature port failed: {detail}".
    [[nodiscard]] static Error port(std::string detail) { return Error(Kind::Port, std::move(detail)); }
    [[nodiscard]] static Error journal(const bus::JournalError& error);
    [[nodiscard]] static Error bus(const bus::BusError& error);

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // The message without the kind's prefix: the port text, or the wrapped error's display.
    [[nodiscard]] const std::string& detail() const noexcept { return detail_; }
    // The wrapped harness error of `Kind::Adapter`.
    [[nodiscard]] const contracts::AdapterError* adapter_error() const noexcept {
        return adapter_ ? &*adapter_ : nullptr;
    }
    // Same text as the Rust `Display`.
    [[nodiscard]] std::string to_string() const;

  private:
    Error(Kind kind, std::string detail) : kind_(kind), detail_(std::move(detail)) {}

    Kind kind_;
    std::string detail_;
    std::optional<contracts::AdapterError> adapter_;
};

template <typename T = void>
using Result = std::expected<T, Error>;

} // namespace loa::session
