#pragma once

// Protobuf messages of `omp/surface/1` and their contract conversions (port of
// `loa-surfaces/src/proto/wire.rs`). The stream carries length delimited
// `StreamMessage`s, each datagram one `Datagram`. Unknown body tags decode as
// `body == std::nullopt` and are skipped. Enum fields hold the raw wire `int32`
// like prost, so unknown values survive a round trip.

#include <cstdint>
#include <expected>
#include <optional>
#include <span>
#include <string>
#include <variant>
#include <vector>

#include "loa/contracts/surface.hpp"
#include "loa/runtime/error.hpp"
#include "loa/surfaces/proto/error.hpp"

namespace loa::surfaces::proto {

using Bytes = std::vector<std::uint8_t>;

// Minor revision within the ALPN's major version.
inline constexpr std::uint32_t VERSION = 1;

enum class WireMode : std::int32_t { Unspecified = 0, Pixel = 1, Document = 2 };
enum class WireEncoding : std::int32_t { Unspecified = 0, Rgba = 1, Jpeg = 2, Png = 3, H264 = 4, Hevc = 5 };
enum class WireButton : std::int32_t { Unspecified = 0, Left = 1, Middle = 2, Right = 3, Back = 4, Forward = 5 };

struct Open {
    std::uint32_t version = 0;
    std::string uri;
    std::vector<std::int32_t> encodings; // WireEncoding, packed
    friend bool operator==(const Open&, const Open&) = default;
};

struct SurfaceInfoMsg {
    std::string uri;
    std::string name;
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    std::int32_t mode = 0; // WireMode
    std::optional<std::string> mime;
    friend bool operator==(const SurfaceInfoMsg&, const SurfaceInfoMsg&) = default;
};

struct StreamInfoMsg {
    std::int32_t encoding = 0; // WireEncoding
    std::string encoder;
    bool hardware = false;
    friend bool operator==(const StreamInfoMsg&, const StreamInfoMsg&) = default;
};

struct Accept {
    // Lower of the two peers' versions.
    std::uint32_t version = 0;
    std::optional<SurfaceInfoMsg> info;
    // Pixel mode: frames come as datagrams, else on the stream.
    bool datagrams = false;
    std::optional<StreamInfoMsg> stream;
    friend bool operator==(const Accept&, const Accept&) = default;
};

struct Reject {
    std::int32_t code = 0; // RejectCode
    std::string reason;
    friend bool operator==(const Reject&, const Reject&) = default;
};

struct KeyframeRequest {
    friend bool operator==(const KeyframeRequest&, const KeyframeRequest&) = default;
};

struct Resize {
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    friend bool operator==(const Resize&, const Resize&) = default;
};

struct Close {
    std::string reason;
    friend bool operator==(const Close&, const Close&) = default;
};

struct MouseMoveMsg {
    float x = 0, y = 0;
    friend bool operator==(const MouseMoveMsg&, const MouseMoveMsg&) = default;
};

struct MouseButtonMsg {
    float x = 0, y = 0;
    std::int32_t button = 0; // WireButton
    bool pressed = false;
    // Bits 0-3: shift, ctrl, alt, meta.
    std::uint32_t modifiers = 0;
    friend bool operator==(const MouseButtonMsg&, const MouseButtonMsg&) = default;
};

struct ScrollMsg {
    float x = 0, y = 0, dx = 0, dy = 0;
    std::uint32_t modifiers = 0;
    friend bool operator==(const ScrollMsg&, const ScrollMsg&) = default;
};

struct KeyMsg {
    std::string key;
    std::string code;
    bool pressed = false;
    std::uint32_t modifiers = 0;
    friend bool operator==(const KeyMsg&, const KeyMsg&) = default;
};

struct TextMsg {
    std::string text;
    friend bool operator==(const TextMsg&, const TextMsg&) = default;
};

// Oneof tags 3-8, in this order.
using InputKind = std::variant<MouseMoveMsg, MouseButtonMsg, ScrollMsg, KeyMsg, TextMsg, Resize>;

struct InputMsg {
    std::uint64_t seq = 0;
    std::uint64_t sent_us = 0;
    std::optional<InputKind> kind;
    friend bool operator==(const InputMsg&, const InputMsg&) = default;

    [[nodiscard]] static InputMsg from(const contracts::surface::InputEvent& event);
    // `nullopt` without a kind or with an unknown button.
    [[nodiscard]] std::optional<contracts::surface::InputEvent> into_event() const&;
};

struct FrameHeader {
    std::uint64_t seq = 0;
    std::uint64_t captured_us = 0;
    std::uint64_t input_seq = 0;
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    std::uint32_t region_x = 0;
    std::uint32_t region_y = 0;
    std::uint32_t region_width = 0;
    std::uint32_t region_height = 0;
    std::int32_t encoding = 0; // WireEncoding
    bool keyframe = false;
    friend bool operator==(const FrameHeader&, const FrameHeader&) = default;

