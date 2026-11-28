#pragma once

// omp2's wire shapes (oh-my-pi `omp2` branch, commit 2f92f3b5): `patch@1` DOM records and the RPC v1
// frames the mapper reads (port of `omp2/wire.rs`). `set_dom_subscription` is assumed: omp2's public
// RPC stream carries no DOM yet.

#include <cstdint>
#include <compare>
#include <expected>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/runtime/json.hpp"

namespace loa::harness::omp2::wire {

using Json = loa::runtime::json::Value;

// A stable omp2 node id; 1 to 4 are the `<session><meta/><body/><queues/></session>` skeleton.
struct Handle {
    std::uint64_t value = 0;
    friend constexpr auto operator<=>(Handle, Handle) = default;
};

inline constexpr Handle ROOT{1};
inline constexpr Handle BODY{3};

// An element name; custom (tool) names carry a `tool:` prefix on the wire or aren't known.
struct Tag {
    std::string name;
    bool custom = false;

    // Parses a wire tag.
    [[nodiscard]] static Tag from_wire(std::string_view wire);
    // True for the structural element `name`.
    [[nodiscard]] bool is(std::string_view n) const { return !custom && name == n; }
};

// A property name without the wire `custom:` prefix.
[[nodiscard]] std::string prop_from_wire(std::string_view wire);

// A node to insert or a snapshot node; props are ordered `[name, value]` pairs.
struct NodeSpec {
    Tag tag;
    std::vector<std::pair<std::string, Json>> props;
    std::vector<Handle> kids;
    std::optional<std::string> content;
};

// `["ins", parent, after, node]`, `["rm", h]`, `["set", h, prop, value]` or `["mv", h, parent, after]`;
// `after: null` is first.
namespace op {
struct Ins { Handle parent; std::optional<Handle> after; NodeSpec node; };
struct Rm { Handle node; };
struct Set { Handle node; std::string prop; Json value; };
struct Mv { Handle node; Handle parent; std::optional<Handle> after; };
} // namespace op
using Op = std::variant<op::Ins, op::Rm, op::Set, op::Mv>;

struct Patch {
    std::vector<Op> ops;
};

enum class StreamOp { Open, Append, Close };

struct SnapshotNode {
    Handle handle;
    std::optional<Handle> parent;
    NodeSpec node;
};

// A stream open at the snapshot; `text` is what it appended so far.
struct SnapshotStream {
    std::uint32_t sid = 0;
    Handle node;
    std::string prop;
    std::string text;
};

struct Snapshot {
    std::uint64_t high_water = 0;
    std::vector<SnapshotNode> nodes;
    std::vector<SnapshotStream> streams;
};

// One replicated DOM record; a snapshot replaces the replica (first on subscription, after branch navigation).
namespace dom_event {
// `patch@1`
struct Patch { wire::Patch data; };
// `stream@1`
struct Stream {
    std::uint32_t sid = 0;
    StreamOp op = StreamOp::Open;
    std::optional<Handle> node;
    std::optional<std::string> prop;
    std::optional<std::string> text;
};
// `snapshot@1`
struct Snapshot { wire::Snapshot data; };
} // namespace dom_event
using DomEvent = std::variant<dom_event::Patch, dom_event::Stream, dom_event::Snapshot>;

// The RPC frames the mapper reads; DOM projections are `Other`.
namespace frame {
struct AgentEnd {
    bool cancelled = false;
    std::optional<std::string> error;
};
struct ToolApprovalRequest {
    std::string prompt_id;
    std::optional<std::string> tool_call_id;
    std::optional<std::string> title;
    std::optional<std::string> body;
    std::optional<std::vector<std::string>> scopes;
    std::optional<std::int64_t> timeout_ms;
};
struct ExtensionUiRequest {
    Json id;
    std::string method;
    std::optional<std::string> title;
    std::vector<std::string> options;
    bool multi = false;
};
struct Other {};
} // namespace frame
using Frame = std::variant<frame::AgentEnd, frame::ToolApprovalRequest, frame::ExtensionUiRequest, frame::Other>;

// One stdout line: DOM records carry `kind`, RPC frames `type`.
using Input = std::variant<DomEvent, Frame>;

// Decodes one stdout line as serde decodes `Input`; the error is the decoder's message.
[[nodiscard]] std::expected<Input, std::string> decode_input(const Json& msg);
[[nodiscard]] std::expected<DomEvent, std::string> decode_dom_event(const Json& msg);
[[nodiscard]] std::expected<Frame, std::string> decode_frame(const Json& msg);

// A mapper reply: `approve` goes out as a request, `extension_ui_response` as a bare frame.
namespace reply {
// Scope is `once`, `call`, `turn`, `session` or `persist`.
struct Approve {
    std::string prompt_id;
    bool approved = false;
    std::string_view scope;
    std::optional<std::string> reason;
    friend bool operator==(const Approve&, const Approve&) = default;
};
// Answers a `select` UI request: picked labels plus typed text.
struct ExtensionUiResponse {
    Json id;
    std::vector<std::string> values;
    std::optional<std::string> custom_input;
    friend bool operator==(const ExtensionUiResponse&, const ExtensionUiResponse&) = default;
};
} // namespace reply
using Reply = std::variant<reply::Approve, reply::ExtensionUiResponse>;

// The reply as serde serializes it (`{"type": "approve", "promptId", …}`).
[[nodiscard]] Json to_json(const Reply& reply);

// One `get_available_models` entry.
struct ModelEntry {
    std::string id;
    std::string name;
    std::string provider;
    bool reasoning = false;
};

// Decodes the `models` array of `get_available_models`.
[[nodiscard]] std::expected<std::vector<ModelEntry>, std::string> decode_models(const Json& models);

} // namespace loa::harness::omp2::wire

namespace loa::harness::omp2 {
using wire::Handle;
using wire::Input;
using wire::ModelEntry;
using wire::NodeSpec;
using wire::Op;
using wire::Patch;
using wire::Reply;
using wire::Tag;
} // namespace loa::harness::omp2
