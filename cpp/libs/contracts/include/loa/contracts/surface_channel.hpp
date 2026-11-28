#pragma once

// The frame broadcast of a pixel surface (Rust `tokio::sync::broadcast` of
// `Arc<Frame>`) on Asio channels. Includes Asio: only backends and viewers.

#include <asio/experimental/channel.hpp>

#include <cstddef>
#include <cstdint>
#include <deque>
#include <memory>
#include <mutex>
#include <vector>

#include "loa/contracts/surface.hpp"

namespace loa::contracts::surface {

// One subscriber's bounded queue. When it is full the oldest frame is dropped
// and counted, as a lagging tokio receiver would see `Lagged(n)`.
class FrameReceiver {
  public:
    FrameReceiver(const asio::any_io_executor& executor, std::size_t capacity);

    // The next frame; `Closed` once the sender is gone and the queue is empty.
    [[nodiscard]] Awaitable<Result<std::shared_ptr<const Frame>>> recv();
    // Frames dropped since the last call (tokio's `Lagged(n)`).
    [[nodiscard]] std::uint64_t take_lagged();

  private:
    friend class FrameSender;
    void push(std::shared_ptr<const Frame> frame);
    void close();

    std::mutex mutex_;
    std::deque<std::shared_ptr<const Frame>> queue_;
    std::size_t capacity_;
    std::uint64_t lagged_ = 0;
    bool closed_ = false;
    asio::experimental::channel<void(asio::error_code)> signal_;
};

// The sending side: every frame goes to every live receiver.
class FrameSender {
  public:
    FrameSender(asio::any_io_executor executor, std::size_t capacity);
    ~FrameSender();
    FrameSender(const FrameSender&) = delete;
    FrameSender& operator=(const FrameSender&) = delete;

    [[nodiscard]] std::shared_ptr<FrameReceiver> subscribe();
    // Number of receivers still alive that got it.
    std::size_t send(std::shared_ptr<const Frame> frame);
    // Receivers still alive (tokio's `receiver_count`).
    [[nodiscard]] std::size_t receiver_count();

  private:
    asio::any_io_executor executor_;
    std::size_t capacity_;
    std::mutex mutex_;
    std::vector<std::weak_ptr<FrameReceiver>> receivers_;
};

} // namespace loa::contracts::surface
