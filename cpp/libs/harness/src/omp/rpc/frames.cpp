#include "loa/harness/omp/rpc/frames.hpp"

#include <algorithm>
#include <limits>
#include <mutex>
#include <type_traits>

#include "de.hpp"
#include "loa/runtime/log.hpp"

namespace loa::harness::omp::rpc {

namespace json = loa::runtime::json;
namespace kit = loa::harness::kit;

Json Chunk::to_json() const {
    return json::Object{{"type", "rpc_chunk"}, {"chunkId", chunk_id}, {"index", std::uint64_t(index)},
                        {"count", std::uint64_t(count)}, {"byteLength", byte_length}, {"data", data}};
}

template <> RpcResult<Chunk> decode_as<Chunk>(const Json& value) {
    Chunk out;
    if (!detail::string(value, "chunkId", out.chunk_id) || !detail::uint32(value, "index", out.index) ||
        !detail::uint32(value, "count", out.count) || !detail::uint64(value, "byteLength", out.byte_length) ||
        !detail::string(value, "data", out.data)) return std::unexpected(detail::json_error("rpc_chunk"));
    return out;
}

std::string ChunkError::message() const {
    return std::visit([](const auto& e) -> std::string {
        using E = std::decay_t<decltype(e)>;
        if constexpr (std::is_same_v<E, chunk_error::OutOfOrder>)
            return "chunk " + std::to_string(e.index) + " of `" + e.chunk_id + "` arrived out of order";
        else if constexpr (std::is_same_v<E, chunk_error::Interrupted>)
            return "chunk sequence `" + e.chunk_id + "` was interrupted by another frame";
        else if constexpr (std::is_same_v<E, chunk_error::TooLarge>)
            return "chunk sequence declares " + std::to_string(e.bytes) + " bytes, over the reassembly limit";
        else if constexpr (std::is_same_v<E, chunk_error::Length>)
            return "chunk sequence `" + e.chunk_id + "` decoded to " + std::to_string(e.got) + " bytes, declared " + std::to_string(e.declared);
        else return "chunk data isn't valid base64";
    }, kind_);
}

bool base64_decode(std::string_view text, std::string& out) {
    // Strict standard alphabet, four-character groups, canonical padding and trailing bits.
    if (text.size() % 4 != 0) return false;
    static constexpr std::string_view alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const auto value = [&](char c) -> int {
        const auto pos = alphabet.find(c);
        return pos == std::string_view::npos ? -1 : static_cast<int>(pos);
    };
    out.reserve(out.size() + text.size() / 4 * 3);
    for (std::size_t i = 0; i < text.size(); i += 4) {
        const int a = value(text[i]), b = value(text[i + 1]);
        const bool pad2 = text[i + 2] == '=', pad3 = text[i + 3] == '=';
        const int c = pad2 ? 0 : value(text[i + 2]), d = pad3 ? 0 : value(text[i + 3]);
        if (a < 0 || b < 0 || c < 0 || d < 0 || (pad2 && !pad3) ||
            (pad2 || pad3) && i + 4 != text.size() ||
            (pad2 && (b & 0x0f)) || (pad3 && !pad2 && (c & 0x03))) return false;
        out.push_back(static_cast<char>((a << 2) | (b >> 4)));
        if (!pad2) out.push_back(static_cast<char>((b << 4) | (c >> 2)));
        if (!pad3) out.push_back(static_cast<char>((c << 6) | d));
    }
    return true;
}

std::optional<ChunkError> Reassembler::interrupt() {
    if (!partial_) return std::nullopt;
    auto id = std::move(partial_->chunk_id);
    partial_.reset();
    return ChunkError(chunk_error::Interrupted{std::move(id)});
}

std::expected<std::optional<std::string>, ChunkError> Reassembler::push(const Chunk& chunk) {
    auto result = push_inner(chunk);
    if (!result) partial_.reset();
    return result;
}

std::expected<std::optional<std::string>, ChunkError> Reassembler::push_inner(const Chunk& chunk) {
    if (!partial_) {
        if (chunk.index != 0 || chunk.count == 0)
            return std::unexpected(ChunkError(chunk_error::OutOfOrder{chunk.chunk_id, chunk.index}));
        if (chunk.byte_length > max_bytes_ || chunk.byte_length > std::numeric_limits<std::size_t>::max())
            return std::unexpected(ChunkError(chunk_error::TooLarge{chunk.byte_length}));
        Partial p{chunk.chunk_id, chunk.count, 0, chunk.byte_length, {}};
        p.bytes.reserve(static_cast<std::size_t>(chunk.byte_length));
        partial_ = std::move(p);
    } else if (partial_->chunk_id != chunk.chunk_id || partial_->next != chunk.index ||
               partial_->count != chunk.count || partial_->byte_length != chunk.byte_length) {
        return std::unexpected(ChunkError(chunk_error::OutOfOrder{chunk.chunk_id, chunk.index}));
    }
    auto& partial = *partial_;
    // Bound the decoded size before decoding; malformed large base64 must not allocate past
    // the advertised cap. The wire's byte count is enforced again after decoding.
    if (chunk.data.size() / 4 > (partial.byte_length - partial.bytes.size()) / 3 + 1)
        return std::unexpected(ChunkError(chunk_error::Length{partial.chunk_id, static_cast<std::uint64_t>(partial.bytes.size()) + chunk.data.size() / 4 * 3, partial.byte_length}));
    if (!base64_decode(chunk.data, partial.bytes)) return std::unexpected(ChunkError(chunk_error::Base64{}));
    const auto got = static_cast<std::uint64_t>(partial.bytes.size());
    if (got > partial.byte_length)
        return std::unexpected(ChunkError(chunk_error::Length{partial.chunk_id, got, partial.byte_length}));
    ++partial.next;
    if (partial.next < partial.count) return std::optional<std::string>{};
    auto done = std::move(*partial_);
    partial_.reset();
    if (got != done.byte_length)
        return std::unexpected(ChunkError(chunk_error::Length{std::move(done.chunk_id), got, done.byte_length}));
    return std::optional<std::string>(std::move(done.bytes));
}

struct OmpFrames::State {
    std::mutex mutex;
    Reassembler reassembler{DEFAULT_REASSEMBLY_LIMIT};
};

OmpFrames::OmpFrames() : state_(std::make_shared<State>()) {}

Json OmpFrames::request(const Json& id, std::string_view method, Json params) const {
    if (!params.is_object()) params = json::Object{};
    if (!params.find("type")) params.set("type", method);
    params.set("id", id);
    return params;
}

Json OmpFrames::notification(std::string_view method, Json params) const {
    Json msg = request(Json{}, method, std::move(params));
    auto* obj = msg.as_object();
    std::erase_if(*obj, [](const auto& kv) { return kv.first == "id"; });
    return msg;
}

Json OmpFrames::response(Json id, std::string_view method, RpcResult<Json> result) const {
    if (result) return std::move(*result);
    if (method == "host_uri_request")
        return json::Object{{"type", "host_uri_result"}, {"id", std::move(id)}, {"isError", true}, {"error", result.error().message()}};
    return json::Object{{"type", "host_tool_result"}, {"id", std::move(id)}, {"isError", true},
                        {"result", json::Object{{"content", json::Array{json::Object{{"type", "text"}, {"text", result.error().message()}}}}}}};
}

std::optional<Json> OmpFrames::chunk(const Json& msg) const {
    auto parsed = decode_as<Chunk>(msg);
    if (!parsed) {
        loa::runtime::log::warn("loa_harness", "dropped an invalid chunk sequence: " + parsed.error().message());
        return std::nullopt;
    }
    std::expected<std::optional<std::string>, ChunkError> assembled = std::optional<std::string>{};
    {
        std::scoped_lock lock(state_->mutex);
        assembled = state_->reassembler.push(*parsed);
    }
    if (!assembled) {
        loa::runtime::log::warn("loa_harness", "dropped an invalid chunk sequence: " + assembled.error().message());
        return std::nullopt;
    }
    if (!*assembled) return std::nullopt;
    auto frame = json::parse(**assembled);
    if (!frame) {
        loa::runtime::log::warn("loa_harness", "dropped an invalid chunk sequence: " + frame.error().message());
        return std::nullopt;
    }
    return *frame;
}

std::optional<kit::Incoming> OmpFrames::classify(Json msg) const {
    const auto* t = msg.find("type");
    const auto kind = t && t->as_string() ? *t->as_string() : std::string{};
    if (kind == "rpc_chunk") {
        auto frame = chunk(msg);
        if (!frame) return std::nullopt;
        const auto* nested = frame->find("type");
        if (nested && nested->as_string() && *nested->as_string() == "rpc_chunk") {
            loa::runtime::log::warn("loa_harness", "a reassembled frame was itself a chunk");
            return std::nullopt;
        }
        return classify(std::move(*frame));
    }
    {
        std::scoped_lock lock(state_->mutex);
        if (auto err = state_->reassembler.interrupt())
            loa::runtime::log::warn("loa_harness", "dropped an interrupted chunk sequence: " + err->message());
        if (kind == "ready") {
            if (const auto* bytes = msg.find("maxReassembledFrameBytes"); bytes && bytes->as_u64())
                state_->reassembler = Reassembler(*bytes->as_u64());
        }
    }
    if (kind == "response") {
        const auto* id = msg.find("id");
        if (id && id->as_string()) {
            RpcResult<Json> result = std::unexpected(RpcError::Command{});
            const auto* success = msg.find("success");
            if (success && success->as_bool() && *success->as_bool()) {
                const auto* data = msg.find("data");
                result = data ? *data : Json{};
            } else {
                const auto* command = msg.find("command"), *error = msg.find("error"), *code = msg.find("code");
                result = std::unexpected(RpcError::Command{
                    command && command->as_string() ? *command->as_string() : std::string{},
                    error && error->as_string() ? *error->as_string() : std::string{},
                    code && code->as_string() ? std::optional(*code->as_string()) : std::nullopt});
            }
            return kit::incoming::Response{*id->as_string(), std::move(result)};
        }
    }
    if (kind == "host_tool_call" || kind == "host_uri_request") {
        const auto* id = msg.find("id");
        return kit::incoming::Request{id ? *id : Json{}, kind, std::move(msg)};
    }
    if (kind == "host_tool_cancel" || kind == "host_uri_cancel") {
        const auto* target = msg.find("targetId");
        return kit::incoming::Cancel{target ? *target : Json{}};
    }
    return kit::incoming::Notification{kind, std::move(msg)};
}

} // namespace loa::harness::omp::rpc
