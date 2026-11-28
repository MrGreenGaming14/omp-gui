#pragma once

// Byte streams to and from a stub, and the host side errors.

#include <cstddef>
#include <expected>
#include <memory>
#include <span>
#include <string>
#include <string_view>

#include "loa/runtime/error.hpp"

namespace loa::sandbox {

template <typename T = void>
using Result = std::expected<T, loa::Error>;

// The Rust `SandboxError` variants as `loa::Error`s; `message()` is the Rust `Display` text.
namespace error {
// The stub answered a request with an error: "sandbox: <message>".
[[nodiscard]] loa::Error stub(std::string message);
// "the sandbox stub disconnected".
[[nodiscard]] loa::Error disconnected();
// A provider couldn't start, reach or stop its sandbox: "<provider>: <message>".
[[nodiscard]] loa::Error provider(std::string_view provider, std::string message);
// "isolation: <message>".
[[nodiscard]] loa::Error isolation(std::string message);
// "invalid arguments: <message>".
[[nodiscard]] loa::Error args(std::string message);
// "invalid sandbox settings: <message>".
[[nodiscard]] loa::Error settings(std::string message);
// "no provider for sandbox kind `<kind>`".
[[nodiscard]] loa::Error no_provider(std::string_view kind);
// An OS error: "<what>: <strerror> (os error N)", as Rust's `io::Error`.
[[nodiscard]] loa::Error os(std::string_view what, int err);
} // namespace error

// Blocking read side of a byte stream.
class ByteReader {
  public:
    virtual ~ByteReader() = default;
    // Reads at most `buf.size()` bytes; 0 is the end of the stream.
    [[nodiscard]] virtual Result<std::size_t> read(std::span<char> buf) = 0;
};

// Blocking write side of a byte stream.
class ByteWriter {
  public:
    virtual ~ByteWriter() = default;
    [[nodiscard]] virtual Result<> write_all(std::string_view bytes) = 0;
    // Ends the stream for the other side.
    [[nodiscard]] virtual Result<> shutdown() = 0;
};

// Reads from an owned file descriptor, closed on destruction.
class FdReader final : public ByteReader {
  public:
    explicit FdReader(int fd) noexcept : fd_(fd) {}
    ~FdReader() override;
    FdReader(const FdReader&) = delete;
    FdReader& operator=(const FdReader&) = delete;
    [[nodiscard]] Result<std::size_t> read(std::span<char> buf) override;
    [[nodiscard]] int fd() const noexcept { return fd_; }

  private:
    int fd_;
};

// Writes to an owned file descriptor, closed on destruction or `shutdown`.
class FdWriter final : public ByteWriter {
  public:
    explicit FdWriter(int fd) noexcept : fd_(fd) {}
    ~FdWriter() override;
    FdWriter(const FdWriter&) = delete;
    FdWriter& operator=(const FdWriter&) = delete;
    [[nodiscard]] Result<> write_all(std::string_view bytes) override;
    [[nodiscard]] Result<> shutdown() override;

  private:
    int fd_;
};

// Byte streams to and from a running stub.
struct Channel {
    std::unique_ptr<ByteReader> reader;
    std::unique_ptr<ByteWriter> writer;
};

} // namespace loa::sandbox
