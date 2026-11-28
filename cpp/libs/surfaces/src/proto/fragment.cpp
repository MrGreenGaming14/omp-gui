#include "loa/surfaces/proto/fragment.hpp"

#include <algorithm>
#include <limits>

#include "loa/runtime/proto.hpp"

namespace loa::surfaces::proto {

namespace cs = loa::contracts::surface;

namespace {

Datagram datagram(std::uint64_t seq, std::uint32_t index, std::uint32_t count, std::optional<FrameHeader> header,
                  Bytes data) {
    return {.kind = Fragment{.seq = seq, .index = index, .count = count, .header = std::move(header), .data = std::move(data)}};
}

Bytes join(std::vector<std::optional<Bytes>>& parts) {
    if (parts.size() == 1) return std::move(*parts.front());
    std::size_t total = 0;
    for (const auto& p : parts) total += p->size();
    Bytes data;
    data.reserve(total);
    for (const auto& p : parts) data.insert(data.end(), p->begin(), p->end());
    return data;
}

} // namespace

Result<void> fragment(const cs::Frame& frame, std::size_t max_datagram, const std::function<Result<void>(Bytes)>& emit) {
    const auto header = FrameHeader::from(frame);
    // Worst case index and count with empty data (omitted on the wire): add the
    // data tag, its length varint and the outer varint growth.
    const auto room = [&](std::optional<FrameHeader> h) -> std::size_t {
        constexpr auto max32 = std::numeric_limits<std::uint32_t>::max();
        const auto empty = encode_to_vec(datagram(frame.seq, max32, max32, std::move(h), {})).size() + 1 +
                           2 * loa::runtime::proto::varint_size(max_datagram);
        return max_datagram > empty ? max_datagram - empty : 0;
    };
    const std::size_t first_room = room(header);
    const std::size_t rest_room = room(std::nullopt);
    if (first_room == 0 || rest_room == 0) return std::unexpected(ProtoError::datagram_too_small(max_datagram));
    const std::size_t len = frame.data.size();
    const std::size_t count = len <= first_room ? 1 : 1 + (len - first_room + rest_room - 1) / rest_room;
    if (count > MAX_FRAGMENTS) return std::unexpected(ProtoError::frame_too_large(len, max_datagram));
    std::optional<FrameHeader> first = header;
    std::size_t start = 0;
    for (std::uint32_t index = 0; index < count; ++index) {
        const std::size_t end = std::min(start + (index == 0 ? first_room : rest_room), len);
        Bytes slice(frame.data.begin() + static_cast<std::ptrdiff_t>(start), frame.data.begin() + static_cast<std::ptrdiff_t>(end));
        auto encoded = encode_to_vec(datagram(frame.seq, index, static_cast<std::uint32_t>(count), std::exchange(first, std::nullopt),
                                              std::move(slice)));
        if (auto r = emit(std::move(encoded)); !r) return r;
        start = end;
    }
    return {};
}

Pushed Reassembler::push_datagram(std::span<const std::uint8_t> bytes) {
    auto decoded = decode_datagram(bytes);
    if (!decoded || !decoded->kind) return {};
    return push_fragment(std::get<Fragment>(std::move(*decoded->kind)));
}

Pushed Reassembler::push_fragment(Fragment f) {
    if (f.count == 0 || f.count > MAX_FRAGMENTS || f.index >= f.count || is_stale(f.seq) || (f.index == 0 && !f.header))
        return {};
    auto it = std::ranges::find(pending_, f.seq, &Partial::seq);
    if (it == pending_.end()) {
        if (pending_.size() == MAX_PENDING) pending_.pop_front();
        auto pos = std::ranges::partition_point(pending_, [&](const Partial& p) { return p.seq < f.seq; });
        it = pending_.insert(pos, Partial{.seq = f.seq, .header = std::nullopt, .parts = std::vector<std::optional<Bytes>>(f.count), .have = 0});
    }
    Partial& partial = *it;
    if (partial.parts.size() != f.count || partial.parts[f.index]) return {};
    partial.parts[f.index] = std::move(f.data);
    ++partial.have;
    if (f.index == 0) partial.header = std::move(f.header);
    if (partial.have < f.count) return {};
    Partial done = std::move(partial);
    pending_.erase(it);
    if (!done.header) return {};
    auto frame = done.header->into_frame(join(done.parts));
    if (!frame) return {};
    return push_frame(std::move(*frame));
}

Pushed Reassembler::push_frame(cs::Frame frame) {
    if (is_stale(frame.seq)) return {};
    const bool lost = last_ && frame.seq > *last_ + 1;
    last_ = frame.seq;
    // Anything older still in reassembly is lost now.
    std::erase_if(pending_, [&](const Partial& p) { return p.seq <= frame.seq; });
    if (frame.keyframe) {
        awaiting_keyframe_ = false;
        return {.frame = std::move(frame), .request_keyframe = false};
    }
    const bool video = is_video(frame.encoding);
    awaiting_keyframe_ = awaiting_keyframe_ || (lost && video);
    Pushed out{.frame = std::nullopt, .request_keyframe = lost};
    if (!(video && awaiting_keyframe_)) out.frame = std::move(frame);
    return out;
}

} // namespace loa::surfaces::proto
