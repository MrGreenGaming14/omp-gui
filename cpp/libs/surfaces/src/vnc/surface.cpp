// One RFB connection as a pixel surface, with reconnect, and the `vm` surface
// server. Port of `loa-surfaces/src/vnc/{surface,server}.rs`.

#include "loa/surfaces/vnc/surface.hpp"

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/experimental/concurrent_channel.hpp>

#include <algorithm>
#include <atomic>
#include <format>

#include "internal.hpp"
#include "loa/contracts/surface_channel.hpp"
#include "loa/runtime/log.hpp"
#include "loa/runtime/time.hpp"
#include "loa/surfaces/feed.hpp"
#include "loa/surfaces/pixels.hpp"
#include "loa/surfaces/vnc/decode.hpp"
#include "loa/surfaces/vnc/input.hpp"
#include "loa/surfaces/vnc/proto.hpp"

namespace loa::surfaces::vnc {

namespace cs = loa::contracts::surface;
using asio::ip::tcp;
using detail::fail;

namespace {

constexpr std::string_view LOG_TARGET = "loa_surfaces::vnc";
constexpr std::size_t READ_BUFFER = 1U << 16U;

// `a` grown to cover `b`.
Rect union_rect(std::optional<Rect> a, Rect b) {
    if (!a) return b;
    const std::uint32_t x = std::min(a->x, b.x);
    const std::uint32_t y = std::min(a->y, b.y);
    const std::uint32_t right = std::max(a->x + a->width, b.x + b.width);
    const std::uint32_t bottom = std::max(a->y + a->height, b.y + b.height);
    return {x, y, right - x, bottom - y};
}

RfbError io_error(const asio::error_code& ec) {
    return ec == asio::error::eof ? RfbError::io("early eof") : RfbError::io(ec.message());
}

} // namespace

// ---- CursorWatch ----

struct CursorWatch::Impl {
    using Signal = asio::experimental::concurrent_channel<void(asio::error_code)>;
    mutable std::mutex mutex;
    std::shared_ptr<const Cursor> cursor;
    std::uint64_t version = 0;
    std::vector<std::shared_ptr<Signal>> waiters;
};

CursorWatch::CursorWatch() : impl_(std::make_unique<Impl>()) {}
CursorWatch::~CursorWatch() = default;

std::shared_ptr<const Cursor> CursorWatch::current() const {
    std::lock_guard lock(impl_->mutex);
    return impl_->cursor;
}

std::uint64_t CursorWatch::version() const {
    std::lock_guard lock(impl_->mutex);
    return impl_->version;
}

asio::awaitable<void> CursorWatch::changed(std::uint64_t seen) {
    std::shared_ptr<Impl::Signal> signal;
    {
        std::lock_guard lock(impl_->mutex);
        if (impl_->version > seen) co_return;
        signal = std::make_shared<Impl::Signal>(co_await asio::this_coro::executor, 1);
        impl_->waiters.push_back(signal);
    }
    co_await signal->async_receive(asio::use_awaitable);
}

void CursorWatch::replace(std::shared_ptr<const Cursor> cursor) {
    std::vector<std::shared_ptr<Impl::Signal>> waiters;
    {
        std::lock_guard lock(impl_->mutex);
        impl_->cursor = std::move(cursor);
        ++impl_->version;
        waiters.swap(impl_->waiters);
    }
    for (const auto& w : waiters) w->try_send(asio::error_code{});
}

// ---- RfbSurface ----

// The local framebuffer copy and the frame numbering.
struct Picture {
    RgbaImage fb;
    std::string desktop;
    // `Frame::input_seq` of the last published frame.
    std::uint64_t input_seq = 0;
};

// Everything but the picture and the cursor runs on `strand`.
struct RfbSurface::Shared : std::enable_shared_from_this<RfbSurface::Shared> {
    using Strand = asio::strand<asio::any_io_executor>;

    Shared(const asio::any_io_executor& executor, cs::SurfaceUri uri, RfbEndpoint endpoint,
           std::shared_ptr<PasswordSource> passwords, RfbOptions options)
        : uri(std::move(uri)), endpoint(std::move(endpoint)), passwords(std::move(passwords)), options(options),
          strand(asio::make_strand(executor)), feed(executor, std::max<std::size_t>(options.frame_capacity, 1)),
          write_lock(strand, 1), backoff(strand) {}

