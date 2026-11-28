#include "loa/surfaces/proto/server.hpp"

#include <algorithm>
#include <format>

#include <zlib.h>

#include "loa/contracts/surface_channel.hpp"
#include "loa/surfaces/proto/fragment.hpp"
#include "pumps.hpp"

namespace loa::surfaces::proto {

namespace cs = loa::contracts::surface;

namespace {

struct FrameEvent {
    cs::Result<std::shared_ptr<const cs::Frame>> frame;
    std::uint64_t lagged = 0;
};

using ServerEvent = std::variant<detail::StreamEvent, FrameEvent>;

asio::awaitable<void> frame_pump(std::shared_ptr<cs::FrameReceiver> frames, std::shared_ptr<detail::Mailbox<ServerEvent>> box) {
    for (;;) {
        auto frame = co_await frames->recv();
        const bool last = !frame.has_value();
        const auto lagged = frames->take_lagged();
        if (!box->push(ServerEvent(std::in_place_index<1>, FrameEvent{std::move(frame), lagged})) || last) co_return;
    }
}

std::string encoding_name(cs::Encoding e) {
    switch (e) {
    case cs::Encoding::Rgba: return "Rgba";
    case cs::Encoding::Jpeg: return "Jpeg";
    case cs::Encoding::Png: return "Png";
    case cs::Encoding::H264: return "H264";
    case cs::Encoding::Hevc: return "Hevc";
    }
    return "?";
}

std::uint32_t crc32_of(const Bytes& data) {
    uLong crc = ::crc32(0L, Z_NULL, 0);
    std::size_t off = 0;
    while (off < data.size()) {
        const auto n = static_cast<uInt>(std::min<std::size_t>(data.size() - off, 1U << 30U));
        crc = ::crc32(crc, data.data() + off, n);
        off += n;
    }
    return static_cast<std::uint32_t>(crc);
}

class Session {
  public:
    Session(Transport transport)
        : reader_(std::make_shared<MessageReader>(std::move(transport.reader))),
          writer_(std::move(transport.writer)),
          datagrams_(std::move(transport.datagrams)) {}

    // `nullopt` if the viewer left or its version was rejected.
    asio::awaitable<Result<std::optional<Open>>> read_open() {
        for (;;) {
            auto msg = co_await reader_->next();
            if (!msg) co_return std::unexpected(std::move(msg).error());
            if (!*msg) co_return std::optional<Open>{};
            auto& body = (*msg)->body;
            if (!body) continue;
            if (auto* open = std::get_if<Open>(&*body)) {
                if (open->version == 0) {
                    auto reason = std::format("protocol version 0 is not supported, this server speaks {}", VERSION);
                    if (auto r = co_await reject(RejectCode::Version, std::move(reason)); !r)
                        co_return std::unexpected(std::move(r).error());
                    co_return std::optional<Open>{};
                }
                co_return std::optional<Open>(std::move(*open));
            }
            co_return std::unexpected(co_await violation("expected Open"));
        }
    }

    asio::awaitable<Result<void>> reject(RejectCode code, std::string reason) {
        if (auto r = co_await writer_.send(Body(Reject{.code = static_cast<std::int32_t>(code), .reason = std::move(reason)})); !r)
            co_return r;
        co_return co_await writer_.finish();
    }

    asio::awaitable<Result<void>> run(std::shared_ptr<cs::Surface> surface, Open open) {
        const auto info = surface->info();
        const bool datagrams = info.mode == cs::SurfaceMode::Pixel && datagrams_->max_size().has_value();
        std::vector<cs::Encoding> encodings;
        for (auto e : open.encodings)
            if (auto enc = encoding_from_wire(e)) encodings.push_back(*enc);
        // Subscribe before accepting so no frame after the keyframe is missed.
        std::shared_ptr<cs::FrameReceiver> frames;
        std::optional<cs::StreamInfo> stream;
        if (info.mode == cs::SurfaceMode::Pixel) {
            auto subscribed = surface->frames();
            if (!subscribed) co_return std::unexpected(ProtoError(subscribed.error()));
            frames = std::move(*subscribed);
            auto negotiated = co_await surface->negotiate(encodings);
            if (!negotiated) co_return std::unexpected(ProtoError(negotiated.error()));
            stream = std::move(*negotiated);
        }
        Accept accept{.version = std::min(open.version, VERSION),
                      .info = surface_info_to_wire(info),
                      .datagrams = datagrams,
                      .stream = stream ? std::optional(stream_info_to_wire(*stream)) : std::nullopt};
        if (auto r = co_await writer_.send(Body(std::move(accept))); !r) co_return r;
        if (frames) co_return co_await pixel(std::move(surface), std::move(frames), std::move(encodings), datagrams, {info.width, info.height});
        co_return co_await document(std::move(surface));
    }

  private:
    asio::awaitable<Result<void>> close(std::string reason) {
        co_return co_await writer_.send(Body(Close{.reason = std::move(reason)}));
    }

    asio::awaitable<ProtoError> violation(std::string what) {
        // The viewer may already be gone.
        (void)co_await close("protocol violation: " + what);
        co_return ProtoError::protocol(std::move(what));
    }

