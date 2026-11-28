#include "loa/surfaces/testkit.hpp"

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>

#include <stdexcept>

#include "loa/runtime/text.hpp"

namespace loa::surfaces::testkit {

std::pair<std::shared_ptr<FakeSurface>, Probe>
FakeSurface::make(const asio::any_io_executor& executor, cs::SurfaceInfo info, std::optional<cs::Document> document) {
    Probe probe{std::make_shared<cs::FrameSender>(executor, 64), std::make_shared<Inbox<cs::InputEvent>>(executor),
                std::make_shared<Inbox<bool>>(executor)};
    std::shared_ptr<FakeSurface> surface(new FakeSurface(std::move(info), std::move(document), probe));
    return {std::move(surface), std::move(probe)};
}

FakeSurface::FakeSurface(cs::SurfaceInfo info, std::optional<cs::Document> document, Probe probe)
    : info_(std::move(info)), document_(std::move(document)), frames_(std::move(probe.frames)),
      inputs_(std::move(probe.inputs)), keyframes_(std::move(probe.keyframes)) {}

cs::SurfaceError FakeSurface::mode_error() const { return cs::SurfaceError::mode(info_.uri, info_.mode); }

cs::Result<std::shared_ptr<cs::FrameReceiver>> FakeSurface::frames() {
    if (info_.mode != cs::SurfaceMode::Pixel) return std::unexpected(mode_error());
    return frames_->subscribe();
}

loa::contracts::Awaitable<cs::Result<void>> FakeSurface::request_keyframe() {
    keyframe_count_.fetch_add(1);
    keyframes_->send(true);
    co_return cs::Result<void>{};
}

loa::contracts::Awaitable<cs::Result<void>> FakeSurface::input(cs::InputEvent event) {
    inputs_->send(std::move(event));
    co_return cs::Result<void>{};
}

loa::contracts::Awaitable<cs::Result<cs::Document>> FakeSurface::document() {
    if (!document_) co_return std::unexpected(mode_error());
    co_return *document_;
}

loa::contracts::Awaitable<cs::Result<cs::Bytes>> FakeSurface::screenshot() { co_return cs::Bytes{}; }

asio::awaitable<std::shared_ptr<const cs::Frame>> next_frame(cs::FrameReceiver& frames) {
    using namespace asio::experimental::awaitable_operators;
    asio::steady_timer timer(co_await asio::this_coro::executor, PATIENCE);
    auto result = co_await (frames.recv() || timer.async_wait(asio::use_awaitable));
    if (result.index() == 1) throw std::runtime_error("no frame in time");
    auto frame = std::get<0>(std::move(result));
    if (!frame) throw std::runtime_error("frame stream closed");
    co_return *frame;
}

std::vector<std::uint8_t> solid(std::uint32_t width, std::uint32_t height, Pixel rgba) {
    return RgbaImage::filled(width, height, rgba).data;
}

std::string html_data_url(std::string_view body) { return "data:text/html," + loa::runtime::text::percent_encode(body); }

} // namespace loa::surfaces::testkit
