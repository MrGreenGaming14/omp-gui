#pragma once

// `OmpFrames`: omp's envelope on `kit::StdioRpc`. Commands carry their `type` and a string
// `id`; responses name the command; host callbacks are requests; oversized frames arrive as
// `rpc_chunk` sequences (protocol v2) and are reassembled here.

#include <cstdint>
#include <expected>
#include <memory>
#include <optional>
#include <string>
#include <variant>

#include "loa/harness/omp/rpc/command.hpp"

namespace loa::harness::omp::rpc {

// Reassembly limit used until omp advertises its own.
inline constexpr std::uint64_t DEFAULT_REASSEMBLY_LIMIT = 64ull * 1024 * 1024;

// One piece of an oversized frame.
struct Chunk {
    std::string chunk_id;
    std::uint32_t index = 0;
    std::uint32_t count = 0;
    std::uint64_t byte_length = 0;
    // Standard base64 with padding.
    std::string data;

    // The `rpc_chunk` frame: `{"type":"rpc_chunk","chunkId","index","count","byteLength","data"}`.
    [[nodiscard]] Json to_json() const;
    friend bool operator==(const Chunk&, const Chunk&) = default;
};

template <> RpcResult<Chunk> decode_as<Chunk>(const Json& data);

namespace chunk_error {
struct OutOfOrder { std::string chunk_id; std::uint32_t index = 0; friend bool operator==(const OutOfOrder&, const OutOfOrder&) = default; };
struct Interrupted { std::string chunk_id; friend bool operator==(const Interrupted&, const Interrupted&) = default; };
struct TooLarge { std::uint64_t bytes = 0; friend bool operator==(const TooLarge&, const TooLarge&) = default; };
struct Length {
    std::string chunk_id;
    std::uint64_t got = 0;
    std::uint64_t declared = 0;
    friend bool operator==(const Length&, const Length&) = default;
};
struct Base64 { friend bool operator==(const Base64&, const Base64&) = default; };
} // namespace chunk_error

class ChunkError {
  public:
    using Kind = std::variant<chunk_error::OutOfOrder, chunk_error::Interrupted, chunk_error::TooLarge,
                              chunk_error::Length, chunk_error::Base64>;
    template <typename K>
        requires std::is_constructible_v<Kind, K&&>
    ChunkError(K&& kind) : kind_(std::forward<K>(kind)) {}

    [[nodiscard]] const Kind& kind() const noexcept { return kind_; }
    template <typename K>
    [[nodiscard]] const K* get() const noexcept { return std::get_if<K>(&kind_); }
    // The Rust `Display` text.
    [[nodiscard]] std::string message() const;
    friend bool operator==(const ChunkError&, const ChunkError&) = default;

  private:
    Kind kind_;
};

// Decodes standard base64 with canonical padding, appending to `out`; false on bad input.
[[nodiscard]] bool base64_decode(std::string_view text, std::string& out);

// Collects one chunk sequence at a time, enforcing the rules of `rpc.md`: no interleaving,
// no gaps, a declared length, and a size cap.
class Reassembler {
  public:
    // `max_bytes` is the advertised `maxReassembledFrameBytes`.
    explicit Reassembler(std::uint64_t max_bytes) : max_bytes_(max_bytes) {}

    // Drops an open sequence because a non-chunk frame interrupted it.
    std::optional<ChunkError> interrupt();

    // Adds one chunk. Returns the whole frame once the last chunk arrived. Any error drops
    // the open sequence.
    std::expected<std::optional<std::string>, ChunkError> push(const Chunk& chunk);

  private:
    struct Partial {
        std::string chunk_id;
        std::uint32_t count = 0;
        std::uint32_t next = 0;
        std::uint64_t byte_length = 0;
        std::string bytes;
    };
    std::expected<std::optional<std::string>, ChunkError> push_inner(const Chunk& chunk);

    std::uint64_t max_bytes_;
    std::optional<Partial> partial_;
};

// The omp `Framing`. Copies share one reassembler.
class OmpFrames final : public loa::harness::kit::Framing {
  public:
    OmpFrames();

    [[nodiscard]] Json request(const Json& id, std::string_view method, Json params) const override;
    [[nodiscard]] Json notification(std::string_view method, Json params) const override;
    // Host callbacks answer with their whole result frame; an error becomes an error result
    // of the callback's kind.
    [[nodiscard]] Json response(Json id, std::string_view method, RpcResult<Json> result) const override;
    [[nodiscard]] std::optional<loa::harness::kit::Incoming> classify(Json msg) const override;

  private:
    struct State;
    // The frame a chunk completes, if it completes one.
    [[nodiscard]] std::optional<Json> chunk(const Json& msg) const;
    std::shared_ptr<State> state_;
};

} // namespace loa::harness::omp::rpc
