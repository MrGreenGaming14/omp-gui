#include "loa/surfaces/hub/service.hpp"

#include <asio.hpp>
#include <asio/experimental/channel.hpp>

#include <algorithm>
#include <cstdlib>
#include <deque>
#include <fstream>

#include "loa/runtime/fs.hpp"
#include "loa/surfaces/browser/server.hpp"
#include "loa/surfaces/browser/xvfb.hpp"
#include "loa/surfaces/docs/server.hpp"
#include "loa/surfaces/hub/providers.hpp"

namespace loa::surfaces::hub {

namespace {

// Events a receiver holds before the oldest are dropped (the Rust `broadcast::channel(64)`).
constexpr std::size_t EVENT_CAPACITY = 64;

// `s` as Rust's `Debug` for `str` writes it.
std::string debug_str(std::string_view s) {
    std::string out = "\"";
    for (const char c : s) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            case '\0': out += "\\0"; break;
            default:
                if (static_cast<unsigned char>(c) < 0x20 || c == 0x7f) {
                    static constexpr char hex[] = "0123456789abcdef";
                    out += "\\u{";
                    if (static_cast<unsigned char>(c) >= 0x10) out += hex[static_cast<unsigned char>(c) >> 4];
                    out += hex[static_cast<unsigned char>(c) & 0xf];
                    out += '}';
                } else {
                    out += c;
                }
        }
    }
    return out + "\"";
}