    cs::SurfaceUri uri;
    RfbEndpoint endpoint;
    std::shared_ptr<PasswordSource> passwords;
    RfbOptions options;
    Strand strand;
    mutable std::mutex picture_mutex;
    Picture picture;
    FrameFeed feed;
    std::shared_ptr<CursorWatch> cursor = std::make_shared<CursorWatch>();

    // The link: the connection's socket and the input state it carries.
    std::shared_ptr<tcp::socket> socket;
    // The socket of a connect in progress, closed by `stop`.
    std::shared_ptr<tcp::socket> pending;
    InputState input;
    // `InputEvent::seq` of the last input written to the socket.
    std::uint64_t last_input_seq = 0;
    std::vector<std::uint8_t> buf;
    asio::experimental::channel<void(asio::error_code)> write_lock;
    // `last_input_seq` when the latest FramebufferUpdateRequest was written.
    std::atomic<std::uint64_t> requested_input_seq{0};
    asio::steady_timer backoff;
    bool stopped = false;

    std::pair<std::uint32_t, std::uint32_t> size() const {
        std::lock_guard lock(picture_mutex);
        return {picture.fb.width, picture.fb.height};
    }

    void stop() {
        stopped = true;
        if (socket) socket->close();
        if (pending) pending->close();
        backoff.cancel();
    }

    // Connects and runs the handshake with the connect timeout; returns the
    // reader positioned after ServerInit.
    asio::awaitable<RfbResult<std::unique_ptr<Reader>>> establish() {
        using namespace asio::experimental::awaitable_operators;
        auto sock = std::make_shared<tcp::socket>(strand);
        pending = sock;
        auto reader = std::make_unique<Reader>(
            [sock](std::span<std::uint8_t> out) -> asio::awaitable<RfbResult<std::size_t>> {
                auto [ec, n] = co_await sock->async_read_some(asio::buffer(out), asio::as_tuple(asio::use_awaitable));
                if (ec == asio::error::eof) co_return std::size_t{0};
                if (ec) co_return std::unexpected(io_error(ec));
                co_return n;
            },
            READ_BUFFER);
        asio::steady_timer timer(strand, options.connect_timeout);
        auto outcome = co_await (attempt(sock, *reader) || timer.async_wait(asio::as_tuple(asio::use_awaitable)));
        if (outcome.index() == 1) {
            sock->close();
            co_return std::unexpected(RfbError::timeout(endpoint.address));
        }
        auto init = std::get<0>(std::move(outcome));
        if (!init) {
            sock->close();
            co_return std::unexpected(std::move(init.error()));
        }
        {
            std::lock_guard lock(picture_mutex);
            const std::uint32_t width = init->width;
            const std::uint32_t height = init->height;
            if (picture.fb.width != width || picture.fb.height != height) {
                picture.fb = RgbaImage::filled(width, height, {0, 0, 0, 255});
            }
            picture.desktop = std::move(init->name);
        }
        pending.reset();
        if (stopped) {
            sock->close();
            co_return std::unexpected(RfbError::disconnected());
        }
        socket = sock;
        input.reset();
        requested_input_seq.store(last_input_seq, std::memory_order_release);
        co_return std::move(reader);
    }

    asio::awaitable<RfbResult<ServerInit>> attempt(std::shared_ptr<tcp::socket> sock, Reader& reader) {
        try {
            const std::string& address = endpoint.address;
            const auto colon = address.rfind(':');
            if (colon == std::string::npos) fail(RfbError::io("invalid socket address"));
            std::string host = address.substr(0, colon);
            if (host.starts_with('[') && host.ends_with(']')) host = host.substr(1, host.size() - 2);
            tcp::resolver resolver(strand);
            auto [rec, endpoints] = co_await resolver.async_resolve(host, address.substr(colon + 1),
                                                                    asio::as_tuple(asio::use_awaitable));
            if (rec) fail(io_error(rec));
            auto [cec, connected] = co_await asio::async_connect(*sock, endpoints, asio::as_tuple(asio::use_awaitable));
            if (cec) fail(io_error(cec));
            sock->set_option(tcp::no_delay(true));
            const detail::WriteFn write = [sock](std::span<const std::uint8_t> bytes) -> asio::awaitable<void> {
                auto [ec, n] = co_await asio::async_write(*sock, asio::buffer(bytes), asio::as_tuple(asio::use_awaitable));
                if (ec) fail(io_error(ec));
            };
            co_return co_await detail::handshake(reader, write, endpoint.name, *passwords, options.shared);
        } catch (detail::Failure& f) {
            co_return std::unexpected(std::move(f.error));
        } catch (const std::exception& e) {
            co_return std::unexpected(RfbError::io(e.what()));
        }
    }

