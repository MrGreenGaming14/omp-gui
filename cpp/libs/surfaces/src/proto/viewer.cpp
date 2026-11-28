#include "loa/surfaces/proto/viewer.hpp"

#include <format>
#include <mutex>

#include <zlib.h>

#include "loa/runtime/time.hpp"
#include "pumps.hpp"

namespace loa::surfaces::proto {

namespace cs = loa::contracts::surface;

namespace detail {

using DatagramEvent = std::expected<Bytes, DatagramError>;
using ViewerItem = std::variant<StreamEvent, DatagramEvent>;

struct InputState {
    explicit InputState(std::shared_ptr<Mailbox<StreamMessage>> out) : out(std::move(out)) {}
    // Like dropping the last tokio sender: the write loop drains and ends the stream.
    ~InputState() { out->close_senders(); }

    std::mutex mutex;
    std::uint64_t seq = 1;
    std::shared_ptr<Mailbox<StreamMessage>> out;
};

struct ViewerQueue {
    ViewerQueue(const asio::any_io_executor& ex) : box(std::make_shared<Mailbox<ViewerItem>>(ex)), read_guard(ex), datagram_guard(ex) {}
    ~ViewerQueue() { box->close_receiver(); }

    std::shared_ptr<Mailbox<ViewerItem>> box;
    PumpGuard read_guard;
    PumpGuard datagram_guard;
};

asio::awaitable<void> datagram_pump(std::shared_ptr<Datagrams> datagrams, std::shared_ptr<Mailbox<ViewerItem>> box) {
    for (;;) {
        auto next = co_await datagrams->recv();
        const bool last = !next.has_value();
        if (!box->push(ViewerItem(std::in_place_index<1>, std::move(next))) || last) co_return;
    }
}

asio::awaitable<void> write_loop(MessageWriter writer, std::shared_ptr<Mailbox<StreamMessage>> rx) {
    while (auto msg = co_await rx->pop()) {
        const bool close = msg->body && std::holds_alternative<Close>(*msg->body);
        if (!co_await writer.send(std::move(*msg))) {
            rx->close_receiver();
            co_return;
        }
        if (close) break;
    }
    rx->close_receiver();
    (void)co_await writer.finish();
}

} // namespace detail

namespace {

// Handles the end of the stream and `Close`; `nullopt` for a body to skip.
Result<std::optional<Body>> known_body(detail::StreamEvent next) {
    if (!next) return std::unexpected(std::move(next).error());
    if (!*next) return std::unexpected(ProtoError::closed("the server ended the stream"));
    auto& body = (*next)->body;
    if (!body) return std::optional<Body>{};
    if (auto* close = std::get_if<Close>(&*body)) return std::unexpected(ProtoError::closed(std::move(close->reason)));
    return std::optional<Body>(std::move(*body));
}

// The next known message body; `Close` and the end of the stream end with an error.
asio::awaitable<Result<Body>> next_body(MessageReader& reader) {
    for (;;) {
        auto body = known_body(co_await reader.next());
        if (!body) co_return std::unexpected(std::move(body).error());
        if (*body) co_return std::move(**body);
    }
}

} // namespace

Result<std::uint64_t> InputSender::send(cs::Input input) const {
    std::lock_guard lock(state_->mutex);
    const auto seq = state_->seq;
    cs::InputEvent event{.seq = seq, .sent_us = loa::runtime::time::now_us(), .input = std::move(input)};
    if (!state_->out->push(StreamMessage(Body(InputMsg::from(event))))) return std::unexpected(ProtoError::closed("the session ended"));
    ++state_->seq;
    return seq;
}

Result<void> InputSender::send_message(Body body) const {
    std::lock_guard lock(state_->mutex);
    if (!state_->out->push(StreamMessage(std::move(body)))) return std::unexpected(ProtoError::closed("the session ended"));
    return {};
}

PixelViewer::PixelViewer(cs::SurfaceInfo info, std::optional<cs::StreamInfo> stream, std::shared_ptr<detail::ViewerQueue> queue,
                         InputSender input)
    : info_(std::move(info)), stream_(std::move(stream)), queue_(std::move(queue)), input_(std::move(input)) {}
PixelViewer::PixelViewer(PixelViewer&&) noexcept = default;
PixelViewer& PixelViewer::operator=(PixelViewer&&) noexcept = default;
PixelViewer::~PixelViewer() = default;

Awaitable<Result<ViewerEvent>> PixelViewer::next_event() {
    for (;;) {
        auto item = co_await queue_->box->pop();
        if (!item) co_return std::unexpected(ProtoError::closed("the viewer closed"));
        Pushed pushed;
        if (auto* next = std::get_if<detail::StreamEvent>(&*item)) {
            auto body = known_body(std::move(*next));
            if (!body) co_return std::unexpected(std::move(body).error());
            if (!*body) continue;
            if (auto* frame = std::get_if<FrameMsg>(&**body)) {
                if (!frame->header) co_return std::unexpected(ProtoError::protocol("Frame without header"));
                // Unknown encoding: skip it like a lost frame.
                auto decoded = frame->header->into_frame(std::move(frame->data));
                if (!decoded) continue;
                pushed = reassembler_.push_frame(std::move(*decoded));
            } else if (auto* resize = std::get_if<Resize>(&**body)) {
                co_return ViewerEvent(Resized{.width = resize->width, .height = resize->height});
            } else {
                co_return std::unexpected(ProtoError::protocol("unexpected message in pixel mode"));
            }
        } else {
            auto& datagram = std::get<detail::DatagramEvent>(*item);
            if (!datagram) co_return std::unexpected(ProtoError(datagram.error()));
            pushed = reassembler_.push_datagram(*datagram);
        }
        if (pushed.request_keyframe) {
            if (auto r = input_.send_message(Body(KeyframeRequest{})); !r) co_return std::unexpected(std::move(r).error());
        }
        if (pushed.frame) co_return ViewerEvent(std::move(*pushed.frame));
    }
}

void PixelViewer::close() { (void)input_.send_message(Body(Close{.reason = "the viewer closed"})); }

Awaitable<Result<cs::Document>> DocumentViewer::receive() {
    auto first = co_await next_body(reader_);
    if (!first) co_return std::unexpected(std::move(first).error());
    auto* header = std::get_if<DocumentHeader>(&*first);
    if (!header) co_return std::unexpected(ProtoError::protocol("expected DocumentHeader"));
    if (header->length > max_) {
        auto reason = std::format("document of {} bytes exceeds the {} byte limit", header->length, max_);
        if (auto r = co_await writer_.send(Body(Close{.reason = std::move(reason)})); !r) co_return std::unexpected(std::move(r).error());
        co_return std::unexpected(ProtoError::too_large(header->length, max_));
    }
    Bytes data;
    data.reserve(static_cast<std::size_t>(header->length));
    while (data.size() < header->length) {
        auto next = co_await next_body(reader_);
        if (!next) co_return std::unexpected(std::move(next).error());
        auto* chunk = std::get_if<DocumentChunk>(&*next);
        if (!chunk) co_return std::unexpected(ProtoError::protocol("expected DocumentChunk"));
        if (chunk->offset != data.size() || data.size() + chunk->data.size() > header->length)
            co_return std::unexpected(ProtoError::protocol("document chunk out of place"));
        data.insert(data.end(), chunk->data.begin(), chunk->data.end());
    }
    uLong crc = ::crc32(0L, Z_NULL, 0);
    for (std::size_t off = 0; off < data.size();) {
        const auto n = static_cast<uInt>(std::min<std::size_t>(data.size() - off, 1U << 30U));
        crc = ::crc32(crc, data.data() + off, n);
        off += n;
    }
    if (static_cast<std::uint32_t>(crc) != header->crc32) co_return std::unexpected(ProtoError::checksum());
    if (auto r = co_await writer_.send(Body(Close{.reason = "received"})); !r) co_return std::unexpected(std::move(r).error());
    if (auto r = co_await writer_.finish(); !r) co_return std::unexpected(std::move(r).error());
    co_return cs::Document{.name = std::move(header->name), .mime = std::move(header->mime), .data = std::move(data)};
}

Awaitable<Result<Opened>> open(Transport transport, const cs::SurfaceUri& uri, ViewerOptions options) {
    MessageReader reader(std::move(transport.reader));
    MessageWriter writer(std::move(transport.writer));
    auto datagrams = std::move(transport.datagrams);
    Open open_msg{.version = VERSION, .uri = uri.as_str(), .encodings = {}};
    for (auto e : options.encodings) open_msg.encodings.push_back(encoding_to_wire(e));
    if (auto r = co_await writer.send(Body(std::move(open_msg))); !r) co_return std::unexpected(std::move(r).error());
    auto reply = co_await next_body(reader);
    if (!reply) co_return std::unexpected(std::move(reply).error());
    if (auto* reject = std::get_if<Reject>(&*reply)) {
        auto code = static_cast<RejectCode>(reject->code);
        if (reject->code < 0 || reject->code > static_cast<std::int32_t>(RejectCode::Unavailable)) code = RejectCode::Unspecified;
        co_return std::unexpected(ProtoError::rejected(code, std::move(reject->reason)));
    }
    auto* accept = std::get_if<Accept>(&*reply);
    if (!accept) co_return std::unexpected(ProtoError::protocol("expected Accept or Reject"));
    if (accept->version == 0) co_return std::unexpected(ProtoError::protocol("the server accepted with protocol version 0"));
    if (!accept->info) co_return std::unexpected(ProtoError::protocol("Accept without surface info"));
    auto info = surface_info_from_wire(*accept->info);
    if (!info) co_return std::unexpected(std::move(info).error());
    if (info->uri != uri)
        co_return std::unexpected(ProtoError::protocol(std::format("asked for {}, the server opened {}", uri.as_str(), info->uri.as_str())));
    if (info->mode == cs::SurfaceMode::Document)
        co_return Opened(std::in_place_type<DocumentViewer>, std::move(reader), std::move(writer), options.max_document);

    auto ex = co_await asio::this_coro::executor;
    auto out = std::make_shared<detail::Mailbox<StreamMessage>>(ex);
    asio::co_spawn(ex, detail::write_loop(std::move(writer), out), asio::detached);
    auto queue = std::make_shared<detail::ViewerQueue>(ex);
    queue->read_guard.spawn(detail::read_pump<detail::ViewerItem>(std::make_shared<MessageReader>(std::move(reader)), queue->box));
    if (accept->datagrams) queue->datagram_guard.spawn(detail::datagram_pump(std::move(datagrams), queue->box));
    std::optional<cs::StreamInfo> stream;
    if (accept->stream) stream = stream_info_from_wire(*accept->stream);
    co_return Opened(std::in_place_type<PixelViewer>, std::move(*info), std::move(stream), std::move(queue),
                     InputSender(std::make_shared<detail::InputState>(std::move(out))));
}

} // namespace loa::surfaces::proto