    [[nodiscard]] static FrameHeader from(const contracts::surface::Frame& frame);
    // `nullopt` for an unknown encoding.
    [[nodiscard]] std::optional<contracts::surface::Frame> into_frame(Bytes data) const;
};

struct FrameMsg {
    std::optional<FrameHeader> header;
    Bytes data;
    friend bool operator==(const FrameMsg&, const FrameMsg&) = default;
};

// Slice `index` of `count` of frame `seq`. Fragment 0 carries the header.
struct Fragment {
    std::uint64_t seq = 0;
    std::uint32_t index = 0;
    std::uint32_t count = 0;
    std::optional<FrameHeader> header;
    Bytes data;
    friend bool operator==(const Fragment&, const Fragment&) = default;
};

// Oneof tag 1.
using DatagramKind = std::variant<Fragment>;

struct Datagram {
    std::optional<DatagramKind> kind;
    friend bool operator==(const Datagram&, const Datagram&) = default;
};

struct DocumentHeader {
    std::string name;
    std::string mime;
    std::uint64_t length = 0;
    // CRC-32 (IEEE) of the whole file.
    std::uint32_t crc32 = 0;
    friend bool operator==(const DocumentHeader&, const DocumentHeader&) = default;
};

struct DocumentChunk {
    std::uint64_t offset = 0;
    Bytes data;
    friend bool operator==(const DocumentChunk&, const DocumentChunk&) = default;
};

// Viewer: `Open`, then `Input`/`KeyframeRequest` (pixel). Server: `Accept` or
// `Reject`, then `Resize`/`Frame` (pixel, `Frame` only without datagrams) or
// `DocumentHeader` and in-order `DocumentChunk`s. Either side may `Close`.
// Oneof tags 1-10, in this order.
using Body = std::variant<Open, Accept, Reject, InputMsg, KeyframeRequest, Resize, FrameMsg, DocumentHeader,
                          DocumentChunk, Close>;

struct StreamMessage {
    std::optional<Body> body;
    StreamMessage() = default;
    StreamMessage(Body b) : body(std::move(b)) {} // NOLINT: Rust `From<Body>`
    friend bool operator==(const StreamMessage&, const StreamMessage&) = default;
};

// Codec. Encoding appends to `out`; decoding errors are `ErrorCode::parse`.
void encode(Bytes& out, const StreamMessage& msg);
void encode(Bytes& out, const Datagram& msg);
void encode(Bytes& out, const InputMsg& msg);
// Varint length prefix, then the message.
void encode_length_delimited(Bytes& out, const StreamMessage& msg);
[[nodiscard]] Bytes encode_to_vec(const StreamMessage& msg);
[[nodiscard]] Bytes encode_to_vec(const Datagram& msg);
[[nodiscard]] Bytes encode_to_vec(const InputMsg& msg);
[[nodiscard]] std::expected<StreamMessage, loa::Error> decode_stream_message(std::span<const std::uint8_t> bytes);
[[nodiscard]] std::expected<Datagram, loa::Error> decode_datagram(std::span<const std::uint8_t> bytes);
[[nodiscard]] std::expected<InputMsg, loa::Error> decode_input_msg(std::span<const std::uint8_t> bytes);

// Contract enum to wire enum and back (`nullopt` for unknown or `Unspecified`).
[[nodiscard]] std::int32_t encoding_to_wire(contracts::surface::Encoding value) noexcept;
[[nodiscard]] std::optional<contracts::surface::Encoding> encoding_from_wire(std::int32_t value) noexcept;
[[nodiscard]] std::int32_t button_to_wire(contracts::surface::MouseButton value) noexcept;
[[nodiscard]] std::optional<contracts::surface::MouseButton> button_from_wire(std::int32_t value) noexcept;
[[nodiscard]] std::int32_t mode_to_wire(contracts::surface::SurfaceMode value) noexcept;
[[nodiscard]] std::optional<contracts::surface::SurfaceMode> mode_from_wire(std::int32_t value) noexcept;
[[nodiscard]] bool is_video(contracts::surface::Encoding encoding) noexcept;

[[nodiscard]] StreamInfoMsg stream_info_to_wire(const contracts::surface::StreamInfo& stream);
[[nodiscard]] std::optional<contracts::surface::StreamInfo> stream_info_from_wire(const StreamInfoMsg& msg);
[[nodiscard]] SurfaceInfoMsg surface_info_to_wire(const contracts::surface::SurfaceInfo& info);
[[nodiscard]] Result<contracts::surface::SurfaceInfo> surface_info_from_wire(const SurfaceInfoMsg& msg);

[[nodiscard]] std::uint32_t modifiers_to_wire(contracts::surface::Modifiers m) noexcept;
[[nodiscard]] contracts::surface::Modifiers modifiers_from_wire(std::uint32_t bits) noexcept;

} // namespace loa::surfaces::proto