    // Reads until the connection fails, then reconnects with backoff, until stopped.
    asio::awaitable<void> supervise(std::unique_ptr<Reader> reader) {
        for (;;) {
            Decoders decoders;
            const RfbError error = co_await read_messages(*reader, decoders);
            loa::runtime::log::debug(LOG_TARGET, std::format("RFB connection lost: endpoint={} error={}", endpoint.name,
                                                             error.message()));
            if (socket) socket->close();
            socket.reset();
            if (stopped) co_return;
            auto delay = options.backoff_initial;
            for (;;) {
                backoff.expires_after(delay);
                co_await backoff.async_wait(asio::as_tuple(asio::use_awaitable));
                if (stopped) co_return;
                auto next = co_await establish();
                if (stopped) {
                    stop();
                    co_return;
                }
                if (next) {
                    reader = std::move(*next);
                    break;
                }
                loa::runtime::log::debug(LOG_TARGET, std::format("RFB reconnect failed: endpoint={} error={}",
                                                                 endpoint.name, next.error().message()));
                delay = std::min(delay * 2, options.backoff_max);
            }
        }
    }

    // Handles server messages until the connection fails.
    asio::awaitable<RfbError> read_messages(Reader& rd, Decoders& dec) {
        try {
            // The first update after connecting answers the full request the
            // handshake sent, so it is published as a keyframe.
            bool full = true;
            for (;;) {
                const std::uint8_t type = co_await rd.u8();
                switch (type) {
                case 0: {
                    co_await read_update(rd, dec, full);
                    auto written = co_await write(nullptr);
                    if (!written) fail(written.error());
                    break;
                }
                case 1: {
                    // SetColourMapEntries: unused with a true color format.
                    co_await rd.skip(3);
                    const std::uint16_t count = co_await rd.u16();
                    co_await rd.skip(std::uint64_t(count) * 6);
                    break;
                }
                case 2: break;
                case 3: {
                    // ServerCutText: the clipboard is not part of the surface.
                    co_await rd.skip(3);
                    const std::uint32_t len = co_await rd.u32();
                    co_await rd.skip(len);
                    break;
                }
                default: fail(RfbError::protocol(std::format("unknown server message type {}", type)));
                }
            }
        } catch (detail::Failure& f) {
            co_return std::move(f.error);
        } catch (const std::exception& e) {
            co_return RfbError::io(e.what());
        }
    }

    // Applies one FramebufferUpdate to the framebuffer and publishes it.
    asio::awaitable<void> read_update(Reader& rd, Decoders& dec, bool& full) {
        std::array<std::uint8_t, 3> head{};
        co_await rd.read_exact(head);
        const std::uint16_t count = std::uint16_t(head[1] << 8U | head[2]);
        const std::uint64_t input_seq = requested_input_seq.load(std::memory_order_acquire);
        auto [width, height] = size();
        std::optional<Rect> dirty;
        for (std::uint16_t n = 0; n < count; ++n) {
            const Rect r = co_await read_rect(rd);
            const std::int32_t encoding = co_await rd.i32();
            if (!dec.supports(encoding)) fail(RfbError::protocol(std::format("unrequested encoding {}", encoding)));
            const auto inside = [&](Rect q) { return region_fits(q, width, height); };
            if (!is_pseudo(encoding) && !inside(r)) {
                fail(RfbError::protocol(std::format("rectangle {} outside {}x{}", detail::rect_debug(r), width, height)));
            }
            auto update = co_await dec.decode(encoding, r, rd);
            if (!update) fail(update.error());
            switch (update->kind) {
            case Update::Kind::Resize: {
                width = r.width;
                height = r.height;
                std::lock_guard lock(picture_mutex);
                picture.fb = RgbaImage::filled(width, height, {0, 0, 0, 255});
                full = true;
                dirty.reset();
                continue;
            }
            case Update::Kind::Cursor:
                cursor->replace(std::make_shared<const Cursor>(std::move(update->cursor)));
                continue;
            case Update::Kind::Copy:
                if (!inside(Rect{update->x, update->y, r.width, r.height})) {
                    fail(RfbError::protocol(std::format("CopyRect source {},{} outside", update->x, update->y)));
                }
                {
                    std::lock_guard lock(picture_mutex);
                    picture.fb.copy_rect(update->x, update->y, r);
                }
                break;
            case Update::Kind::Blit: {
                std::lock_guard lock(picture_mutex);
                auto blit = picture.fb.blit(r, dec.scratch.data);
                if (!blit) fail(RfbError::protocol(blit.error().message()));
                break;
            }
            }
            if (r.width > 0 && r.height > 0) dirty = union_rect(dirty, r);
        }

        std::lock_guard lock(picture_mutex);
        picture.input_seq = input_seq;
        const std::optional<Rect> region = full ? std::optional<Rect>(picture.fb.full()) : dirty;
        if (region) send(*region);
        full = false;
    }

