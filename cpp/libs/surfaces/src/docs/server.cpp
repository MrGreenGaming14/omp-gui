#include "loa/surfaces/docs/server.hpp"

#include <asio.hpp>

#include <format>
#include <fstream>
#include <iterator>
#include <system_error>

namespace loa::surfaces::docs {

namespace {

// Scale of `surface_screenshot` pages: keeps 10 pt text legible while a Letter page stays under 1200 px.
constexpr float SCREENSHOT_SCALE = 1.5F;

// Threads for parsing and rendering, as tokio's `spawn_blocking`.
asio::thread_pool& blocking_pool() {
    static asio::thread_pool pool(2);
    return pool;
}

// Runs `work` on the blocking pool and resumes on the caller's executor.
template <typename F>
auto run_blocking(F work) -> asio::awaitable<decltype(work())> {
    const auto home = co_await asio::this_coro::executor;
    co_await asio::post(asio::bind_executor(blocking_pool(), asio::use_awaitable));
    auto result = work();
    co_await asio::post(asio::bind_executor(home, asio::use_awaitable));
    co_return result;
}

Result<std::pair<Format, cs::Bytes>> read_file(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) {
        const std::error_code ec(errno, std::generic_category());
        return std::unexpected(DocsError::io(path, std::format("{} (os error {})", ec.message(), ec.value())));
    }
    cs::Bytes bytes((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    if (in.bad()) return std::unexpected(DocsError::io(path, "read failed"));
    const auto format = detect(path, bytes);
    if (!format) return std::unexpected(DocsError::unsupported(path.string()));
    return std::pair{*format, std::move(bytes)};
}

} // namespace

// --- DocSurface ---

DocSurface::DocSurface(cs::SurfaceUri uri, std::string name, std::filesystem::path path, Format format, Fonts fonts,
                       cs::Bytes bytes, std::unique_ptr<Paged> parsed)
    : uri_(std::move(uri)), name_(std::move(name)), path_(std::move(path)), format_(format), fonts_(std::move(fonts)),
      bytes_(std::move(bytes)), parsed_(std::move(parsed)) {}

Result<json::Value> DocSurface::answer(const DataQuery& query) const {
    if (std::holds_alternative<query::Info>(query.v)) {
        json::Array pages;
        for (std::size_t i = 0; i < parsed_->page_count(); ++i) {
            auto size = parsed_->page_size(i);
            if (!size) return std::unexpected(std::move(size.error()));
            pages.push_back(size->to_json());
        }
        // `DocInfo` field order and names.
        return json::Value(json::Object{
            {"uri", uri_.as_str()},
            {"name", name_},
            {"path", path_.string()},
            {"format", serde_name(format_)},
            {"mime", mime(format_)},
            {"pages", std::move(pages)},
        });
    }
    if (auto answer = parsed_->query(query)) return std::move(*answer);
    return std::unexpected(DocsError::request(std::format("{} files have no {}", to_string(format_), query.what())));
}

Awaitable<Result<json::Value>> DocSurface::data(DataQuery query) {
    auto self = shared_from_this();
    co_return co_await run_blocking([self, query = std::move(query)] { return self->answer(query); });
}

cs::SurfaceError DocSurface::mode_error() const { return cs::SurfaceError::mode(uri_, cs::SurfaceMode::Document); }

cs::SurfaceInfo DocSurface::info() const {
    std::pair<std::uint32_t, std::uint32_t> px{1, 1};
    if (auto size = parsed_->page_size(0)) px = size->pixels(1.0F);
    return cs::SurfaceInfo{uri_, name_, px.first, px.second, cs::SurfaceMode::Document, std::string(mime(format_))};
}

cs::Result<std::shared_ptr<cs::FrameReceiver>> DocSurface::frames() { return std::unexpected(mode_error()); }

Awaitable<cs::Result<void>> DocSurface::request_keyframe() { co_return std::unexpected(mode_error()); }

Awaitable<cs::Result<void>> DocSurface::input(cs::InputEvent) { co_return std::unexpected(mode_error()); }

Awaitable<cs::Result<cs::Document>> DocSurface::document() {
    co_return cs::Document{name_, std::string(mime(format_)), bytes_};
}

Awaitable<cs::Result<cs::Bytes>> DocSurface::screenshot() {
    if (format_ == Format::Png) co_return bytes_;
    auto self = shared_from_this();
    const float scale = is_image(format_) ? 1.0F : SCREENSHOT_SCALE;
    auto png = co_await run_blocking([self, scale]() -> Result<cs::Bytes> {
        auto page = self->parsed_->render_page(0, scale, self->fonts_);
        if (!page) return std::unexpected(std::move(page.error()));
        auto png = page->to_png();
        if (!png) return std::unexpected(DocsError::pixels(std::move(png.error())));
        return std::move(*png);
    });
    if (!png) co_return std::unexpected(png.error().to_surface_error());
    co_return std::move(*png);
}

// --- DocsServer ---

DocsServer::DocsServer(Fonts fonts) : fonts_(std::move(fonts)) {}

Result<std::shared_ptr<DocSurface>> DocsServer::open_file_blocking(const std::filesystem::path& path) {
    auto file = read_file(path);
    if (!file) return std::unexpected(std::move(file.error()));
    auto& [format, bytes] = *file;
    auto parsed = open_pages(format, bytes);
    if (!parsed) return std::unexpected(std::move(parsed.error()));
    const auto id = next_id_.fetch_add(1, std::memory_order_relaxed);
    auto uri = cs::SurfaceUri::make(SERVER_NAME, std::to_string(id));
    if (!uri) return std::unexpected(DocsError::surface(std::move(uri.error())));
    // Rust `Path::file_name`: none for `..` and roots, where the whole path is the name.
    const auto file_name = path.filename();
    std::string name = (file_name.empty() || file_name == "." || file_name == "..") ? path.string() : file_name.string();
    auto surface = std::make_shared<DocSurface>(*uri, std::move(name), path, format, fonts_, std::move(bytes), std::move(*parsed));
    std::lock_guard lock(mutex_);
    surfaces_.insert_or_assign(*uri, surface);
    return surface;
}

Awaitable<Result<std::shared_ptr<DocSurface>>> DocsServer::open_file(std::filesystem::path path) {
    co_return co_await run_blocking([this, path = std::move(path)] { return open_file_blocking(path); });
}

void DocsServer::close(const cs::SurfaceUri& uri) {
    std::lock_guard lock(mutex_);
    surfaces_.erase(uri);
}

Result<std::shared_ptr<DocSurface>> DocsServer::surface(const cs::SurfaceUri& uri) const {
    std::lock_guard lock(mutex_);
    const auto it = surfaces_.find(uri);
    if (it == surfaces_.end()) return std::unexpected(DocsError::surface(cs::SurfaceError::not_found(uri)));
    return it->second;
}

} // namespace loa::surfaces::docs