    asio::awaitable<Result<void>> pixel(std::shared_ptr<cs::Surface> surface, std::shared_ptr<cs::FrameReceiver> frames,
                                        std::vector<cs::Encoding> encodings, bool use_datagrams,
                                        std::pair<std::uint32_t, std::uint32_t> size) {
        if (auto r = co_await surface->request_keyframe(); !r) co_return std::unexpected(ProtoError(r.error()));
        auto ex = co_await asio::this_coro::executor;
        auto box = std::make_shared<detail::Mailbox<ServerEvent>>(ex);
        detail::PumpGuard read_guard(ex), frame_guard(ex);
        read_guard.spawn(detail::read_pump<ServerEvent>(reader_, box));
        frame_guard.spawn(frame_pump(std::move(frames), box));
        struct Closer {
            std::shared_ptr<detail::Mailbox<ServerEvent>> box;
            ~Closer() { box->close_receiver(); }
        } closer{box};
        for (;;) {
            auto event = co_await box->pop();
            if (!event) co_return Result<void>{};
            if (auto* next = std::get_if<detail::StreamEvent>(&*event)) {
                if (!*next) co_return std::unexpected(std::move(*next).error());
                if (!**next) co_return Result<void>{};
                auto& body = (**next)->body;
                if (!body) continue;
                if (auto* input = std::get_if<InputMsg>(&*body)) {
                    if (auto ev = input->into_event()) {
                        if (auto r = co_await surface->input(std::move(*ev)); !r) co_return std::unexpected(ProtoError(r.error()));
                    }
                } else if (std::holds_alternative<KeyframeRequest>(*body)) {
                    if (auto r = co_await surface->request_keyframe(); !r) co_return std::unexpected(ProtoError(r.error()));
                } else if (std::holds_alternative<Close>(*body)) {
                    co_return Result<void>{};
                } else {
                    co_return std::unexpected(co_await violation("unexpected message in pixel mode"));
                }
                continue;
            }
            auto& got = std::get<FrameEvent>(*event);
            // Skipped frames: deltas can't be decoded until a keyframe.
            if (got.lagged > 0) {
                if (auto r = co_await surface->request_keyframe(); !r) co_return std::unexpected(ProtoError(r.error()));
            }
            if (!got.frame) {
                if (auto r = co_await close("the surface closed"); !r) co_return r;
                co_return co_await writer_.finish();
            }
            const cs::Frame& frame = **got.frame;
            if (std::ranges::find(encodings, frame.encoding) == encodings.end()) {
                auto reason = std::format("the viewer can't decode {} frames", encoding_name(frame.encoding));
                if (auto r = co_await close(reason); !r) co_return r;
                co_return std::unexpected(ProtoError::closed(std::move(reason)));
            }
            if (size != std::pair{frame.width, frame.height}) {
                size = {frame.width, frame.height};
                if (auto r = co_await writer_.send(Body(Resize{.width = size.first, .height = size.second})); !r) co_return r;
            }
            if (!use_datagrams) {
                if (auto r = co_await writer_.send(Body(FrameMsg{.header = FrameHeader::from(frame), .data = frame.data})); !r) co_return r;
            } else {
                auto sent = send_datagrams(frame);
                if (!sent) co_return std::unexpected(std::move(sent).error());
                if (!*sent) co_return Result<void>{};
            }
        }
    }

    // `false` when the connection is gone.
    Result<bool> send_datagrams(const cs::Frame& frame) {
        const auto max = datagrams_->max_size();
        if (!max) return std::unexpected(ProtoError(DatagramError::unsupported()));
        auto sent = fragment(frame, *max, [this](Bytes datagram) -> Result<void> {
            if (auto r = datagrams_->send(std::move(datagram)); !r) return std::unexpected(ProtoError(r.error()));
            return {};
        });
        if (sent) return true;
        if (const auto& d = sent.error().datagram()) {
            if (d->kind() == DatagramError::Kind::Closed) return false;
            // The path shrank: drop this late frame, the next is cut to fit.
            if (d->kind() == DatagramError::Kind::TooLarge) return true;
        }
        return std::unexpected(std::move(sent).error());
    }

    // Sends the file once, then waits for the viewer to close.
    asio::awaitable<Result<void>> document(std::shared_ptr<cs::Surface> surface) {
        auto doc = co_await surface->document();
        if (!doc) co_return std::unexpected(ProtoError(doc.error()));
        const auto& data = doc->data;
        DocumentHeader header{.name = doc->name, .mime = doc->mime, .length = data.size(), .crc32 = crc32_of(data)};
        if (auto r = co_await writer_.send(Body(std::move(header))); !r) co_return r;
        for (std::size_t offset = 0; offset < data.size(); offset += DOCUMENT_CHUNK) {
            const auto end = std::min(offset + DOCUMENT_CHUNK, data.size());
            DocumentChunk chunk{.offset = offset,
                                .data = Bytes(data.begin() + static_cast<std::ptrdiff_t>(offset), data.begin() + static_cast<std::ptrdiff_t>(end))};
            if (auto r = co_await writer_.send(Body(std::move(chunk))); !r) co_return r;
        }
        for (;;) {
            auto msg = co_await reader_->next();
            if (!msg) co_return std::unexpected(std::move(msg).error());
            if (!*msg) co_return Result<void>{};
            const auto& body = (*msg)->body;
            if (!body) continue;
            if (std::holds_alternative<Close>(*body)) co_return co_await writer_.finish();
            co_return std::unexpected(co_await violation("unexpected message in document mode"));
        }
    }

    std::shared_ptr<MessageReader> reader_;
    MessageWriter writer_;
    std::shared_ptr<Datagrams> datagrams_;
};

} // namespace

Awaitable<Result<void>> serve(std::shared_ptr<cs::Surface> surface, Transport transport) {
    Session session(std::move(transport));
    auto open = co_await session.read_open();
    if (!open) co_return std::unexpected(std::move(open).error());
    if (!*open) co_return Result<void>{};
    const auto uri = surface->info().uri;
    if ((*open)->uri != uri.as_str())
        co_return co_await session.reject(RejectCode::NotFound, std::format("this connection serves {}", uri.as_str()));
    co_return co_await session.run(std::move(surface), std::move(**open));
}

} // namespace loa::surfaces::proto