    // Publishes `region` of the framebuffer as the next frame. Holds `picture_mutex`.
    void send(Rect region) {
        const Rect whole = picture.fb.full();
        if (whole.width == 0 || whole.height == 0) return;
        cs::Frame frame;
        frame.captured_us = loa::runtime::time::now_us();
        frame.input_seq = picture.input_seq;
        frame.width = whole.width;
        frame.height = whole.height;
        frame.region = region;
        frame.encoding = cs::Encoding::Rgba;
        frame.keyframe = region == whole;
        frame.data = picture.fb.crop(region);
        feed.publish(std::move(frame));
    }

    // Writes `event` (if any) followed by an incremental update request for the
    // whole framebuffer, so the update that shows its effect carries its
    // sequence number.
    asio::awaitable<RfbResult<void>> write(const cs::InputEvent* event) {
        const auto [width, height] = size();
        co_await write_lock.async_send(asio::error_code{}, asio::as_tuple(asio::use_awaitable));
        struct Unlock {
            asio::experimental::channel<void(asio::error_code)>& lock;
            ~Unlock() { lock.try_receive([](asio::error_code) {}); }
        } unlock{write_lock};
        const auto sock = socket;
        if (!sock) co_return std::unexpected(RfbError::disconnected());
        buf.clear();
        if (event) {
            auto encoded = input.encode(event->input, width, height, options.scroll_step, buf);
            if (!encoded) co_return std::unexpected(std::move(encoded.error()));
            if (buf.empty()) co_return RfbResult<void>{};
        }
        // The framebuffer size came from u16 fields.
        Msg::update_request(true, std::uint16_t(width), std::uint16_t(height)).write(buf);
        auto [ec, n] = co_await asio::async_write(*sock, asio::buffer(buf), asio::as_tuple(asio::use_awaitable));
        if (ec) co_return std::unexpected(io_error(ec));
        if (event) last_input_seq = event->seq;
        requested_input_seq.store(last_input_seq, std::memory_order_release);
        co_return RfbResult<void>{};
    }
};

asio::awaitable<RfbResult<std::shared_ptr<RfbSurface>>>
RfbSurface::connect(cs::SurfaceUri uri, RfbEndpoint endpoint, std::shared_ptr<PasswordSource> passwords,
                    RfbOptions options) {
    const auto executor = co_await asio::this_coro::executor;
    auto shared = std::make_shared<Shared>(executor, std::move(uri), std::move(endpoint), std::move(passwords), options);
    auto reader = co_await asio::co_spawn(shared->strand, shared->establish(), asio::use_awaitable);
    if (!reader) co_return std::unexpected(std::move(reader.error()));
    asio::co_spawn(
        shared->strand,
        [shared, reader = std::move(*reader)]() mutable -> asio::awaitable<void> {
            co_await shared->supervise(std::move(reader));
        },
        asio::detached);
    co_return std::shared_ptr<RfbSurface>(new RfbSurface(std::move(shared)));
}

RfbSurface::RfbSurface(std::shared_ptr<Shared> shared) : shared_(std::move(shared)) {}

RfbSurface::~RfbSurface() {
    asio::post(shared_->strand, [shared = shared_] { shared->stop(); });
}

std::shared_ptr<CursorWatch> RfbSurface::cursor() const { return shared_->cursor; }

cs::SurfaceInfo RfbSurface::info() const {
    std::lock_guard lock(shared_->picture_mutex);
    const Picture& pic = shared_->picture;
    return cs::SurfaceInfo{shared_->uri, pic.desktop.empty() ? shared_->endpoint.name : pic.desktop, pic.fb.width,
                           pic.fb.height, cs::SurfaceMode::Pixel, std::nullopt};
}

cs::Result<std::shared_ptr<cs::FrameReceiver>> RfbSurface::frames() { return shared_->feed.subscribe(); }

asio::awaitable<cs::Result<void>> RfbSurface::request_keyframe() {
    std::lock_guard lock(shared_->picture_mutex);
    shared_->send(shared_->picture.fb.full());
    co_return cs::Result<void>{};
}

asio::awaitable<cs::Result<void>> RfbSurface::input(cs::InputEvent event) {
    auto shared = shared_;
    auto result = co_await asio::co_spawn(
        shared->strand,
        [shared, event = std::move(event)]() -> asio::awaitable<RfbResult<void>> { co_return co_await shared->write(&event); },
        asio::use_awaitable);
    if (!result) co_return std::unexpected(result.error().to_surface_error());
    co_return cs::Result<void>{};
}

asio::awaitable<cs::Result<cs::Document>> RfbSurface::document() {
    co_return std::unexpected(cs::SurfaceError::mode(shared_->uri, cs::SurfaceMode::Pixel));
}

asio::awaitable<cs::Result<cs::Bytes>> RfbSurface::screenshot() {
    RgbaImage fb;
    {
        std::lock_guard lock(shared_->picture_mutex);
        fb = shared_->picture.fb;
    }
    auto png = encode_png(fb.width, fb.height, fb.data);
    if (!png) co_return std::unexpected(cs::SurfaceError::backend(png.error().message()));
    co_return std::move(*png);
}

// ---- RfbServer ----

RfbServer::RfbServer(std::vector<RfbEndpoint> endpoints, std::shared_ptr<PasswordSource> passwords, RfbOptions options)
    : endpoints_(std::move(endpoints)), passwords_(std::move(passwords)), options_(options) {}

std::shared_ptr<RfbSurface> RfbServer::live(const std::string& name) {
    std::lock_guard lock(mutex_);
    const auto it = open_.find(name);
    return it == open_.end() ? nullptr : it->second.lock();
}

cs::Result<cs::SurfaceUri> RfbServer::add_endpoint(RfbEndpoint endpoint) {
    auto uri = cs::SurfaceUri::make(SERVER_NAME, endpoint.name);
    if (!uri) return uri;
    std::lock_guard lock(mutex_);
    const auto it = std::ranges::find(endpoints_, endpoint.name, &RfbEndpoint::name);
    if (it == endpoints_.end()) {
        endpoints_.push_back(std::move(endpoint));
    } else if (*it != endpoint) {
        open_.erase(endpoint.name);
        *it = std::move(endpoint);
    }
    return uri;
}

asio::awaitable<cs::Result<std::shared_ptr<RfbSurface>>> RfbServer::open_rfb(cs::SurfaceUri uri) {
    std::optional<RfbEndpoint> found;
    {
        std::lock_guard lock(mutex_);
        if (uri.server() == SERVER_NAME) {
            const auto it = std::ranges::find(endpoints_, uri.name(), &RfbEndpoint::name);
            if (it != endpoints_.end()) found = *it;
        }
    }
    if (!found) co_return std::unexpected(cs::SurfaceError::not_found(uri));
    if (auto surface = live(found->name)) co_return surface;
    const std::string name = found->name;
    auto surface = co_await RfbSurface::connect(uri, std::move(*found), passwords_, options_);
    if (!surface) co_return std::unexpected(surface.error().to_surface_error());
    std::lock_guard lock(mutex_);
    // A concurrent open may have connected first; keep its connection.
    if (const auto it = open_.find(name); it != open_.end()) {
        if (auto existing = it->second.lock()) co_return existing;
    }
    open_.insert_or_assign(name, *surface);
    co_return std::move(*surface);
}

asio::awaitable<cs::Result<std::vector<cs::SurfaceInfo>>> RfbServer::list() {
    std::vector<RfbEndpoint> endpoints;
    {
        std::lock_guard lock(mutex_);
        endpoints = endpoints_;
    }
    std::vector<cs::SurfaceInfo> out;
    for (const auto& e : endpoints) {
        if (auto surface = live(e.name)) {
            out.push_back(surface->info());
            continue;
        }
        auto uri = cs::SurfaceUri::make(SERVER_NAME, e.name);
        if (!uri) co_return std::unexpected(uri.error());
        out.push_back(cs::SurfaceInfo{std::move(*uri), e.name, 0, 0, cs::SurfaceMode::Pixel, std::nullopt});
    }
    co_return out;
}

asio::awaitable<cs::Result<std::shared_ptr<cs::Surface>>> RfbServer::open(const cs::SurfaceUri& uri) {
    auto surface = co_await open_rfb(uri);
    if (!surface) co_return std::unexpected(surface.error());
    co_return std::shared_ptr<cs::Surface>(std::move(*surface));
}

} // namespace loa::surfaces::vnc
