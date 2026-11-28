#pragma once

// Protobuf wire format of patches and DOMs (port of `crates/loa-bus/src/proto.rs`).
//
// The journal stores patches in this format and peers exchange them in it.
// Messages and field numbers (frozen; change a message only by adding fields):
//
//   Value     { oneof kind { bool 1; sint64 2; double 3; string 4; bytes 5; ValueList 6 } }
//   ValueList { repeated Value items = 1 }
//   Attr      { string name = 1; optional Value value = 2 }  -- attribute maps travel sorted by name
//   Patch     { uint64 seq = 1; uint64 node = 2; int64 ts = 3; optional Op op = 4 }
//   Op        { oneof kind { Insert 1; Set 2; Append 3; Remove 4 } }
//   Insert    { uint64 parent = 1; optional uint32 index = 2; string kind = 3; repeated Attr attrs = 4; string text = 5 }
//   Set       { string name = 1; optional Value value = 2 }   -- absent clears the attribute
//   Append    { string text = 1 }
//   Remove    { }
//   Node      { uint64 id = 1; string kind = 2; uint64 parent = 3; packed uint64 children = 4;
//               repeated Attr attrs = 5; string text = 6 }
//   Dom       { uint64 seq = 1; uint64 next_id = 2; repeated Node nodes = 3 }
//
// Encoding matches prost byte for byte: fields in tag order, proto3 defaults
// omitted, set `optional`/oneof fields always written.

#include <cstdint>
#include <expected>
#include <span>
#include <string>
#include <variant>
#include <vector>

#include "loa/core/dom.hpp"
#include "loa/runtime/error.hpp"

namespace loa::bus::proto {

using Bytes = std::vector<std::uint8_t>;

namespace proto_error {
// Malformed protobuf (prost `DecodeError`).
struct Decode {
    std::string description;
    friend bool operator==(const Decode&, const Decode&) = default;
};
struct MissingOp {
    std::uint64_t seq = 0;
    friend bool operator==(const MissingOp&, const MissingOp&) = default;
};
struct MissingValue {
    friend bool operator==(const MissingValue&, const MissingValue&) = default;
};
struct UnknownKind {
    std::string kind;
    friend bool operator==(const UnknownKind&, const UnknownKind&) = default;
};
struct DuplicateAttr {
    std::string name;
    friend bool operator==(const DuplicateAttr&, const DuplicateAttr&) = default;
};
struct Parts {
    core::PartsError error;
    friend bool operator==(const Parts&, const Parts&) = default;
};
} // namespace proto_error

// Why wire data doesn't convert to `loa::core` types.
using ProtoError = std::variant<proto_error::Decode, proto_error::MissingOp, proto_error::MissingValue,
                                proto_error::UnknownKind, proto_error::DuplicateAttr, proto_error::Parts>;

// The Rust `Display` text.
[[nodiscard]] std::string to_string(const ProtoError& e);
[[nodiscard]] loa::Error to_error(const ProtoError& e);

// Appends the encoded message to `out`.
void encode(const core::Value& value, Bytes& out);
void encode(const core::Patch& patch, Bytes& out);
void encode(const core::Node& node, Bytes& out);
void encode(const core::Dom& dom, Bytes& out);

[[nodiscard]] Bytes encode_patch(const core::Patch& patch);
[[nodiscard]] Bytes encode_dom(const core::Dom& dom);

// Decodes one encoded `Patch` message.
[[nodiscard]] std::expected<core::Patch, ProtoError> decode_patch(std::span<const std::uint8_t> bytes);
// Decodes one encoded `Dom` message and checks that it forms a tree.
[[nodiscard]] std::expected<core::Dom, ProtoError> decode_dom(std::span<const std::uint8_t> bytes);
[[nodiscard]] std::expected<core::Value, ProtoError> decode_value(std::span<const std::uint8_t> bytes);

} // namespace loa::bus::proto
