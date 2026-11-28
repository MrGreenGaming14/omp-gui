// The `surface` smoke screen: the Surfaces tab with a pixel tab showing a streamed frame beside
// a second panel on its launcher. The surface and decoder are local samples; the app's media
// decoders and document renderers are wired by the services layer.

#include <array>
#include <memory>

#include <asio.hpp>

#include <QApplication>
#include <QHBoxLayout>
#include <QWidget>

#include "loa/contracts/surface.hpp"
#include "loa/ui/app/smoke.hpp"
#include "loa/ui/surface/view.hpp"

namespace loa::ui::app {

namespace {

namespace vs = loa::view::surface;
namespace cs = loa::contracts::surface;

// A still surface: no stream, every call goes through.
class SampleSurface final : public vs::Surface {
  public:
    explicit SampleSurface(vs::SurfaceInfo info) : info_(std::move(info)) {}
    [[nodiscard]] vs::SurfaceInfo info() const override { return info_; }
    [[nodiscard]] cs::Result<std::shared_ptr<cs::FrameReceiver>> frames() override { return std::shared_ptr<cs::FrameReceiver>{}; }
    [[nodiscard]] loa::contracts::Awaitable<cs::Result<void>> request_keyframe() override { co_return cs::Result<void>{}; }
    [[nodiscard]] loa::contracts::Awaitable<cs::Result<void>> input(vs::InputEvent) override { co_return cs::Result<void>{}; }
    [[nodiscard]] loa::contracts::Awaitable<cs::Result<vs::Document>> document() override {
        co_return std::unexpected(cs::SurfaceError::mode(info_.uri, info_.mode));
    }
    [[nodiscard]] loa::contracts::Awaitable<cs::Result<cs::Bytes>> screenshot() override { co_return cs::Bytes{}; }

  private:
    vs::SurfaceInfo info_;
};

// Raw RGBA regions.
class RgbaDecoder final : public vs::port::FrameDecoder {
  public:
    [[nodiscard]] std::span<const vs::Encoding> accepts() const override {
        static constexpr std::array E{vs::Encoding::Rgba};
        return E;
    }
    [[nodiscard]] std::expected<bool, vs::port::DecodeError> apply(const vs::Frame& frame, std::span<std::uint8_t> picture,
                                                                   std::uint32_t width, std::uint32_t height) override {
        const auto& r = frame.region;
        if (r.x + r.width > width || r.y + r.height > height || frame.data.size() != std::size_t{r.width} * r.height * 4)
            return std::unexpected(vs::port::DecodeError{"region doesn't fit"});
        for (std::uint32_t y = 0; y < r.height; ++y)
            std::copy_n(frame.data.begin() + std::ptrdiff_t(y) * r.width * 4, std::size_t{r.width} * 4,
                        picture.begin() + std::ptrdiff_t(((r.y + y) * width + r.x) * 4));
        return true;
    }
};

// Opens nothing: the smoke screen shows no documents.
class NoDocuments final : public vs::port::DocumentRenderer {
  public:
    [[nodiscard]] std::vector<std::string> extensions() const override { return {"pdf", "xlsx", "docx", "pptx"}; }
    [[nodiscard]] std::expected<vs::port::OpenedDocument, std::string> open(const vs::Document& d) const override {
        return std::unexpected("no viewer for " + d.mime);
    }
};

surface::Options options() {
    return surface::Options::make(vs::port::Decoders{std::nullopt, [] { return std::make_unique<RgbaDecoder>(); }},
                                  std::make_shared<NoDocuments>());
}

// The panel states outlive the views that render them.
struct Screen : QWidget {
    std::unique_ptr<surface::Panel> tabbed;
    std::unique_ptr<surface::Panel> launcher;
};

surface::PanelView* view_of(surface::Panel& panel) {
    auto* view = new surface::PanelView;
    QObject::connect(view, &surface::PanelView::message, view, [&panel, view](const surface::panel::Message& m) {
        (void)panel.update(m);
        view->render(panel);
    });
    view->render(panel);
    return view;
}

} // namespace

QWidget* surface_screen() {
    auto* screen = new Screen;
    screen->setAccessibleName(QStringLiteral("surface"));
    const auto dir = std::filesystem::current_path();
    screen->tabbed = std::make_unique<surface::Panel>(options(), vs::browser_status::Ready{"/usr/bin/chromium", true}, dir);
    screen->launcher = std::make_unique<surface::Panel>(options(), vs::browser_status::Missing{}, dir);

    auto uri = vs::SurfaceUri::make("vm", "desktop");
    if (!uri) return nullptr;
    vs::SurfaceInfo info{*uri, "Desktop", 64, 48, vs::SurfaceMode::Pixel, std::nullopt};
    (void)screen->tabbed->attach(std::make_shared<SampleSurface>(info), "vm");
    std::vector<std::uint8_t> pixels;
    pixels.reserve(64 * 48 * 4);
    for (std::uint32_t y = 0; y < 48; ++y)
        for (std::uint32_t x = 0; x < 64; ++x) pixels.insert(pixels.end(), {std::uint8_t(x * 4), std::uint8_t(y * 5), 160, 255});
    auto frame = std::make_shared<const vs::Frame>(vs::Frame{1, 0, 0, 64, 48, {0, 0, 64, 48}, vs::Encoding::Rgba, true, std::move(pixels)});
    (void)screen->tabbed->update(surface::panel::msg::Surface{*uri, surface::msg::Frame{frame}});
    screen->launcher->set_available({vs::AvailableSurface{info, "vm"}});
    screen->launcher->failed("no Chrome or Chromium is installed");

    auto* row = new QHBoxLayout(screen);
    row->addWidget(view_of(*screen->tabbed), 1);
    row->addWidget(view_of(*screen->launcher), 1);
    screen->resize(1000, 640);
    return screen;
}

std::string check_surface_screen(QWidget& screen) {
    const auto* viewer = screen.findChild<surface::Viewer*>();
    if (!viewer || !viewer->isVisible()) return "missing pixel viewer";
    bool launcher = false;
    for (const auto* w : screen.findChildren<QWidget*>())
        if (w->accessibleName() == QStringLiteral("surface launcher") && w->isVisible()) launcher = true;
    if (!launcher) return "missing launcher";
    if (!screen.findChild<surface::PickerView*>()) return "missing file picker";
    return {};
}

} // namespace loa::ui::app
