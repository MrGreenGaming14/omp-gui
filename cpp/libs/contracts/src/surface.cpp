#include "loa/contracts/surface.hpp"

#include <asio.hpp>
#include <asio/experimental/channel.hpp>

#include <algorithm>
#include <format>

#include "loa/contracts/surface_channel.hpp"

namespace loa::contracts::surface {

SurfaceError SurfaceError::uri(std::string text) { return {Kind::Uri, std::move(text)}; }
SurfaceError SurfaceError::not_found(const SurfaceUri& uri) { return {Kind::NotFound, uri.as_str()}; }
SurfaceError SurfaceError::mode(const SurfaceUri& uri, SurfaceMode mode) { return {Kind::Mode, uri.as_str(), mode}; }
SurfaceError SurfaceError::closed() { return {Kind::Closed, {}}; }
SurfaceError SurfaceError::backend(std::string message) { return {Kind::Backend, std::move(message)}; }

std::string SurfaceError::message() const {
    switch (kind_) {
    case Kind::Uri: return std::format("`{}` isn't a surface://<server>/<name> uri", detail_);
    case Kind::NotFound: return std::format("no surface {}", detail_);
    case Kind::Mode: return std::format("{} is a {} surface", detail_, loa::core::as_str(*mode_));
    case Kind::Closed: return "the surface is closed";
    case Kind::Backend: return detail_;
    }
    return detail_;
}

loa::Error SurfaceError::to_error() const {
    switch (kind_) {
    case Kind::Uri: return {ErrorCode::parse, message()};
    case Kind::NotFound: return {ErrorCode::not_found, message()};
    case Kind::Mode: return {ErrorCode::unsupported, message()};
    case Kind::Closed: return {ErrorCode::cancelled, message()};
    case Kind::Backend: break;
    }
    return {ErrorCode::unknown, message()};
}

Result<SurfaceUri> SurfaceUri::parse(std::string_view text) {
    std::string_view rest = text;
    if (rest.starts_with(SCHEME) && rest.substr(SCHEME.size()).starts_with("://")) {
        rest.remove_prefix(SCHEME.size() + 3);
        const auto slash = rest.find('/');
        if (slash != std::string_view::npos && slash != 0 && slash + 1 < rest.size())
            return SurfaceUri(std::string(text));
    }
    return std::unexpected(SurfaceError::uri(std::string(text)));
}

Result<SurfaceUri> SurfaceUri::make(std::string_view server, std::string_view name) {
    return parse(std::format("{}://{}/{}", SCHEME, server, name));
}

std::string_view SurfaceUri::server() const noexcept {
    const std::string_view rest = std::string_view(text_).substr(SCHEME.size() + 3);
    return rest.substr(0, rest.find('/'));
}

std::string_view SurfaceUri::name() const noexcept {
    const std::string_view rest = std::string_view(text_).substr(SCHEME.size() + 3);
    return rest.substr(rest.find('/') + 1);
}

Awaitable<Result<std::optional<StreamInfo>>> Surface::negotiate(std::vector<Encoding>) {
    co_return std::optional<StreamInfo>{};
}

FrameReceiver::FrameReceiver(const asio::any_io_executor& executor, std::size_t capacity)
    : capacity_(std::max<std::size_t>(capacity, 1)), signal_(executor, 1) {}

Awaitable<Result<std::shared_ptr<const Frame>>> FrameReceiver::recv() {
    for (;;) {
        {
            std::lock_guard lock(mutex_);
            if (!queue_.empty()) {
                auto frame = std::move(queue_.front());
                queue_.pop_front();
                co_return frame;
            }
            if (closed_) co_return std::unexpected(SurfaceError::closed());
        }
        auto [ec] = co_await signal_.async_receive(asio::as_tuple(asio::use_awaitable));
        (void)ec;
    }
}

std::uint64_t FrameReceiver::take_lagged() {
    std::lock_guard lock(mutex_);
    return std::exchange(lagged_, 0);
}

void FrameReceiver::push(std::shared_ptr<const Frame> frame) {
    {
        std::lock_guard lock(mutex_);
        if (queue_.size() == capacity_) {
            queue_.pop_front();
            ++lagged_;
        }
        queue_.push_back(std::move(frame));
    }
    signal_.try_send(asio::error_code{});
}

void FrameReceiver::close() {
    {
        std::lock_guard lock(mutex_);
        closed_ = true;
    }
    signal_.try_send(asio::error_code{});
}

FrameSender::FrameSender(asio::any_io_executor executor, std::size_t capacity)
    : executor_(std::move(executor)), capacity_(capacity) {}

FrameSender::~FrameSender() {
    for (auto& weak : receivers_)
        if (auto receiver = weak.lock()) receiver->close();
}

std::shared_ptr<FrameReceiver> FrameSender::subscribe() {
    auto receiver = std::make_shared<FrameReceiver>(executor_, capacity_);
    std::lock_guard lock(mutex_);
    receivers_.push_back(receiver);
    return receiver;
}

std::size_t FrameSender::send(std::shared_ptr<const Frame> frame) {
    std::lock_guard lock(mutex_);
    std::erase_if(receivers_, [](const auto& weak) { return weak.expired(); });
    for (auto& weak : receivers_)
        if (auto receiver = weak.lock()) receiver->push(frame);
    return receivers_.size();
}

std::size_t FrameSender::receiver_count() {
    std::lock_guard lock(mutex_);
    return static_cast<std::size_t>(std::ranges::count_if(receivers_, [](const auto& weak) { return !weak.expired(); }));
}

} // namespace loa::contracts::surface
