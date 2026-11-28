#pragma once

// The transport port of one `omp/surface/1` connection (a reliable stream plus
// size-limited unreliable datagrams, as QUIC gives) and an in-memory pair.
// Port of `loa-surfaces/src/proto/transport.rs`.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <memory>
#include <optional>
#include <span>
#include <utility>

#include "loa/contracts/async.hpp"
#include "loa/surfaces/proto/error.hpp"
#include "loa/surfaces/proto/wire.hpp"

namespace loa::surfaces::proto {

using contracts::Awaitable;

// Unreliable datagrams of a connection (the Rust `Datagrams` trait).
class Datagrams {
  public:
    virtual ~Datagrams() = default;

    // Largest datagram `send` accepts right now; `nullopt` without datagrams (TCP fallback).
    [[nodiscard]] virtual std::optional<std::size_t> max_size() const = 0;
    // Queues one datagram; a full send buffer drops it silently.
    virtual std::expected<void, DatagramError> send(Bytes datagram) = 0;
    // The next datagram. Must be cancel safe: a cancelled wait loses no datagram.
    [[nodiscard]] virtual Awaitable<std::expected<Bytes, DatagramError>> recv() = 0;
};

// The reading half of the reliable stream (Rust `AsyncRead`).
class StreamReader {
  public:
    virtual ~StreamReader() = default;
    // Reads at least one byte into `into`; 0 at a clean end of stream. Must be
    // cancel safe: a cancelled wait consumes nothing.
    [[nodiscard]] virtual Awaitable<Result<std::size_t>> read_some(std::span<std::uint8_t> into) = 0;
};

// The writing half of the reliable stream (Rust `AsyncWrite`).
class StreamWriter {
  public:
    virtual ~StreamWriter() = default;
    [[nodiscard]] virtual Awaitable<Result<void>> write_all(std::span<const std::uint8_t> bytes) = 0;
    // Ends the stream: the peer reads end of stream after the bytes written so far.
    [[nodiscard]] virtual Awaitable<Result<void>> shutdown() = 0;
};

struct Transport {
    std::unique_ptr<StreamReader> reader;
    std::unique_ptr<StreamWriter> writer;
    std::shared_ptr<Datagrams> datagrams;
};

// Two connected in-memory transports on `executor`; `max_datagram == nullopt` is the TCP fallback.
[[nodiscard]] std::pair<Transport, Transport> memory_pair(const asio::any_io_executor& executor,
                                                          std::optional<std::size_t> max_datagram);

// Largest stream message accepted: a whole frame on the TCP fallback.
inline constexpr std::size_t MAX_STREAM_MESSAGE = 64 * 1024 * 1024;

// Reads length delimited `StreamMessage`s. `next` is cancel safe.
class MessageReader {
  public:
    explicit MessageReader(std::unique_ptr<StreamReader> inner) : inner_(std::move(inner)) {}

    // `nullopt` at a clean end of stream.
    [[nodiscard]] Awaitable<Result<std::optional<StreamMessage>>> next();
    // A message already buffered in full, without reading; `nullopt` if none.
    [[nodiscard]] Result<std::optional<StreamMessage>> buffered();

  private:
    std::unique_ptr<StreamReader> inner_;
    Bytes buf_;             // valid bytes are `[start_, end_)`; the rest is spare room
    std::size_t start_ = 0;
    std::size_t end_ = 0;
};

// Writes length delimited `StreamMessage`s, reusing one buffer.
class MessageWriter {
  public:
    explicit MessageWriter(std::unique_ptr<StreamWriter> inner) : inner_(std::move(inner)) {}

    [[nodiscard]] Awaitable<Result<void>> send(StreamMessage msg);
    [[nodiscard]] Awaitable<Result<void>> finish();

  private:
    std::unique_ptr<StreamWriter> inner_;
    Bytes buf_;
};

} // namespace loa::surfaces::proto
