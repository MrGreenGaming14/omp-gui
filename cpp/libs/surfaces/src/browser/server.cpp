#include "loa/surfaces/browser/server.hpp"

#include <asio/as_tuple.hpp>
#include <asio/co_spawn.hpp>
#include <asio/detached.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/experimental/concurrent_channel.hpp>
#include <asio/steady_timer.hpp>
#include <asio/this_coro.hpp>
#include <asio/use_awaitable.hpp>

#include <unistd.h>

#include <array>
#include <thread>

#include "browser/cdp.hpp"
#include "browser/process.hpp"
#include "loa/contracts/surface_channel.hpp"
#include "loa/runtime/log.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/runtime/time.hpp"
#include "loa/surfaces/browser/input.hpp"
#include "loa/surfaces/feed.hpp"
#if defined(__linux__)
#include "loa/surfaces/browser/x11.hpp"
#include "loa/surfaces/browser/xvfb.hpp"
#endif

namespace loa::surfaces::browser {

namespace cs = loa::contracts::surface;
namespace json = loa::runtime::json;
using loa::contracts::Awaitable;
using namespace asio::experimental::awaitable_operators;

namespace {

// CDP parameters from `key, value` pairs. Keeps braced lists out of coroutine
// bodies, where g++ 13 hits an internal compiler error on them.
inline void add_params(json::Value&) {}
template <typename V, typename... Rest>
void add_params(json::Value& out, std::string_view key, V&& value, Rest&&... rest) {
    out.set(key, json::Value(std::forward<V>(value)));
    add_params(out, std::forward<Rest>(rest)...);
}
template <typename... A>
json::Value params(A&&... a) {
    json::Value out{json::Object()};
    add_params(out, std::forward<A>(a)...);
    return out;
}

json::Value display_styles() {
    json::Array styles;
    styles.emplace_back("display");
    return styles;
}

constexpr auto use_tuple = asio::as_tuple(asio::use_awaitable);
constexpr std::string_view LOG = "loa_surfaces::browser";

// How long Chrome gets to exit after `Browser.close` before it is killed.
constexpr auto CLOSE_TIMEOUT = std::chrono::seconds(5);
// How long Chrome may take to print its DevTools address.
constexpr auto LAUNCH_TIMEOUT = std::chrono::seconds(20);
// How long a navigation may take to fire its load event.
constexpr auto NAVIGATION_TIMEOUT = std::chrono::seconds(30);

// chromiumoxide's default flags (Puppeteer's), used for the headless browser.
constexpr std::array<std::string_view, 24> DEFAULT_ARGS = {
    "disable-background-networking",
    "enable-features=NetworkService,NetworkServiceInProcess",
    "disable-background-timer-throttling",
    "disable-backgrounding-occluded-windows",
    "disable-breakpad",
    "disable-client-side-phishing-detection",
    "disable-component-extensions-with-background-pages",
    "disable-default-apps",
    "disable-dev-shm-usage",
    "disable-features=TranslateUI",
    "disable-hang-monitor",
    "disable-ipc-flooding-protection",
    "disable-popup-blocking",
    "disable-prompt-on-repost",
    "disable-renderer-backgrounding",
    "disable-sync",
    "force-color-profile=srgb",
    "metrics-recording-only",
    "no-first-run",
    "enable-automation",
    "password-store=basic",
    "use-mock-keychain",
    "enable-blink-features=IdleDetection",
    "lang=en_US",
};

// The default flags without `enable-automation`, plus the switches that keep
// infobars off a headed window: they would sit on top of the captured page,
// shifting it away from where input lands.
constexpr std::array<std::string_view, 26> HEADED_ARGS = {
    "disable-background-networking",
    "enable-features=NetworkService,NetworkServiceInProcess",
    "disable-background-timer-throttling",
    "disable-backgrounding-occluded-windows",
    "disable-breakpad",
    "disable-client-side-phishing-detection",
    "disable-component-extensions-with-background-pages",
    "disable-default-apps",
    "disable-dev-shm-usage",
    "disable-features=TranslateUI",
    "disable-hang-monitor",
    "disable-ipc-flooding-protection",
    "disable-popup-blocking",
    "disable-prompt-on-repost",
    "disable-renderer-backgrounding",
    "disable-sync",
    "force-color-profile=srgb",
    "metrics-recording-only",
    "no-first-run",
    "password-store=basic",
    "use-mock-keychain",
    "enable-blink-features=IdleDetection",
    "lang=en_US",
    "disable-infobars",
    // Hides the "unsupported command-line flag" bar `--no-sandbox` brings.
    "test-type",
    "noerrdialogs",
};

// Holds a tokio-style async mutex: one waiter at a time, in order.
class AsyncMutex {
  public:
    explicit AsyncMutex(const asio::any_io_executor& ex) : slot_(ex, 1) {}
    asio::awaitable<void> lock() { co_await slot_.async_send(asio::error_code{}, asio::use_awaitable); }
    void unlock() { slot_.try_receive([](asio::error_code) {}); }

