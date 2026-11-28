#pragma once

// Test doubles and helpers for surface code (Rust feature `testkit`). Port of
// the generic part of `loa-surfaces/src/testkit.rs`; the RFB server lives with
// the VNC surface. Test-only: includes Asio.

#include <asio/any_io_executor.hpp>
#include <asio/awaitable.hpp>
#include <asio/use_awaitable.hpp>
#include <asio/experimental/channel.hpp>

#include <atomic>
#include <chrono>
#include <cstdint>
#include <deque>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/surface.hpp"
#include "loa/contracts/surface_channel.hpp"
#include "loa/surfaces/pixels.hpp"

namespace loa::surfaces::testkit {

namespace cs = loa::contracts::surface;

// Upper bound for anything a test waits on.
inline constexpr std::chrono::seconds PATIENCE{20};

// An unbounded single-consumer queue (tokio's `mpsc::unbounded_channel`).
template <typename T>
class Inbox {
  public:
    explicit Inbox(const asio::any_io_executor& executor) : signal_(executor, 1) {}

    void send(T value) {
        {
            std::lock_guard lock(mutex_);
            queue_.push_back(std::move(value));
        }
        signal_.try_send(asio::error_code{});
    }

    [[nodiscard]] std::optional<T> try_recv() {
        std::lock_guard lock(mutex_);
        if (queue_.empty()) return std::nullopt;
        T value = std::move(queue_.front());
        queue_.pop_front();
        return value;
    }

    // The next value; waits for one to arrive.
    asio::awaitable<T> recv() {
        for (;;) {
            if (auto value = try_recv()) co_return std::move(*value);
            co_await signal_.async_receive(asio::use_awaitable);
        }
    }

  private:
    std::mutex mutex_;
    std::deque<T> queue_;
    asio::experimental::channel<void(asio::error_code)> signal_;
};

// The test's end of a `FakeSurface`.
struct Probe {
    std::shared_ptr<cs::FrameSender> frames;
    std::shared_ptr<Inbox<cs::InputEvent>> inputs;
    std::shared_ptr<Inbox<bool>> keyframes;
};

// A scripted surface: the test publishes frames through its `Probe` and reads
// back the inputs and keyframe requests the code under test sent.
class FakeSurface : public cs::Surface {
  public:
    // A surface and its probe; frames carry a backlog of 64 like the Rust double.
    [[nodiscard]] static std::pair<std::shared_ptr<FakeSurface>, Probe>
    make(const asio::any_io_executor& executor, cs::SurfaceInfo info, std::optional<cs::Document> document);

    cs::SurfaceInfo info_;
    std::optional<cs::Document> document_;

    [[nodiscard]] std::size_t keyframes_requested() const { return keyframe_count_.load(); }

    [[nodiscard]] cs::SurfaceInfo info() const override { return info_; }
    [[nodiscard]] cs::Result<std::shared_ptr<cs::FrameReceiver>> frames() override;
    [[nodiscard]] loa::contracts::Awaitable<cs::Result<void>> request_keyframe() override;
    [[nodiscard]] loa::contracts::Awaitable<cs::Result<void>> input(cs::InputEvent event) override;
    [[nodiscard]] loa::contracts::Awaitable<cs::Result<cs::Document>> document() override;
    [[nodiscard]] loa::contracts::Awaitable<cs::Result<cs::Bytes>> screenshot() override;

  private:
    FakeSurface(cs::SurfaceInfo info, std::optional<cs::Document> document, Probe probe);
    [[nodiscard]] cs::SurfaceError mode_error() const;

    std::shared_ptr<cs::FrameSender> frames_;
    std::shared_ptr<Inbox<cs::InputEvent>> inputs_;
    std::shared_ptr<Inbox<bool>> keyframes_;
    std::atomic<std::size_t> keyframe_count_{0};
};

// Next frame within `PATIENCE`; lag is skipped. Throws `std::runtime_error`
// when no frame arrives in time or the stream closes (Rust panics).
[[nodiscard]] asio::awaitable<std::shared_ptr<const cs::Frame>> next_frame(cs::FrameReceiver& frames);

// `width` x `height` RGBA pixels of one colour.
[[nodiscard]] std::vector<std::uint8_t> solid(std::uint32_t width, std::uint32_t height, Pixel rgba);

// `data:` URL of an HTML document.
[[nodiscard]] std::string html_data_url(std::string_view body);

} // namespace loa::surfaces::testkit
