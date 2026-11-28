#include "loa/surfaces/hub/mcp_surface.hpp"

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/experimental/channel.hpp>

#include <atomic>
#include <chrono>
#include <mutex>
#include <optional>

#include "loa/mcp/host_tools.hpp"
#include "loa/mcp/toolkit.hpp"
#include "loa/runtime/log.hpp"
#include "loa/runtime/time.hpp"
#include "loa/surfaces/feed.hpp"
#include "loa/surfaces/hub/tools.hpp"
#include "loa/surfaces/pixels.hpp"
#include "off_thread.hpp"

namespace loa::surfaces::hub {

namespace json = loa::runtime::json;
using contracts::Awaitable;
using contracts::ImageBlock;
using contracts::ToolOutput;

namespace {

// Time between screenshots while a viewer watches.
constexpr std::chrono::milliseconds POLL{500};

// Tokio's `Notify` for the poll loop: one stored wake-up, waited for on a strand.
struct Waker : std::enable_shared_from_this<Waker> {
    explicit Waker(const asio::any_io_executor& executor) : strand(asio::make_strand(executor)), signal(strand, 1), timer(strand) {}

    void notify() {
        asio::post(strand, [self = shared_from_this()] { self->signal.try_send(asio::error_code{}); });
    }

    // Until woken or `timeout` passes. Runs on `strand`.
    asio::awaitable<void> wait(std::chrono::milliseconds timeout) {
        using namespace asio::experimental::awaitable_operators;
        timer.expires_after(timeout);
        co_await (signal.async_receive(asio::as_tuple(asio::use_awaitable)) || timer.async_wait(asio::as_tuple(asio::use_awaitable)));
    }

    asio::strand<asio::any_io_executor> strand;
    asio::experimental::channel<void(asio::error_code)> signal;
    asio::steady_timer timer;
    std::atomic<bool> stopped{false};
};

const char* button_name(cs::MouseButton button) {
    switch (button) {
        case cs::MouseButton::Left: return "left";
        case cs::MouseButton::Middle: return "middle";
        case cs::MouseButton::Right: return "right";
        case cs::MouseButton::Back:
        case cs::MouseButton::Forward: break;
    }
    return nullptr;
}

json::Array modifier_names(const cs::Modifiers& m) {
    json::Array out;
    const std::pair<bool, const char*> all[] = {{m.shift, "shift"}, {m.ctrl, "ctrl"}, {m.alt, "alt"}, {m.meta, "meta"}};
    for (const auto& [on, name] : all) {
        if (on) out.emplace_back(name);
    }
    return out;
}

json::Value number(float v) { return static_cast<double>(v); }

} // namespace

struct McpSurface::Shared {
    Shared(mcp::Manager manager, std::string server, cs::SurfaceInfo info, const asio::any_io_executor& executor)
        : manager(std::move(manager)), server(std::move(server)), info(std::move(info)), feed(executor, 4),
          waker(std::make_shared<Waker>(executor)) {}

    mcp::Manager manager;
    std::string server;
    mutable std::mutex mutex;
    cs::SurfaceInfo info;
    // Left button press waiting for its release to become a click.
    std::optional<cs::MouseButton> press;
    FrameFeed feed;
    // Highest input whose tool call returned.
    std::atomic<std::uint64_t> input_done{0};
    std::shared_ptr<Waker> waker;

    std::string uri() const {
        std::lock_guard lock(mutex);
        return info.uri.as_str();
    }

    static Awaitable<cs::Result<ToolOutput>> call(std::shared_ptr<Shared> self, std::string_view tool, json::Object arguments) {
        const std::string name(tool);
        auto result = co_await detail::off_thread<mcp::Result<mcp::model::CallToolResult>>(
            [manager = self->manager, server = self->server, name, arguments = std::move(arguments)]() mutable {
                return manager.call_tool(server, name, std::move(arguments), mcp::AllowAll{});
            });
        if (!result) co_return std::unexpected(cs::SurfaceError::backend(result.error().to_string()));
        auto output = mcp::host_tools::to_output(*result);
        if (output.is_error) {
            co_return std::unexpected(cs::SurfaceError::backend(name + " on " + self->server + ": " + output.joined_text()));
        }
        co_return output;
    }