  private:
    asio::experimental::concurrent_channel<void(asio::error_code)> slot_;
};

struct Unlock {
    AsyncMutex* mutex;
    ~Unlock() { mutex->unlock(); }
};

struct SharedLog {
    std::mutex mutex;
    InputLog log;
};

struct SharedSize {
    mutable std::mutex mutex;
    std::pair<std::uint32_t, std::uint32_t> size;
    std::pair<std::uint32_t, std::uint32_t> get() const {
        std::lock_guard lock(mutex);
        return size;
    }
    void set(std::pair<std::uint32_t, std::uint32_t> value) {
        std::lock_guard lock(mutex);
        size = value;
    }
};

Result<json::Value> field(Result<json::Value> result, std::string_view name) {
    if (!result) return result;
    if (const auto* v = result->find(name)) return *v;
    return std::unexpected(BrowserError::cdp("reply without " + std::string(name)));
}

// Emulates a `width` x `height` viewport at one device pixel per CSS pixel,
// so surface coordinates are CSS pixels.
Awaitable<Result<void>> set_viewport(const cdp::Page& page, std::uint32_t width, std::uint32_t height) {
    auto r = co_await page.call("Emulation.setDeviceMetricsOverride",
                                params("width", std::int64_t(width), "height", std::int64_t(height), "deviceScaleFactor", 1.0, "mobile", false));
    if (!r) co_return std::unexpected(r.error());
    co_return Result<void>{};
}

// Evaluates `expression` in the page and returns its value.
Awaitable<Result<json::Value>> evaluate(const cdp::Page& page, std::string expression) {
    auto r = co_await page.call("Runtime.evaluate",
                                params("expression", std::move(expression), "returnByValue", true, "awaitPromise", true));
    if (!r) co_return std::unexpected(r.error());
    if (const auto* ex = r->find("exceptionDetails")) {
        const auto* text = ex->find("text");
        co_return std::unexpected(BrowserError::script(text && text->as_string() ? *text->as_string() : json::dump(*ex)));
    }
    const auto* result = r->find("result");
    const auto* value = result ? result->find("value") : nullptr;
    co_return value ? *value : json::Value{};
}

// Where a tab's frames come from: Chrome's screencast or a virtual display grab.
class Capture {
  public:
    virtual ~Capture() = default;
    // The viewport the capture can show for a requested `width` x `height`.
    [[nodiscard]] virtual std::pair<std::uint32_t, std::uint32_t> fit(std::uint32_t width, std::uint32_t height) const {
        return {width, height};
    }
    // Sets the page viewport and captures at the new size.
    virtual Awaitable<Result<void>> resize(const cdp::Page& page, std::uint32_t width, std::uint32_t height) = 0;
    // Makes the next frame a whole picture; `subscribe` when a viewer just subscribed.
    virtual void keyframe(FrameFeed& hub, bool subscribe) = 0;
    virtual void stop() = 0;
};

// Frames from Chrome's `Page.startScreencast`: the headless capture strategy.
namespace screencast {

// Starts the screencast at `width` x `height`. Chrome sends a frame on each
// repaint and waits for the acknowledgement before sending the next.
Awaitable<Result<void>> start(const cdp::Page& page, std::uint32_t width, std::uint32_t height, std::uint8_t quality) {
    auto r = co_await page.call("Page.startScreencast", params("format", "jpeg", "quality", std::int64_t(std::min<std::uint8_t>(quality, 100)), "maxWidth", std::int64_t(width), "maxHeight", std::int64_t(height), "everyNthFrame", std::int64_t(1)));
    if (!r) co_return std::unexpected(r.error());
    co_return Result<void>{};
}

Awaitable<Result<void>> stop(const cdp::Page& page) {
    auto r = co_await page.call("Page.stopScreencast");
    if (!r) co_return std::unexpected(r.error());
    co_return Result<void>{};
}

// Publishes screencast frames until unsubscribed. Subscribed before `start`
// so no frame is missed.
std::uint64_t pump(const cdp::Page& page, std::shared_ptr<FrameFeed> hub, std::shared_ptr<SharedLog> log) {
    std::weak_ptr<cdp::Connection> weak = page.conn;
    return page.conn->subscribe(page.session_id, "Page.screencastFrame",
                                [weak, session = page.session_id, hub, log](const json::Value& event) {
        std::uint64_t captured_us = loa::runtime::time::now_us();
        if (const auto* meta = event.find("metadata")) {
            if (const auto* t = meta->find("timestamp"); t && t->as_f64()) captured_us = std::uint64_t(*t->as_f64() * 1e6);
        }
        const auto* data = event.find("data");
        auto jpeg = data && data->as_string() ? cdp::base64_decode(*data->as_string()) : std::nullopt;
        if (!jpeg) {
            loa::runtime::log::warn(LOG, "screencast frame is not base64");
        } else {
            std::uint64_t input_seq = 0;
            {
                std::lock_guard lock(log->mutex);
                input_seq = log->log.seq_before(captured_us);
            }
            // Metadata may describe the new viewport for a frame still encoded
            // at the old size during resize. The JPEG header is authoritative.
            if (auto size = cdp::jpeg_size(*jpeg)) {
                cs::Frame frame;
                frame.captured_us = captured_us;
                frame.input_seq = input_seq;
                frame.width = size->first;
                frame.height = size->second;
                frame.region = {0, 0, size->first, size->second};
                frame.encoding = cs::Encoding::Jpeg;
                frame.keyframe = true;
                frame.data = std::move(*jpeg);
                hub->publish(std::move(frame));
            } else {
                loa::runtime::log::warn(LOG, "screencast frame is not a JPEG");
            }
        }
        const auto* sid = event.find("sessionId");
        if (auto conn = weak.lock(); conn && sid && sid->as_i64()) {
            conn->send("Page.screencastFrameAck", params("sessionId", *sid->as_i64()), session);
        }
    });
}

class Screencast final : public Capture {
  public:
    Screencast(cdp::Page page, std::uint64_t token, std::uint8_t quality)
        : page_(std::move(page)), token_(token), quality_(quality) {}

