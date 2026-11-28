#pragma once

// One length-prefixed protobuf codec (4-byte big-endian length, at most
// `MAX_FRAME_SIZE`) and the messages of every node protocol (Rust
// `loa_p2p::wire`). The authenticated transport supplies the peer key; no
// message carries one. Field numbers and oneof tags match the prost structs.

#include <cstddef>
#include <cstdint>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/p2p/error.hpp"
#include "loa/p2p/transport.hpp"

namespace loa::core {
struct Patch;
}

namespace loa::p2p::wire {

// The protocol labels this build serves; dialing checks the exact label.
inline constexpr std::string_view CONTROL = "omp/control/1";
inline constexpr std::string_view SESSION = "omp/session/1";
inline constexpr std::string_view FOLDER = "omp/folder/1";
// Every label in the order a hello announces them.
inline constexpr std::string_view LOCAL[3] = {CONTROL, SESSION, FOLDER};
// Largest encoded message, without its length prefix.
inline constexpr std::size_t MAX_FRAME_SIZE = 1024 * 1024;

// An encoded `loa_bus::proto::Patch` message, carried verbatim so frames
// re-encode byte-identically; `to_wire`/`patches` convert.
using WirePatch = Bytes;

// Accepted when `error` is empty.
struct Ack { std::string error; friend bool operator==(const Ack&, const Ack&) = default; };
struct ProtocolVersion {
    std::string name;
    std::uint32_t version = 0;
    friend bool operator==(const ProtocolVersion&, const ProtocolVersion&) = default;
};
// Both sides send it first on a control stream; `name` is a display hint.
struct Hello {
    std::string app_version;
    std::vector<ProtocolVersion> protocols;
    std::string name;
    friend bool operator==(const Hello&, const Hello&) = default;
};
struct Presence {
    bool online = false;
    std::optional<std::int64_t> last_seen_ms;
    friend bool operator==(const Presence&, const Presence&) = default;
};
// An opaque Biscuit; `root` is empty when the sender minted it.
struct GrantDelivery {
    Bytes token;
    Bytes root;
    friend bool operator==(const GrantDelivery&, const GrantDelivery&) = default;
};
struct GrantRenewal { Bytes grant_id; friend bool operator==(const GrantRenewal&, const GrantRenewal&) = default; };
struct GrantRenewalResult {
    std::optional<Bytes> token;
    std::string refusal;
    friend bool operator==(const GrantRenewalResult&, const GrantRenewalResult&) = default;
};
struct GrantRevocation { Bytes grant_id; friend bool operator==(const GrantRevocation&, const GrantRevocation&) = default; };

// A nonce the requester signs over each grant it presents.
struct Challenge { Bytes nonce; friend bool operator==(const Challenge&, const Challenge&) = default; };
struct PresentedGrant {
    Bytes token;
    Bytes proof;
    friend bool operator==(const PresentedGrant&, const PresentedGrant&) = default;
};
struct Present { std::vector<PresentedGrant> grants; friend bool operator==(const Present&, const Present&) = default; };
struct ListSessions { friend bool operator==(const ListSessions&, const ListSessions&) = default; };
struct SessionEntry {
    std::string id;
    // Empty unless the grant covers the whole session.
    std::string title;
    std::string harness;
    std::int64_t created_ms = 0;
    std::vector<std::string> actions;
    bool partial = false;
    std::uint64_t last_seq = 0;
    friend bool operator==(const SessionEntry&, const SessionEntry&) = default;
};
struct SessionList { std::vector<SessionEntry> sessions; friend bool operator==(const SessionList&, const SessionList&) = default; };
// The requester's visible DOM at the owner's `source_seq`, as patches from 1.
struct Seed {
    std::uint64_t source_seq = 0;
    std::vector<WirePatch> patches;
    std::vector<std::string> actions;
    friend bool operator==(const Seed&, const Seed&) = default;
};
struct DriveResult {
    std::optional<WirePatch> patch;
    std::optional<std::uint64_t> conflict_current;
    std::string error;
    friend bool operator==(const DriveResult&, const DriveResult&) = default;
};
struct Prompt {
    std::string session;
    std::string text;
    friend bool operator==(const Prompt&, const Prompt&) = default;
};
// An answer to a waiting decision, by its id in the peer's view.
struct Answer {
    std::string session;
    std::uint64_t decision = 0;
    std::vector<std::string> values;
    std::optional<std::string> reason;
    friend bool operator==(const Answer&, const Answer&) = default;
};
struct Subscribe {
    std::string session;
    std::uint64_t after_source_seq = 0;
    friend bool operator==(const Subscribe&, const Subscribe&) = default;
};
struct WireUpdate {
    std::uint64_t source_seq = 0;
    std::optional<WirePatch> patch;
    friend bool operator==(const WireUpdate&, const WireUpdate&) = default;
};
// A draft (JSON) against the owner's `base_seq`.
struct Drive {
    std::string session;
    std::uint64_t base_seq = 0;
    Bytes draft;
    std::int64_t ts = 0;
    friend bool operator==(const Drive&, const Drive&) = default;
};

// A folder request (`list`, `read`, `write`) and each later chunk of a write (`data` only).
struct FolderRequest {
    std::string op;
    std::string folder;
    std::string path;
    std::uint64_t size = 0;
    Bytes data;
    friend bool operator==(const FolderRequest&, const FolderRequest&) = default;
};
struct WireEntry {
    std::string name;
    bool dir = false;
    std::uint64_t size = 0;
    friend bool operator==(const WireEntry&, const WireEntry&) = default;
};
// Refused (`error`), go ahead (`ready`), then the result; a read's last chunk is `done`.
struct FolderReply {
    std::string error;
    bool ready = false;
    std::vector<WireEntry> entries;
    Bytes data;
    bool done = false;
    friend bool operator==(const FolderReply&, const FolderReply&) = default;
};

// `omp/control/1` frame; the variant index + 1 is the oneof tag.
using Control = std::variant<Hello, Presence, GrantDelivery, GrantRenewal, GrantRenewalResult, GrantRevocation, Ack>;
struct ControlFrame {
    std::optional<Control> body;
    friend bool operator==(const ControlFrame&, const ControlFrame&) = default;
};
// `omp/session/1` frame; the variant index + 1 is the oneof tag.
using Body = std::variant<Subscribe, WireUpdate, Drive, Challenge, Present, ListSessions, SessionList, Seed, DriveResult,
                          Prompt, Ack, Answer>;
struct Frame {
    std::optional<Body> body;
    friend bool operator==(const Frame&, const Frame&) = default;
};

// Appends the protobuf encoding (no length prefix).
#define LOA_P2P_WIRE_CODEC(M)                                                                                          \
    void encode(const M& message, Bytes& out);                                                                         \
    [[nodiscard]] Result<> decode(std::span<const std::uint8_t> bytes, M& out);
LOA_P2P_WIRE_CODEC(Ack)
LOA_P2P_WIRE_CODEC(ProtocolVersion)
LOA_P2P_WIRE_CODEC(Hello)
LOA_P2P_WIRE_CODEC(Presence)
LOA_P2P_WIRE_CODEC(GrantDelivery)
LOA_P2P_WIRE_CODEC(GrantRenewal)
LOA_P2P_WIRE_CODEC(GrantRenewalResult)
LOA_P2P_WIRE_CODEC(GrantRevocation)
LOA_P2P_WIRE_CODEC(Challenge)
LOA_P2P_WIRE_CODEC(PresentedGrant)
LOA_P2P_WIRE_CODEC(Present)
LOA_P2P_WIRE_CODEC(ListSessions)
LOA_P2P_WIRE_CODEC(SessionEntry)
LOA_P2P_WIRE_CODEC(SessionList)
LOA_P2P_WIRE_CODEC(Seed)
LOA_P2P_WIRE_CODEC(DriveResult)
LOA_P2P_WIRE_CODEC(Prompt)
LOA_P2P_WIRE_CODEC(Answer)
LOA_P2P_WIRE_CODEC(Subscribe)
LOA_P2P_WIRE_CODEC(WireUpdate)
LOA_P2P_WIRE_CODEC(Drive)
LOA_P2P_WIRE_CODEC(FolderRequest)
LOA_P2P_WIRE_CODEC(WireEntry)
LOA_P2P_WIRE_CODEC(FolderReply)
LOA_P2P_WIRE_CODEC(ControlFrame)
LOA_P2P_WIRE_CODEC(Frame)
#undef LOA_P2P_WIRE_CODEC

template <class M>
[[nodiscard]] Bytes encode(const M& message) {
    Bytes out;
    encode(message, out);
    return out;
}

template <class M>
[[nodiscard]] Result<M> decode(std::span<const std::uint8_t> bytes) {
    M message;
    if (auto r = decode(bytes, message); !r) return std::unexpected(r.error());
    return message;
}

// Writes one encoded body with its prefix; an empty or oversized one is refused before writing.
Result<> send_frame(SendHalf& tx, std::span<const std::uint8_t> body);
// The next frame's body; `nullopt` only on a clean end of stream before it.
Result<std::optional<Bytes>> recv_frame(RecvHalf& rx);

// Sends one message.
template <class M>
Result<> send(SendHalf& tx, const M& message) {
    return send_frame(tx, encode(message));
}

// The next message; `nullopt` only on a clean end of stream before it.
template <class M>
Result<std::optional<M>> recv(RecvHalf& rx) {
    auto body = recv_frame(rx);
    if (!body) return std::unexpected(body.error());
    if (!*body) return std::optional<M>();
    auto message = decode<M>(**body);
    if (!message) return std::unexpected(message.error());
    return std::optional<M>(std::move(*message));
}

// Envelopes: answer types that carry an acknowledgement or a refusal.
// An acknowledgement, a refusal when `error` is set.
[[nodiscard]] ControlFrame make_ack(std::type_identity<ControlFrame>, std::optional<std::string> error);
[[nodiscard]] Frame make_ack(std::type_identity<Frame>, std::optional<std::string> error);
[[nodiscard]] FolderReply make_ack(std::type_identity<FolderReply>, std::optional<std::string> error);
// `refused` for a refusal, `protocol` for an empty frame, the message otherwise.
Result<ControlFrame> into_ack(ControlFrame message);
Result<Frame> into_ack(Frame message);
Result<FolderReply> into_ack(FolderReply message);

// The next answer, with refusals and an early end as errors.
template <class M>
Result<M> expect(RecvHalf& rx) {
    auto message = recv<M>(rx);
    if (!message) return std::unexpected(message.error());
    if (!*message) return fail(NodeError::Kind::protocol, "the stream ended early");
    return into_ack(std::move(**message));
}

// Sends `request` and waits for its answer.
template <class Rep, class Req>
Result<Rep> call(Stream& stream, const Req& request) {
    if (auto sent = send(*stream.send, request); !sent) return std::unexpected(sent.error());
    return expect<Rep>(*stream.recv);
}

// Sends an acknowledgement, or a refusal when `error` is set.
template <class M>
Result<> ack(SendHalf& tx, std::optional<std::string> error) {
    return send(tx, make_ack(std::type_identity<M>{}, std::move(error)));
}

// A core patch as a wire patch.
[[nodiscard]] WirePatch to_wire(const loa::core::Patch& patch);
// Decodes wire patches.
[[nodiscard]] Result<std::vector<loa::core::Patch>> patches(std::span<const WirePatch> wire);

} // namespace loa::p2p::wire
