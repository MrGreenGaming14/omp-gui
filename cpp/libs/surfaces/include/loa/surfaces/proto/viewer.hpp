#pragma once

// Viewer side of `omp/surface/1` (port of `loa-surfaces/src/proto/viewer.rs`).

#include <cstdint>
#include <memory>
#include <optional>
#include <variant>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/surface.hpp"
#include "loa/surfaces/proto/error.hpp"
#include "loa/surfaces/proto/fragment.hpp"
#include "loa/surfaces/proto/transport.hpp"

namespace loa::surfaces::proto {

namespace detail {
struct InputState;
struct ViewerQueue;
} // namespace detail

struct ViewerOptions {
    // Frame encodings the viewer can decode; the server closes the session rather than send any other.
    std::vector<contracts::surface::Encoding> encodings;
    // Documents larger than this are refused before any byte is buffered.
    std::uint64_t max_document = 512ULL * 1024 * 1024;

    explicit ViewerOptions(std::vector<contracts::surface::Encoding> e) : encodings(std::move(e)) {}
    friend bool operator==(const ViewerOptions&, const ViewerOptions&) = default;
};

// The surface changed size; frames from now on have this size.
struct Resized {
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    friend bool operator==(const Resized&, const Resized&) = default;
};

// The next frame to show (older frames never follow it), or a size change.
using ViewerEvent = std::variant<contracts::surface::Frame, Resized>;

// Sends input events of one viewer, numbered from 1 in send order. Copies share
// the numbering; the session's stream ends when the viewer closes or the last
// copy (the viewer's own included) is gone.
class InputSender {
  public:
    explicit InputSender(std::shared_ptr<detail::InputState> state) : state_(std::move(state)) {}

    // Returns the seq frames echo in `Frame::input_seq`. Never blocks.
    Result<std::uint64_t> send(contracts::surface::Input input) const;
    // Queues one raw message for the server.
    Result<void> send_message(Body body) const;

  private:
    std::shared_ptr<detail::InputState> state_;
};

class PixelViewer {
  public:
    PixelViewer(contracts::surface::SurfaceInfo info, std::optional<contracts::surface::StreamInfo> stream,
                std::shared_ptr<detail::ViewerQueue> queue, InputSender input);
    PixelViewer(PixelViewer&&) noexcept;
    PixelViewer& operator=(PixelViewer&&) noexcept;
    ~PixelViewer();

    [[nodiscard]] const contracts::surface::SurfaceInfo& info() const noexcept { return info_; }
    [[nodiscard]] const std::optional<contracts::surface::StreamInfo>& stream() const noexcept { return stream_; }
    [[nodiscard]] InputSender input() const { return input_; }

    // Ends with `ProtoError::Kind::Closed` when the server closes.
    [[nodiscard]] Awaitable<Result<ViewerEvent>> next_event();
    // Ends the session, even if input senders are still held elsewhere.
    void close();

  private:
    contracts::surface::SurfaceInfo info_;
    std::optional<contracts::surface::StreamInfo> stream_;
    std::shared_ptr<detail::ViewerQueue> queue_;
    Reassembler reassembler_;
    InputSender input_;
};

class DocumentViewer {
  public:
    DocumentViewer(MessageReader reader, MessageWriter writer, std::uint64_t max)
        : reader_(std::move(reader)), writer_(std::move(writer)), max_(max) {}

    // Receives the file, checks its length and checksum, and closes the session.
    [[nodiscard]] Awaitable<Result<contracts::surface::Document>> receive();

  private:
    MessageReader reader_;
    MessageWriter writer_;
    std::uint64_t max_;
};

using Opened = std::variant<PixelViewer, DocumentViewer>;

// Opens `uri` on `transport`: a pixel viewer or a document viewer by the surface's mode.
[[nodiscard]] Awaitable<Result<Opened>> open(Transport transport, const contracts::surface::SurfaceUri& uri,
                                             ViewerOptions options);

} // namespace loa::surfaces::proto