    Awaitable<Result<void>> resize(const cdp::Page& page, std::uint32_t width, std::uint32_t height) override {
        if (auto r = co_await screencast::stop(page); !r) co_return r;
        if (auto r = co_await set_viewport(page, width, height); !r) co_return r;
        co_return co_await start(page, width, height, quality_);
    }

    // Every frame is a whole JPEG and a static page sends no new ones, so the
    // last is sent again. A new viewer gets it too: navigation can finish
    // after the only repaint.
    void keyframe(FrameFeed& hub, bool) override { hub.republish_keyframe(); }

    void stop() override { page_.conn->unsubscribe(token_); }

  private:
    cdp::Page page_;
    std::uint64_t token_;
    std::uint8_t quality_;
};

} // namespace screencast

#if defined(__linux__)
// What the capture thread needs from its tab.
struct CaptureTarget {
    std::shared_ptr<FrameFeed> hub;
    std::shared_ptr<SharedLog> log;
    // Surface size; the capture is clipped to it.
    std::shared_ptr<SharedSize> size;
    std::atomic<bool> stop{false};
    std::atomic<bool> keyframe{true};
    std::chrono::nanoseconds interval;
};

// Captures on a thread of its own (xcb is blocking) until `stop` is set or the
// server goes away. The first capture is always a keyframe.
void spawn_capture(X11Capture capture, std::shared_ptr<CaptureTarget> target) {
    std::thread([capture = std::move(capture), target]() mutable {
        target->keyframe.store(true, std::memory_order_relaxed);
        while (!target->stop.load(std::memory_order_relaxed)) {
            const auto started = std::chrono::steady_clock::now();
            const auto [width, height] = target->size->get();
            const auto captured_us = loa::runtime::time::now_us();
            std::uint64_t input_seq = 0;
            {
                std::lock_guard lock(target->log->mutex);
                input_seq = target->log->log.seq_before(captured_us);
            }
            auto grabbed = capture.grab(width, height, target->keyframe.exchange(false, std::memory_order_relaxed));
            if (!grabbed) {
                loa::runtime::log::warn(LOG, "virtual display capture stopped: " + grabbed.error().message());
                return;
            }
            if (*grabbed) {
                auto frame = std::move(**grabbed);
                frame.captured_us = captured_us;
                frame.input_seq = input_seq;
                target->hub->publish(std::move(frame));
            }
            // Pacing, not synchronization: caps the capture rate.
            const auto elapsed = std::chrono::steady_clock::now() - started;
            if (elapsed < target->interval) std::this_thread::sleep_for(target->interval - elapsed);
        }
    }).detach();
}

// The headed capture strategy: a tab's virtual display grabbed by `spawn_capture`.
class X11Display final : public Capture {
  public:
    X11Display(std::shared_ptr<CaptureTarget> target, std::pair<std::uint32_t, std::uint32_t> screen)
        : target_(std::move(target)), screen_(screen) {}

    std::pair<std::uint32_t, std::uint32_t> fit(std::uint32_t width, std::uint32_t height) const override {
        return {std::min(width, screen_.first), std::min(height, screen_.second)};
    }

