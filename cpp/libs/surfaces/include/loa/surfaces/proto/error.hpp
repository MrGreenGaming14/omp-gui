#pragma once

// Errors of `omp/surface/1` (port of `ProtoError` in `loa-surfaces/src/proto/mod.rs`).

#include <cstddef>
#include <cstdint>
#include <expected>
#include <string>

#include "loa/contracts/surface.hpp"
#include "loa/runtime/error.hpp"

namespace loa::surfaces::proto {

enum class RejectCode : std::int32_t { Unspecified = 0, Version = 1, NotFound = 2, Unavailable = 3 };

class DatagramError {
  public:
    enum class Kind : std::uint8_t { TooLarge, Unsupported, Closed };

    [[nodiscard]] static DatagramError too_large(std::size_t len, std::size_t max) { return {Kind::TooLarge, len, max}; }
    [[nodiscard]] static DatagramError unsupported() { return {Kind::Unsupported, 0, 0}; }
    [[nodiscard]] static DatagramError closed() { return {Kind::Closed, 0, 0}; }

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    [[nodiscard]] std::size_t len() const noexcept { return len_; }
    [[nodiscard]] std::size_t max() const noexcept { return max_; }
    [[nodiscard]] std::string message() const;

    friend bool operator==(const DatagramError&, const DatagramError&) = default;

  private:
    DatagramError(Kind kind, std::size_t len, std::size_t max) : kind_(kind), len_(len), max_(max) {}
    Kind kind_;
    std::size_t len_;
    std::size_t max_;
};

class ProtoError {
  public:
    enum class Kind : std::uint8_t {
        Surface,
        Io,
        Decode,
        Datagram,
        Protocol,
        TooLarge,
        DatagramTooSmall,
        FrameTooLarge,
        Rejected,
        Closed,
        Checksum,
        Video,
    };

    ProtoError(contracts::surface::SurfaceError e); // NOLINT: implicit like Rust `#[from]`
    ProtoError(DatagramError e);                    // NOLINT

    [[nodiscard]] static ProtoError io(std::string message);
    [[nodiscard]] static ProtoError decode(const loa::Error& e);
    [[nodiscard]] static ProtoError protocol(std::string what);
    [[nodiscard]] static ProtoError too_large(std::uint64_t len, std::uint64_t max);
    [[nodiscard]] static ProtoError datagram_too_small(std::size_t max_datagram);
    [[nodiscard]] static ProtoError frame_too_large(std::size_t len, std::size_t max_datagram);
    [[nodiscard]] static ProtoError rejected(RejectCode code, std::string reason);
    [[nodiscard]] static ProtoError closed(std::string reason);
    [[nodiscard]] static ProtoError checksum();
    [[nodiscard]] static ProtoError video(std::string message);

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // Protocol/Closed/Io/Decode/Video/Rejected text.
    [[nodiscard]] const std::string& detail() const noexcept { return detail_; }
    // Rejected only.
    [[nodiscard]] RejectCode reject_code() const noexcept { return code_; }
    [[nodiscard]] std::uint64_t len() const noexcept { return len_; }
    [[nodiscard]] std::uint64_t max() const noexcept { return max_; }
    [[nodiscard]] const std::optional<contracts::surface::SurfaceError>& surface() const noexcept { return surface_; }
    [[nodiscard]] const std::optional<DatagramError>& datagram() const noexcept { return datagram_; }
    // The Rust `Display` text.
    [[nodiscard]] std::string message() const;
    [[nodiscard]] loa::Error to_error() const;

  private:
    explicit ProtoError(Kind kind) : kind_(kind) {}
    Kind kind_;
    std::string detail_;
    RejectCode code_ = RejectCode::Unspecified;
    std::uint64_t len_ = 0;
    std::uint64_t max_ = 0;
    std::optional<contracts::surface::SurfaceError> surface_;
    std::optional<DatagramError> datagram_;
};

template <typename T>
using Result = std::expected<T, ProtoError>;

} // namespace loa::surfaces::proto