std::optional<bool> proc_flag(const char* path) {
    std::ifstream in(path);
    if (!in) return std::nullopt;
    std::string text((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    const auto first = text.find_first_not_of(" \t\r\n");
    const auto last = text.find_last_not_of(" \t\r\n");
    const std::string trimmed = first == std::string::npos ? std::string() : text.substr(first, last - first + 1);
    return trimmed == "1";
}

} // namespace

std::expected<VirtualDisplaySetup, std::string> VirtualDisplaySetup::find(std::optional<std::string_view> path_var,
                                                                          const std::optional<Path>& home) {
#if !defined(__linux__)
    (void)path_var;
    (void)home;
    return std::unexpected(std::string(browser::VIRTUAL_DISPLAY_UNAVAILABLE));
#else
    auto xvfb = browser::find_xvfb(path_var);
    if (!xvfb) return std::unexpected(std::string("Xvfb isn't installed (install the xvfb package)"));
    auto chrome = browser::find_chrome(path_var, home, true);
    if (!chrome) return std::unexpected(std::string("no Chrome or Chromium that runs headed (the headless shell can't)"));
    return VirtualDisplaySetup{std::move(*xvfb), std::move(*chrome)};
#endif
}

HubConfig HubConfig::discover(const Path& state_dir) {
    HubConfig config;
    if (auto browser = browser::BrowserConfig::discover()) {
        browser->profile_root = state_dir / "browser";
        browser->sandbox = user_namespaces();
        config.browser = std::move(*browser);
    }
    const char* path = std::getenv("PATH");
    const auto path_var = path ? std::optional<std::string_view>(path) : std::nullopt;
    config.fonts = docs::Fonts::system();
    config.screenshot_dir = state_dir / "screenshots";
#ifdef LOA_SURFACES_REMOTE_VIDEO
    config.video = VideoConfig::discover();
#endif
    config.capture = CaptureMode::Screencast;
    config.virtual_display = VirtualDisplaySetup::find(path_var, loa::runtime::fs::home());
    return config;
}

bool user_namespaces() {
    const bool restricted = proc_flag("/proc/sys/kernel/apparmor_restrict_unprivileged_userns") == std::optional(true);
    const bool disabled = proc_flag("/proc/sys/kernel/unprivileged_userns_clone") == std::optional(false);
    return !(restricted || disabled);
}

std::string_view Origin::server() const noexcept { return kind_ == Kind::Mcp ? std::string_view(server_) : provider(); }

std::string_view Origin::provider() const noexcept {
    switch (kind_) {
        case Kind::Browser: return browser::SERVER_NAME;
        case Kind::Document: return docs::SERVER_NAME;
        case Kind::Vm: return vnc::SERVER_NAME;
        case Kind::Mcp: break;
    }
    return "mcp";
}

Opened Opened::make(std::shared_ptr<cs::Surface> surface, Origin origin) {
    auto info = surface->info();
    return Opened{std::move(info), std::move(origin), std::move(surface)};
}

std::string debug(const OpenRequest& req) {
    return std::visit(
        [](const auto& r) -> std::string {
            using T = std::decay_t<decltype(r)>;
            if constexpr (std::is_same_v<T, open_request::Url>) {
                return "Url(" + debug_str(r.url) + ")";
            } else if constexpr (std::is_same_v<T, open_request::File>) {
                return "File(" + debug_str(r.path.string()) + ")";
            } else if constexpr (std::is_same_v<T, open_request::Vm>) {
                const std::string password = r.password ? "Some(" + debug_str(*r.password) + ")" : "None";
                return "Vm { address: " + debug_str(r.address) + ", password: " + password + " }";
            } else {
                return "Uri(SurfaceUri(" + debug_str(r.uri.as_str()) + "))";
            }
        },
        req);
}

// ---- events ----------------------------------------------------------------

struct HubEventReceiver::State {
    std::mutex mutex;
    std::deque<HubEvent> queue;
    // Created by the first `recv` on its executor; never replaced.
    std::unique_ptr<asio::experimental::channel<void(asio::error_code)>> signal;

    void push(const HubEvent& event) {
        std::lock_guard lock(mutex);
        if (queue.size() == EVENT_CAPACITY) queue.pop_front();
        queue.push_back(event);
        if (signal) {
            auto* channel = signal.get();
            asio::post(channel->get_executor(), [channel] { channel->try_send(asio::error_code{}); });
        }
    }
};

std::optional<HubEvent> HubEventReceiver::try_recv() {
    std::lock_guard lock(state_->mutex);
    if (state_->queue.empty()) return std::nullopt;
    HubEvent event = std::move(state_->queue.front());
    state_->queue.pop_front();
    return event;
}

Awaitable<HubEvent> HubEventReceiver::recv() {
    auto state = state_;
    const auto executor = co_await asio::this_coro::executor;
    for (;;) {
        asio::experimental::channel<void(asio::error_code)>* signal = nullptr;
        {
            std::lock_guard lock(state->mutex);
            if (!state->queue.empty()) {
                HubEvent event = std::move(state->queue.front());
                state->queue.pop_front();
                co_return event;
            }
            if (!state->signal) {
                state->signal = std::make_unique<asio::experimental::channel<void(asio::error_code)>>(executor, 1);
            }
            signal = state->signal.get();
        }
        co_await signal->async_receive(asio::as_tuple(asio::use_awaitable));
    }
}

// ---- providers -------------------------------------------------------------

Awaitable<std::vector<Listed>> SurfaceProvider::list() { co_return std::vector<Listed>{}; }

Awaitable<Result<void>> SurfaceProvider::close(cs::SurfaceUri) { co_return Result<void>{}; }

Awaitable<void> SurfaceProvider::shutdown() { co_return; }

// ---- hub -------------------------------------------------------------------

SurfaceHub::SurfaceHub(HubConfig config) : config_(std::move(config)) {
    browser_ = std::make_shared<BrowserProvider>(config_.browser, config_.virtual_display, config_.capture);
    docs_ = std::make_shared<DocsProvider>(config_.fonts);
    mcp_ = std::make_shared<McpDeclared>();
    const std::shared_ptr<SurfaceProvider> all[] = {browser_, docs_, mcp_, std::make_shared<VncProvider>(config_.rfb)};
    for (const auto& provider : all) providers_.insert(std::string(provider->name()), provider);
#ifdef LOA_SURFACES_REMOTE_VIDEO
    video_ = std::make_unique<RemoteVideo>(config_.video);
#endif
}

SurfaceHub::~SurfaceHub() = default;

std::shared_ptr<SurfaceHub> SurfaceHub::make(HubConfig config) { return std::shared_ptr<SurfaceHub>(new SurfaceHub(std::move(config))); }

std::optional<Path> SurfaceHub::browser_executable() const {
    if (!config_.browser) return std::nullopt;
    return config_.browser->executable;
}

bool SurfaceHub::browser_sandboxed() const { return config_.browser && config_.browser->sandbox; }

HubEventReceiver SurfaceHub::events() {
    auto state = std::make_shared<HubEventReceiver::State>();
    std::lock_guard lock(mutex_);
    std::erase_if(subscribers_, [](const auto& weak) { return weak.expired(); });
    subscribers_.push_back(state);
    return HubEventReceiver(std::move(state));
}

void SurfaceHub::emit(const HubEvent& event) {
    std::vector<std::shared_ptr<HubEventReceiver::State>> live;
    {
        std::lock_guard lock(mutex_);
        for (const auto& weak : subscribers_) {
            if (auto state = weak.lock()) live.push_back(std::move(state));
        }
    }
    // No receiver is fine: nobody is watching yet.
    for (const auto& state : live) state->push(event);
}

Awaitable<Result<Opened>> SurfaceHub::open_request(OpenRequest req) {
    std::string_view provider;
    if (std::holds_alternative<open_request::Url>(req)) {
        provider = browser::SERVER_NAME;
    } else if (std::holds_alternative<open_request::File>(req)) {
        provider = docs::SERVER_NAME;
    } else if (std::holds_alternative<open_request::Vm>(req)) {
        provider = vnc::SERVER_NAME;
    } else {
        const auto& uri = std::get<open_request::Uri>(req).uri;
        if (auto found = opened(uri)) co_return std::move(*found);
        const auto listed = co_await this->listed();
        const auto it = std::ranges::find_if(listed, [&](const Listed& l) { return l.info.uri == uri; });
        if (it == listed.end()) co_return std::unexpected(HubError::not_found(uri));
        provider = it->origin.provider();
    }
    auto found = providers_.get(provider);
    if (!found) co_return std::unexpected(HubError::unsupported(std::string(provider)));
    auto opened = co_await found->open(std::move(req));
    if (!opened) co_return std::unexpected(std::move(opened.error()));
    co_return register_opened(std::move(*opened));
}

Awaitable<Result<Opened>> SurfaceHub::open_browser(std::string url) { return open_request(open_request::Url{std::move(url)}); }

Awaitable<Result<Opened>> SurfaceHub::open_document(Path path) { return open_request(open_request::File{std::move(path)}); }

Awaitable<Result<Opened>> SurfaceHub::open_vm(std::string address, std::optional<std::string> password) {
    return open_request(open_request::Vm{std::move(address), std::move(password)});
}

Awaitable<Result<Opened>> SurfaceHub::open(cs::SurfaceUri uri) { return open_request(open_request::Uri{std::move(uri)}); }

#ifdef LOA_SURFACES_REMOTE_VIDEO
Awaitable<Result<std::shared_ptr<cs::Surface>>> SurfaceHub::network_surface(cs::SurfaceUri uri) {
    auto found = opened(uri);
    if (!found) co_return std::unexpected(HubError::not_found(uri));
    co_return co_await video_->surface(std::move(*found));
}
#endif

CaptureMode SurfaceHub::capture() const { return browser_->capture(); }

Result<void> SurfaceHub::set_capture(CaptureMode mode) { return browser_->set_capture(mode); }

bool SurfaceHub::capture_pending() const { return browser_->capture_pending(); }

Awaitable<Result<std::shared_ptr<browser::Tab>>> SurfaceHub::tab(cs::SurfaceUri uri) { co_return browser_->tab(uri); }

Result<std::shared_ptr<docs::DocSurface>> SurfaceHub::document(const cs::SurfaceUri& uri) const { return docs_->document(uri); }

Opened SurfaceHub::register_opened(Opened opened) {
    {
        std::lock_guard lock(mutex_);
        const auto it = std::ranges::find_if(open_, [&](const Opened& o) { return o.info.uri == opened.info.uri; });
        if (it != open_.end()) return *it;
        open_.push_back(opened);
    }
    emit(hub_event::Opened{opened.info, opened.origin});
    return opened;
}

std::optional<Opened> SurfaceHub::opened(const cs::SurfaceUri& uri) const {
    std::lock_guard lock(mutex_);
    const auto it = std::ranges::find_if(open_, [&](const Opened& o) { return o.info.uri == uri; });
    if (it == open_.end()) return std::nullopt;
    return *it;
}

std::vector<Opened> SurfaceHub::open_surfaces() const {
    std::lock_guard lock(mutex_);
    return open_;
}

Awaitable<Result<void>> SurfaceHub::close(cs::SurfaceUri uri) {
    std::optional<Opened> closed;
    {
        std::lock_guard lock(mutex_);
        const auto it = std::ranges::find_if(open_, [&](const Opened& o) { return o.info.uri == uri; });
        if (it != open_.end()) {
            closed = std::move(*it);
            open_.erase(it);
        }
    }
    if (!closed) co_return std::unexpected(HubError::not_found(uri));
#ifdef LOA_SURFACES_REMOTE_VIDEO
    video_->forget(&uri);
#endif
    if (auto provider = providers_.get(closed->origin.provider())) {
        auto done = co_await provider->close(uri);
        if (!done) co_return std::unexpected(std::move(done.error()));
    }
    emit(hub_event::Closed{std::move(uri)});
    co_return Result<void>{};
}

Awaitable<std::vector<Listed>> SurfaceHub::listed() {
    std::vector<Listed> listed;
    for (const auto& [_, provider] : providers_) {
        auto more = co_await provider->list();
        std::ranges::move(more, std::back_inserter(listed));
    }
    co_return listed;
}

Awaitable<std::vector<Listed>> SurfaceHub::list() {
    const auto open = open_surfaces();
    std::vector<Listed> listed;
    listed.reserve(open.size());
    for (const auto& o : open) listed.push_back(Listed{o.surface->info(), o.origin, true});
    for (auto& l : co_await this->listed()) {
        const bool is_open = std::ranges::any_of(open, [&](const Opened& o) { return o.info.uri == l.info.uri; });
        if (!is_open) listed.push_back(std::move(l));
    }
    co_return listed;
}

std::map<std::string, std::vector<cs::SurfaceInfo>> SurfaceHub::declared() const { return mcp_->declared(); }

Awaitable<std::vector<std::pair<std::string, std::string>>> SurfaceHub::refresh_declared(mcp::Manager manager) {
    auto failed = co_await mcp_->refresh(std::move(manager));
    emit(hub_event::Declared{});
    co_return failed;
}

Awaitable<void> SurfaceHub::shutdown() {
    {
        std::lock_guard lock(mutex_);
        open_.clear();
    }
#ifdef LOA_SURFACES_REMOTE_VIDEO
    video_->forget(nullptr);
#endif
    for (const auto& [_, provider] : providers_) co_await provider->shutdown();
}

std::string normalize_url(std::string_view url) {
    const auto first = url.find_first_not_of(" \t\r\n\f\v");
    const auto last = url.find_last_not_of(" \t\r\n\f\v");
    url = first == std::string_view::npos ? std::string_view() : url.substr(first, last - first + 1);
    const bool has_scheme = url.find("://") != std::string_view::npos ||
                            std::ranges::any_of(std::array<std::string_view, 4>{"about:", "data:", "file:", "chrome:"},
                                                [&](std::string_view s) { return url.starts_with(s); });
    if (has_scheme || url.empty()) return std::string(url);
    if (url.starts_with("localhost") || url.starts_with("127.")) return "http://" + std::string(url);
    return "https://" + std::string(url);
}

} // namespace loa::surfaces::hub