    Awaitable<Result<void>> resize(const cdp::Page& page, std::uint32_t width, std::uint32_t height) override {
        if (auto r = co_await set_viewport(page, width, height); !r) co_return r;
        target_->size->set({width, height});
        target_->keyframe.store(true, std::memory_order_relaxed);
        co_return Result<void>{};
    }

    // Grabs are diffed, so only an explicit request needs a whole picture.
    void keyframe(FrameFeed&, bool subscribe) override {
        if (!subscribe) target_->keyframe.store(true, std::memory_order_relaxed);
    }

    void stop() override { target_->stop.store(true, std::memory_order_relaxed); }

  private:
    std::shared_ptr<CaptureTarget> target_;
    // The display's screen; the viewport can't outgrow it.
    std::pair<std::uint32_t, std::uint32_t> screen_;
};
#else
// Xvfb exists only on Linux; no instance of this is ever made elsewhere.
class VirtualDisplay {
  public:
    std::string display() const { return {}; }
    Awaitable<Result<void>> stop() { co_return Result<void>{}; }
};
#endif

} // namespace

// One running Chrome process and its DevTools connection.
class Instance {
  public:
    static Awaitable<Result<std::shared_ptr<Instance>>> launch(const BrowserConfig& config,
                                                                std::optional<VirtualDisplay> display) {
        auto profile = loa::runtime::TempDir::create(config.profile_root.value_or(Path{}));
        if (!profile) co_return std::unexpected(BrowserError::io(profile.error().to_string()));
        const bool headed = display.has_value();
        std::vector<std::string> args;
        auto add = [&](std::string_view a) {
            while (a.starts_with('-')) a.remove_prefix(1);
            args.push_back("--" + std::string(a));
        };
        if (!headed) {
            for (auto a : DEFAULT_ARGS) add(a);
        }
        for (const auto& a : config.extra_args) add(a);
        if (headed) {
            for (auto a : HEADED_ARGS) add(a);
            for (auto a : {"ozone-platform=x11", "kiosk", "window-position=0,0", "no-default-browser-check"}) add(a);
        }
        add("remote-debugging-port=0");
        add("disable-extensions");
        args.push_back("--user-data-dir=" + profile->path().string());
        add("window-size=" + std::to_string(config.width) + "," + std::to_string(config.height));
        if (!config.sandbox) {
            add("no-sandbox");
            add("disable-setuid-sandbox");
        }
        if (!headed) {
            add("headless");
            add("hide-scrollbars");
            add("mute-audio");
        }
        std::vector<std::pair<std::string, std::string>> env;
        if (display) env.emplace_back("DISPLAY", display->display());
        auto spawned = detail::Process::spawn(config.executable, args, env, false);
        if (!spawned) co_return std::unexpected(BrowserError::cdp("launching " + config.executable.string() + ": " + spawned.error().detail()));
        auto process = std::make_unique<detail::Process>(std::move(*spawned));
        const int err = process->take_stderr();
        const auto deadline = std::chrono::steady_clock::now() + LAUNCH_TIMEOUT;
        std::optional<std::string> url;
        std::string output;
        while (!url) {
            auto line = co_await detail::read_line(err, deadline);
            if (!line) break;
            if (const auto at = line->find("ws://"); at != std::string::npos) {
                auto u = line->substr(at);
                while (!u.empty() && (u.back() == '\r' || u.back() == ' ')) u.pop_back();
                url = std::move(u);
            } else {
                output += *line + "\n";
            }
        }
        if (!url) {
            ::close(err);
            co_return std::unexpected(BrowserError::cdp("chrome did not report its DevTools address: " + output));
        }
        detail::drain_lines(err, "chrome");
        auto conn = co_await cdp::Connection::connect(co_await asio::this_coro::executor, *url);
        if (!conn) co_return std::unexpected(conn.error());
        auto instance = std::shared_ptr<Instance>(new Instance());
        instance->profile_.emplace(std::move(*profile));
        instance->process_ = std::move(process);
        instance->conn_ = std::move(*conn);
        if (display) instance->display_.emplace(std::move(*display));
        co_return instance;
    }

    ~Instance() {
        if (conn_) conn_->close();
        // The Chrome and Xvfb processes are killed on destruction by their handles.
    }

    Awaitable<Result<cdp::Page>> new_page() {
        auto target = field(co_await conn_->call("Target.createTarget", params("url", "about:blank")), "targetId");
        if (!target) co_return std::unexpected(target.error());
        const std::string target_id = *target->as_string();
        auto session = field(co_await conn_->call("Target.attachToTarget",
                                                  params("targetId", target_id, "flatten", true)),
                             "sessionId");
        if (!session) co_return std::unexpected(session.error());
        cdp::Page page{conn_, target_id, *session->as_string()};
        if (auto r = co_await page.call("Page.enable"); !r) co_return std::unexpected(r.error());
        co_return page;
    }