    // The first image block of `output`.
    static std::optional<cs::Bytes> first_image(const ToolOutput& output) {
        for (const auto& block : output.blocks) {
            if (const auto* image = std::get_if<ImageBlock>(&block)) {
                const auto* bytes = reinterpret_cast<const std::uint8_t*>(image->data.data());
                return cs::Bytes(bytes, bytes + image->data.size());
            }
        }
        return std::nullopt;
    }

    static Awaitable<cs::Result<cs::Bytes>> screenshot(std::shared_ptr<Shared> self) {
        json::Object args{{"uri", self->uri()}};
        auto output = co_await call(self, SCREENSHOT, std::move(args));
        if (!output) co_return std::unexpected(std::move(output.error()));
        auto image = first_image(*output);
        if (!image) co_return std::unexpected(cs::SurfaceError::backend(std::string(SCREENSHOT) + " returned no image"));
        co_return std::move(*image);
    }

    // Takes a screenshot and publishes it as a PNG keyframe.
    static Awaitable<cs::Result<void>> publish(std::shared_ptr<Shared> self) {
        const auto input_seq = self->input_done.load(std::memory_order_acquire);
        auto png = co_await screenshot(self);
        if (!png) co_return std::unexpected(std::move(png.error()));
        const auto size = png_size(*png);
        if (!size) co_return std::unexpected(cs::SurfaceError::backend(std::string(SCREENSHOT) + " isn't a PNG"));
        const auto [width, height] = *size;
        {
            std::lock_guard lock(self->mutex);
            self->info.width = width;
            self->info.height = height;
        }
        cs::Frame frame;
        frame.captured_us = runtime::time::now_us();
        frame.input_seq = input_seq;
        frame.width = width;
        frame.height = height;
        frame.region = cs::Rect{0, 0, width, height};
        frame.encoding = cs::Encoding::Png;
        frame.keyframe = true;
        frame.data = std::move(*png);
        self->feed.publish(std::move(frame));
        co_return cs::Result<void>{};
    }

