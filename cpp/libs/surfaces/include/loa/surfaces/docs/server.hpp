#pragma once

// The `docs` surface server: opens files as document mode surfaces. Port of
// `loa-surfaces/src/docs/server.rs`.

#include <atomic>
#include <filesystem>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>

#include "loa/contracts/surface.hpp"
#include "loa/surfaces/docs/format.hpp"

namespace loa::surfaces::docs {

// Server part of the `surface://docs/<id>` uris.
inline constexpr std::string_view SERVER_NAME = "docs";

namespace cs = loa::contracts::surface;
using loa::contracts::Awaitable;

// One open document. Shared by the server and its users.
class DocSurface final : public cs::Surface, public std::enable_shared_from_this<DocSurface> {
  public:
    DocSurface(cs::SurfaceUri uri, std::string name, std::filesystem::path path, Format format, Fonts fonts,
               cs::Bytes bytes, std::unique_ptr<Paged> parsed);

    [[nodiscard]] const cs::SurfaceUri& uri() const noexcept { return uri_; }
    [[nodiscard]] Format format() const noexcept { return format_; }
    [[nodiscard]] const Paged& parsed() const noexcept { return *parsed_; }

    // Answers a structured data query as JSON, off the executor. A query the
    // format doesn't offer is a `DocsError::Request`.
    [[nodiscard]] Awaitable<Result<json::Value>> data(DataQuery query);
    // `data` without leaving the calling thread.
    [[nodiscard]] Result<json::Value> answer(const DataQuery& query) const;

    [[nodiscard]] cs::SurfaceInfo info() const override;
    [[nodiscard]] cs::Result<std::shared_ptr<cs::FrameReceiver>> frames() override;
    [[nodiscard]] Awaitable<cs::Result<void>> request_keyframe() override;
    [[nodiscard]] Awaitable<cs::Result<void>> input(cs::InputEvent event) override;
    [[nodiscard]] Awaitable<cs::Result<cs::Document>> document() override;
    // The first page as PNG. Images are returned at their own size, a PNG file as it is.
    [[nodiscard]] Awaitable<cs::Result<cs::Bytes>> screenshot() override;

  private:
    [[nodiscard]] cs::SurfaceError mode_error() const;

    cs::SurfaceUri uri_;
    std::string name_;
    std::filesystem::path path_;
    Format format_;
    Fonts fonts_;
    cs::Bytes bytes_;
    std::unique_ptr<Paged> parsed_;
};

// The `docs` surface server. Files are opened explicitly with `open_file`.
class DocsServer {
  public:
    // `fonts` draws text in XLSX, DOCX and PPTX renderings; pass `Fonts::system()` in the app.
    explicit DocsServer(Fonts fonts);

    // Opens the file at `path` as `surface://docs/<id>`. The format comes from
    // the extension, else from the file's magic bytes. The file is parsed now,
    // so a broken file fails here. Work runs off the executor.
    [[nodiscard]] Awaitable<Result<std::shared_ptr<DocSurface>>> open_file(std::filesystem::path path);
    // `open_file` without leaving the calling thread.
    [[nodiscard]] Result<std::shared_ptr<DocSurface>> open_file_blocking(const std::filesystem::path& path);

    void close(const cs::SurfaceUri& uri);
    // The open surface at `uri`, typed, for data queries.
    [[nodiscard]] Result<std::shared_ptr<DocSurface>> surface(const cs::SurfaceUri& uri) const;

  private:
    Fonts fonts_;
    std::atomic<std::uint64_t> next_id_{1};
    mutable std::mutex mutex_;
    std::map<cs::SurfaceUri, std::shared_ptr<DocSurface>> surfaces_;
};

} // namespace loa::surfaces::docs