    [[nodiscard]] std::optional<std::uint32_t> pid() const {
        std::lock_guard lock(mutex_);
        if (!process_ || process_->pid() <= 0) return std::nullopt;
        return std::uint32_t(process_->pid());
    }

    // Closes Chrome (killing it if it does not exit in time), then the display.
    Awaitable<Result<void>> shutdown() {
        if (shut_.exchange(true)) co_return Result<void>{};
        (void)co_await conn_->call("Browser.close", params(), {}, CLOSE_TIMEOUT);
        bool exited = co_await process_->wait(CLOSE_TIMEOUT);
        if (!exited) {
            process_->kill();
            exited = co_await process_->wait(CLOSE_TIMEOUT);
            if (!exited) loa::runtime::log::warn(LOG, "killing chrome: it did not exit");
        }
        conn_->close();
        if (display_) {
            auto display = std::move(*display_);
            display_.reset();
            if (auto r = co_await display.stop(); !r) co_return r;
        }
        co_return Result<void>{};
    }

    [[nodiscard]] const std::shared_ptr<cdp::Connection>& conn() const { return conn_; }

  private:
    Instance() = default;

    // Declared first, destroyed last: after Chrome is gone.
    std::optional<loa::runtime::TempDir> profile_;
    mutable std::mutex mutex_;
    std::unique_ptr<detail::Process> process_;
    std::shared_ptr<cdp::Connection> conn_;
    // The Xvfb server the browser draws on, in display mode.
    std::optional<VirtualDisplay> display_;
    std::atomic<bool> shut_{false};
};

struct Tab::Impl {
    Impl(const asio::any_io_executor& ex, cs::SurfaceUri uri_, cdp::Page page_, std::shared_ptr<Instance> instance_,
         std::shared_ptr<FrameFeed> hub_, std::shared_ptr<SharedLog> log_, std::shared_ptr<SharedSize> size_,
         std::unique_ptr<Capture> capture_)
        : uri(std::move(uri_)), page(std::move(page_)), instance(std::move(instance_)), hub(std::move(hub_)),
          log(std::move(log_)), size(std::move(size_)), dispatching(ex), label(uri.name()), capture(std::move(capture_)) {}

    ~Impl() { stop_feed(); }

    cs::SurfaceUri uri;
    cdp::Page page;
    // Shared with the other tabs in screencast mode, owned in display mode.
    std::shared_ptr<Instance> instance;
    std::shared_ptr<FrameFeed> hub;
    std::shared_ptr<SharedLog> log;
    std::shared_ptr<SharedSize> size;
    // Held while an input is dispatched so inputs reach Chrome in order.
    AsyncMutex dispatching;
    InputMapper mapper;
    mutable std::mutex label_mutex;
    std::string label;
    std::unique_ptr<Capture> capture;
    std::atomic<bool> stopped{false};

    // Stops the frame feed. The page itself is closed by the server.
    void stop_feed() {
        if (!stopped.exchange(true)) capture->stop();
    }

    // Sets the page viewport, clipped to what the capture can show.
    Awaitable<Result<void>> resize(std::uint32_t width, std::uint32_t height) {
        const auto fitted = capture->fit(width, height);
        if (fitted == size->get()) co_return Result<void>{};
        if (auto r = co_await capture->resize(page, fitted.first, fitted.second); !r) co_return r;
        size->set(fitted);
        co_return Result<void>{};
    }