    static Awaitable<cs::Result<void>> input(std::shared_ptr<Shared> self, cs::InputEvent event) {
        const std::string uri = self->uri();
        if (const auto* button = std::get_if<cs::input::MouseButton>(&event.input)) {
            if (button->pressed) {
                std::lock_guard lock(self->mutex);
                self->press = button->button;
                co_return cs::Result<void>{};
            }
            std::optional<cs::MouseButton> pressed;
            {
                std::lock_guard lock(self->mutex);
                pressed = std::exchange(self->press, std::nullopt);
            }
            if (pressed != button->button) co_return cs::Result<void>{};
            const char* name = button_name(button->button);
            if (!name) co_return std::unexpected(cs::SurfaceError::backend(std::string(CLICK) + " takes left, middle and right clicks"));
            json::Object args{{"uri", uri},
                              {"x", number(button->x)},
                              {"y", number(button->y)},
                              {"button", name},
                              {"modifiers", modifier_names(button->modifiers)}};
            if (auto done = co_await call(self, CLICK, std::move(args)); !done) co_return std::unexpected(std::move(done.error()));
        } else if (const auto* scroll = std::get_if<cs::input::Scroll>(&event.input)) {
            json::Object args{{"uri", uri}, {"x", number(scroll->x)}, {"y", number(scroll->y)}, {"dx", number(scroll->dx)}, {"dy", number(scroll->dy)}};
            if (auto done = co_await call(self, SCROLL, std::move(args)); !done) co_return std::unexpected(std::move(done.error()));
        } else if (const auto* text = std::get_if<cs::input::Text>(&event.input)) {
            json::Object args{{"uri", uri}, {"text", text->text}};
            if (auto done = co_await call(self, TYPE, std::move(args)); !done) co_return std::unexpected(std::move(done.error()));
        } else if (const auto* key = std::get_if<cs::input::Key>(&event.input); key && key->pressed && key->key == "Enter") {
            json::Object args{{"uri", uri}, {"text", ""}, {"submit", true}};
            if (auto done = co_await call(self, TYPE, std::move(args)); !done) co_return std::unexpected(std::move(done.error()));
        } else {
            // Other keys, mouse moves and resizes have no tool.
            co_return cs::Result<void>{};
        }
        auto done = self->input_done.load(std::memory_order_acquire);
        while (done < event.seq && !self->input_done.compare_exchange_weak(done, event.seq, std::memory_order_acq_rel)) {
        }
        self->waker->notify();
        co_return cs::Result<void>{};
    }
};

namespace {

// Publishes screenshots while someone watches: right away when woken, else every `POLL`.
asio::awaitable<void> poll(std::weak_ptr<McpSurface::Shared> weak, std::shared_ptr<Waker> waker) {
    for (;;) {
        if (waker->stopped.load()) co_return;
        {
            auto strong = weak.lock();
            if (!strong) co_return;
            if (strong->feed.viewers() > 0) {
                if (auto published = co_await McpSurface::Shared::publish(strong); !published) {
                    runtime::log::debug("loa_surfaces::hub", "mcp surface screenshot from " + strong->server + ": " + published.error().message());
                }
            }
        }
        if (waker->stopped.load()) co_return;
        co_await waker->wait(POLL);
    }
}

} // namespace

McpSurface::McpSurface(mcp::Manager manager, std::string server, cs::SurfaceInfo info, const asio::any_io_executor& executor) {
    const bool pixel = info.mode == cs::SurfaceMode::Pixel;
    shared_ = std::make_shared<Shared>(std::move(manager), std::move(server), std::move(info), executor);
    if (pixel) asio::co_spawn(shared_->waker->strand, poll(shared_, shared_->waker), asio::detached);
}

McpSurface::~McpSurface() {
    shared_->waker->stopped.store(true);
    shared_->waker->notify();
}

cs::SurfaceInfo McpSurface::info() const {
    std::lock_guard lock(shared_->mutex);
    return shared_->info;
}

cs::Result<cs::SurfaceInfo> McpSurface::in_mode(cs::SurfaceMode mode) const {
    auto current = info();
    if (current.mode != mode) return std::unexpected(cs::SurfaceError::mode(current.uri, current.mode));
    return current;
}

cs::Result<std::shared_ptr<cs::FrameReceiver>> McpSurface::frames() {
    if (auto ok = in_mode(cs::SurfaceMode::Pixel); !ok) return std::unexpected(std::move(ok.error()));
    auto receiver = shared_->feed.subscribe();
    shared_->waker->notify();
    return receiver;
}

Awaitable<cs::Result<void>> McpSurface::request_keyframe() {
    shared_->waker->notify();
    co_return cs::Result<void>{};
}

Awaitable<cs::Result<void>> McpSurface::input(cs::InputEvent event) {
    if (auto ok = in_mode(cs::SurfaceMode::Pixel); !ok) co_return std::unexpected(std::move(ok.error()));
    co_return co_await Shared::input(shared_, std::move(event));
}

Awaitable<cs::Result<cs::Document>> McpSurface::document() {
    auto info = in_mode(cs::SurfaceMode::Document);
    if (!info) co_return std::unexpected(std::move(info.error()));
    auto read = co_await detail::off_thread<mcp::Result<std::vector<mcp::model::ResourceContents>>>(
        [manager = shared_->manager, server = shared_->server, uri = info->uri.as_str()] { return manager.read_resource(server, uri); });
    if (!read) co_return std::unexpected(cs::SurfaceError::backend(read.error().to_string()));
    std::optional<std::pair<std::optional<std::string>, cs::Bytes>> found;
    for (const auto& contents : *read) {
        if (const auto* blob = std::get_if<mcp::model::BlobResourceContents>(&contents)) {
            auto decoded = mcp::image(blob->blob, "");
            if (!decoded) continue;
            const auto& data = std::get<ImageBlock>(*decoded).data;
            const auto* bytes = reinterpret_cast<const std::uint8_t*>(data.data());
            found.emplace(blob->mime_type, cs::Bytes(bytes, bytes + data.size()));
        } else {
            const auto& text = std::get<mcp::model::TextResourceContents>(contents);
            found.emplace(text.mime_type, cs::Bytes(text.text.begin(), text.text.end()));
        }
        break;
    }
    if (!found) co_return std::unexpected(cs::SurfaceError::backend(info->uri.as_str() + " has no contents"));
    std::string mime = found->first ? *found->first : info->mime ? *info->mime : "application/octet-stream";
    co_return cs::Document{info->name, std::move(mime), std::move(found->second)};
}

Awaitable<cs::Result<cs::Bytes>> McpSurface::screenshot() { co_return co_await Shared::screenshot(shared_); }

} // namespace loa::surfaces::hub
