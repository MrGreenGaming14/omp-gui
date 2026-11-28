#pragma once

// Fakes at the renderer's port boundary: a scripted surface, a still decoder and a document
// renderer, so the UI is tested without the app's media code. Port of
// `crates/loa-ui-surface/tests/support/mod.rs`.

#include <array>
#include <atomic>
#include <cstdint>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <vector>

#include "loa/contracts/surface.hpp"
#include "loa/ui/surface/tab.hpp"

namespace loa::contracts::surface {
class FrameSender;
}

namespace support {

namespace vs = loa::view::surface;

// A scripted surface: inputs and keyframe requests are recorded; frames come from `sender` when
// a test sets one.
class FakeSurface final : public vs::Surface {
  public:
    FakeSurface(vs::SurfaceInfo info, std::optional<vs::Document> document) : info_(std::move(info)), document_(std::move(document)) {}

    [[nodiscard]] vs::SurfaceInfo info() const override { return info_; }
    [[nodiscard]] loa::contracts::surface::Result<std::shared_ptr<loa::contracts::surface::FrameReceiver>> frames() override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<void>> request_keyframe() override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<void>> input(vs::InputEvent event) override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<vs::Document>> document() override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<loa::contracts::surface::Bytes>> screenshot() override;

    [[nodiscard]] std::size_t keyframes_requested() const { return keyframes_; }
    [[nodiscard]] std::vector<vs::InputEvent> inputs() const {
        std::scoped_lock g(lock_);
        return inputs_;
    }

    vs::SurfaceInfo info_;
    std::optional<vs::Document> document_;
    std::shared_ptr<loa::contracts::surface::FrameSender> sender;

  private:
    std::atomic<std::size_t> keyframes_{0};
    mutable std::mutex lock_;
    std::vector<vs::InputEvent> inputs_;
};

// `width` x `height` RGBA pixels of one colour.
[[nodiscard]] std::vector<std::uint8_t> solid(std::uint32_t width, std::uint32_t height, std::array<std::uint8_t, 4> rgba);

// Stills whose PNG and JPEG "codecs" carry the region's RGBA as is.
class FakeStill final : public vs::port::FrameDecoder {
  public:
    [[nodiscard]] std::span<const vs::Encoding> accepts() const override;
    [[nodiscard]] std::expected<bool, vs::port::DecodeError> apply(const vs::Frame& frame, std::span<std::uint8_t> picture,
                                                                   std::uint32_t width, std::uint32_t height) override;
};

// MIME type `FakeDocuments` opens as one 300x200 page.
inline constexpr std::string_view PAGED_MIME = "application/x-fake-pages";

// Opens `image/*` as a 30x20 image and `PAGED_MIME` as one page; nothing else.
class FakeDocuments final : public vs::port::DocumentRenderer {
  public:
    [[nodiscard]] std::vector<std::string> extensions() const override;
    [[nodiscard]] std::expected<vs::port::OpenedDocument, std::string> open(const vs::Document& document) const override;
};

// Options over the fakes, with no extra decoder.
[[nodiscard]] loa::ui::surface::Options options();

// A surface uri that must parse.
[[nodiscard]] vs::SurfaceUri uri(std::string_view server, std::string_view name);

} // namespace support