    // Dispatches one input. It is logged once Chrome has handled it, so a
    // frame is only stamped with inputs whose effect it can contain.
    Awaitable<Result<void>> dispatch(const cs::InputEvent& event) {
        co_await dispatching.lock();
        Unlock unlock{&dispatching};
        auto command = mapper.map(event.input, InputMapper::Clock::now());
        if (!command) co_return Result<void>{};
        Result<json::Value> sent = json::Value{};
        if (const auto* m = std::get_if<MouseParams>(&*command)) {
            sent = co_await page.call("Input.dispatchMouseEvent", m->to_json());
        } else if (const auto* k = std::get_if<KeyParams>(&*command)) {
            sent = co_await page.call("Input.dispatchKeyEvent", k->to_json());
        } else if (const auto* t = std::get_if<TextParams>(&*command)) {
            sent = co_await page.call("Input.insertText", params("text", t->text));
        } else {
            const auto& r = std::get<ResizeCommand>(*command);
            if (auto done = co_await resize(r.width, r.height); !done) co_return done;
        }
        if (!sent) co_return std::unexpected(sent.error());
        std::lock_guard lock(log->mutex);
        log->log.record(event.seq, loa::runtime::time::now_us());
        co_return Result<void>{};
    }
};

Tab::Tab(std::shared_ptr<Impl> impl) : impl_(std::move(impl)) {}
Tab::~Tab() = default;

const cs::SurfaceUri& Tab::uri() const { return impl_->uri; }

Awaitable<Result<PageInfo>> Tab::navigate(std::string url) {
    auto impl = impl_;
    auto loaded = std::make_shared<asio::experimental::concurrent_channel<void(asio::error_code)>>(
        co_await asio::this_coro::executor, 1);
    const auto token = impl->page.conn->subscribe(impl->page.session_id, "Page.loadEventFired",
                                                  [loaded](const json::Value&) { loaded->try_send(asio::error_code{}); });
    auto nav = co_await impl->page.call("Page.navigate", params("url", url));
    if (nav) {
        if (const auto* text = nav->find("errorText"); text && text->as_string() && !text->as_string()->empty()) {
            nav = std::unexpected(BrowserError::cdp(*text->as_string()));
        }
    }
    // A same-document navigation has no loader and fires no load event.
    if (nav && nav->find("loaderId")) {
        asio::steady_timer timer(co_await asio::this_coro::executor, NAVIGATION_TIMEOUT);
        auto done = co_await (loaded->async_receive(use_tuple) || timer.async_wait(use_tuple));
        if (done.index() == 1) nav = std::unexpected(BrowserError::cdp("navigation timed out: " + url));
    }
    impl->page.conn->unsubscribe(token);
    if (!nav) co_return std::unexpected(nav.error());
    co_return co_await page_info();
}

Awaitable<Result<PageInfo>> Tab::page_info() {
    auto impl = impl_;
    auto value = co_await evaluate(impl->page, "[location.href, document.title]");
    if (!value) co_return std::unexpected(value.error());
    PageInfo info;
    if (const auto* a = value->as_array(); a && a->size() == 2) {
        if (const auto* s = (*a)[0].as_string()) info.url = *s;
        if (const auto* s = (*a)[1].as_string()) info.title = *s;
    }
    {
        std::lock_guard lock(impl->label_mutex);
        impl->label = info.title.empty() ? info.url : info.title;
    }
    co_return info;
}

Awaitable<Result<std::string>> Tab::page_text() {
    auto impl = impl_;
    auto value = co_await evaluate(impl->page, "document.body ? document.body.innerText : ''");
    if (!value) co_return std::unexpected(value.error());
    if (const auto* s = value->as_string()) co_return *s;
    co_return std::unexpected(BrowserError::script("invalid type: expected a string, got " + json::dump(*value)));
}

Awaitable<Result<std::vector<OutlineNode>>> Tab::outline() {
    auto impl = impl_;
    auto ax = field(co_await impl->page.call("Accessibility.getFullAXTree"), "nodes");
    if (!ax) co_return std::unexpected(ax.error());
    // Chrome requires `computedStyles` even when only layout boxes are needed.
    auto snapshot = co_await impl->page.call("DOMSnapshot.captureSnapshot",
                                             params("computedStyles", display_styles()));
    if (!snapshot) co_return std::unexpected(snapshot.error());
    const auto [width, height] = impl->size->get();
    co_return outline_from(*ax, *snapshot, width, height);
}

cs::SurfaceInfo Tab::info() const {
    const auto [width, height] = impl_->size->get();
    std::lock_guard lock(impl_->label_mutex);
    return cs::SurfaceInfo{impl_->uri, impl_->label, width, height, cs::SurfaceMode::Pixel, std::nullopt};
}

cs::Result<std::shared_ptr<cs::FrameReceiver>> Tab::frames() {
    auto receiver = impl_->hub->subscribe();
    impl_->capture->keyframe(*impl_->hub, true);
    return receiver;
}

Awaitable<cs::Result<void>> Tab::request_keyframe() {
    impl_->capture->keyframe(*impl_->hub, false);
    co_return cs::Result<void>{};
}

Awaitable<cs::Result<void>> Tab::input(cs::InputEvent event) {
    auto impl = impl_;
    auto r = co_await impl->dispatch(event);
    if (!r) co_return std::unexpected(r.error().to_surface());
    co_return cs::Result<void>{};
}

Awaitable<cs::Result<cs::Document>> Tab::document() {
    co_return std::unexpected(cs::SurfaceError::mode(impl_->uri, cs::SurfaceMode::Pixel));
}

Awaitable<cs::Result<cs::Bytes>> Tab::screenshot() {
    auto impl = impl_;
    auto data = field(co_await impl->page.call("Page.captureScreenshot", params("format", "png")), "data");
    if (!data) co_return std::unexpected(data.error().to_surface());
    auto png = data->as_string() ? cdp::base64_decode(*data->as_string()) : std::nullopt;
    if (!png) co_return std::unexpected(BrowserError::script("screenshot is not base64").to_surface());
    co_return std::move(*png);
}

BrowserServer::BrowserServer(BrowserConfig config, std::shared_ptr<Instance> shared)
    : config_(std::move(config)), shared_(std::move(shared)) {}

BrowserServer::~BrowserServer() = default;

Awaitable<Result<std::shared_ptr<BrowserServer>>> BrowserServer::launch(BrowserConfig config) {
    std::shared_ptr<Instance> shared;
    if (std::holds_alternative<Screencast>(config.capture)) {
        auto instance = co_await Instance::launch(config, std::nullopt);
        if (!instance) co_return std::unexpected(instance.error());
        shared = std::move(*instance);
    }
    co_return std::shared_ptr<BrowserServer>(new BrowserServer(std::move(config), std::move(shared)));
}

Awaitable<Result<std::shared_ptr<Tab>>> BrowserServer::open_tab(std::string url) {
    if (closed_.load(std::memory_order_acquire)) co_return std::unexpected(BrowserError::closed());
    const auto id = next_.fetch_add(1, std::memory_order_relaxed) + 1;
    auto uri = cs::SurfaceUri::make(SERVER_NAME, "tab" + std::to_string(id));
    if (!uri) co_return std::unexpected(BrowserError::config(uri.error().message()));
    Result<std::shared_ptr<Tab>> tab = std::unexpected(BrowserError::closed());
    if (const auto* display = std::get_if<VirtualDisplayConfig>(&config_.capture)) {
        tab = co_await open_display_tab(std::move(*uri), *display);
    } else {
        tab = co_await open_screencast_tab(std::move(*uri));
    }
    if (!tab) co_return tab;
    if (auto nav = co_await (*tab)->navigate(std::move(url)); !nav) {
        co_await close(*tab);
        co_return std::unexpected(nav.error());
    }
    {
        std::lock_guard lock(mutex_);
        tabs_.emplace(id, *tab);
    }
    co_return tab;
}

Awaitable<Result<std::shared_ptr<Tab>>> BrowserServer::open_screencast_tab(cs::SurfaceUri uri) {
    auto instance = shared_;
    if (!instance) co_return std::unexpected(BrowserError::closed());
    const auto width = config_.width;
    const auto height = config_.height;
    auto page = co_await instance->new_page();
    if (!page) co_return std::unexpected(page.error());
    auto executor = co_await asio::this_coro::executor;
    auto hub = std::make_shared<FrameFeed>(executor, FRAME_BACKLOG);
    auto log = std::make_shared<SharedLog>();
    std::optional<std::uint64_t> token;
    auto started = co_await set_viewport(*page, width, height);
    if (started) {
        token = screencast::pump(*page, hub, log);
        started = co_await screencast::start(*page, width, height, config_.jpeg_quality);
    }
    if (!started) {
        if (token) page->conn->unsubscribe(*token);
        (void)co_await instance->conn()->call("Target.closeTarget", params("targetId", page->target_id));
        co_return std::unexpected(started.error());
    }
    auto size = std::make_shared<SharedSize>();
    size->set({width, height});
    auto capture = std::make_unique<screencast::Screencast>(*page, *token, config_.jpeg_quality);
    co_return std::make_shared<Tab>(std::make_shared<Tab::Impl>(executor, std::move(uri), std::move(*page), instance, hub,
                                                                log, size, std::move(capture)));
}

Awaitable<Result<std::shared_ptr<Tab>>> BrowserServer::open_display_tab(cs::SurfaceUri uri, VirtualDisplayConfig config) {
#if defined(__linux__)
    const auto width = config_.width;
    const auto height = config_.height;
    auto display = co_await VirtualDisplay::start(config.xvfb, config.socket_dir, width, height);
    if (!display) co_return std::unexpected(display.error());
    const auto socket = display->socket();
    auto launched = co_await Instance::launch(config_, std::move(*display));
    if (!launched) co_return std::unexpected(launched.error());
    auto instance = std::move(*launched);
    auto page = co_await instance->new_page();
    Result<X11Capture> capture = std::unexpected(BrowserError::closed());
    if (page) {
        if (auto r = co_await set_viewport(*page, width, height); !r) {
            page = std::unexpected(r.error());
        } else {
            capture = X11Capture::connect(socket);
        }
    }
    if (!page || !capture) {
        auto error = !page ? page.error() : capture.error();
        (void)co_await instance->shutdown();
        co_return std::unexpected(std::move(error));
    }
    auto executor = co_await asio::this_coro::executor;
    auto hub = std::make_shared<FrameFeed>(executor, FRAME_BACKLOG);
    auto log = std::make_shared<SharedLog>();
    auto size = std::make_shared<SharedSize>();
    size->set({width, height});
    const auto screen = capture->screen_size();
    auto target = std::make_shared<CaptureTarget>();
    target->hub = hub;
    target->log = log;
    target->size = size;
    target->interval = std::chrono::nanoseconds(std::chrono::seconds(1)) / std::max<std::uint32_t>(config.fps, 1);
    // The thread ends when the tab sets `stop` or the display goes away.
    spawn_capture(std::move(*capture), target);
    auto x11 = std::make_unique<X11Display>(target, screen);
    co_return std::make_shared<Tab>(std::make_shared<Tab::Impl>(executor, std::move(uri), std::move(*page), instance, hub,
                                                                log, size, std::move(x11)));
#else
    (void)uri;
    (void)config;
    co_return std::unexpected(BrowserError::display(std::string(VIRTUAL_DISPLAY_UNAVAILABLE)));
#endif
}

Result<std::shared_ptr<Tab>> BrowserServer::tab(const cs::SurfaceUri& uri) const {
    std::lock_guard lock(mutex_);
    for (const auto& [_, t] : tabs_) {
        if (t->uri() == uri) return t;
    }
    return std::unexpected(BrowserError::not_found(uri));
}

std::vector<std::shared_ptr<Tab>> BrowserServer::tabs() const {
    std::lock_guard lock(mutex_);
    std::vector<std::shared_ptr<Tab>> out;
    for (const auto& [_, t] : tabs_) out.push_back(t);
    return out;
}

Awaitable<Result<void>> BrowserServer::close_tab(cs::SurfaceUri uri) {
    std::shared_ptr<Tab> tab;
    {
        std::lock_guard lock(mutex_);
        for (auto it = tabs_.begin(); it != tabs_.end(); ++it) {
            if (it->second->uri() == uri) {
                tab = it->second;
                tabs_.erase(it);
                break;
            }
        }
    }
    if (!tab) co_return std::unexpected(BrowserError::not_found(uri));
    co_await close(std::move(tab));
    co_return Result<void>{};
}

bool BrowserServer::shares(const Tab& tab) const { return shared_ && shared_ == tab.impl().instance; }

Awaitable<void> BrowserServer::close(std::shared_ptr<Tab> tab) {
    tab->impl().stop_feed();
    Result<void> result;
    if (shares(*tab)) {
        auto r = co_await tab->impl().page.conn->call("Target.closeTarget",
                                                      params("targetId", tab->impl().page.target_id));
        if (!r) result = std::unexpected(r.error());
    } else {
        result = co_await tab->impl().instance->shutdown();
    }
    if (!result) loa::runtime::log::warn(LOG, "closing tab " + tab->uri().as_str() + ": " + result.error().message());
}

Awaitable<std::vector<std::uint32_t>> BrowserServer::process_ids() const {
    std::vector<std::uint32_t> ids;
    if (shared_) {
        if (auto pid = shared_->pid()) ids.push_back(*pid);
    }
    for (const auto& tab : tabs()) {
        if (shares(*tab)) continue;
        if (auto pid = tab->impl().instance->pid()) ids.push_back(*pid);
    }
    co_return ids;
}

Awaitable<Result<void>> BrowserServer::shutdown() {
    closed_.store(true, std::memory_order_release);
    std::map<std::uint64_t, std::shared_ptr<Tab>> tabs;
    {
        std::lock_guard lock(mutex_);
        tabs.swap(tabs_);
    }
    for (const auto& [_, tab] : tabs) {
        tab->impl().stop_feed();
        if (!shares(*tab)) {
            if (auto r = co_await tab->impl().instance->shutdown(); !r) co_return r;
        }
    }
    if (shared_) {
        if (auto r = co_await shared_->shutdown(); !r) co_return r;
    }
    co_return Result<void>{};
}

Awaitable<cs::Result<std::vector<cs::SurfaceInfo>>> BrowserServer::list() {
    std::vector<cs::SurfaceInfo> out;
    for (const auto& t : tabs()) out.push_back(t->info());
    co_return out;
}

Awaitable<cs::Result<std::shared_ptr<cs::Surface>>> BrowserServer::open(const cs::SurfaceUri& uri) {
    if (uri.server() != SERVER_NAME) co_return std::unexpected(cs::SurfaceError::not_found(uri));
    auto t = tab(uri);
    if (!t) co_return std::unexpected(t.error().to_surface());
    co_return std::shared_ptr<cs::Surface>(*t);
}

} // namespace loa::surfaces::browser
