#include "loa/surfaces/hub/providers.hpp"

#include <asio.hpp>

#include <algorithm>
#include <charconv>
#include <system_error>

#include "loa/mcp/tool_filter.hpp"
#include "loa/runtime/log.hpp"
#include "loa/surfaces/browser/server.hpp"
#include "loa/surfaces/hub/mcp.hpp"
#include "loa/surfaces/hub/mcp_surface.hpp"
#include "off_thread.hpp"

namespace loa::surfaces::hub {

namespace {

constexpr std::string_view LOG = "loa_surfaces::hub";

HubError unsupported(std::string_view provider, const OpenRequest& req) {
    return HubError::unsupported("the " + std::string(provider) + " provider can't open " + debug(req));
}

CaptureMode mode_of(const browser::BrowserServer& server) {
    return std::holds_alternative<browser::Screencast>(server.config().capture) ? CaptureMode::Screencast : CaptureMode::VirtualDisplay;
}

std::string_view trim(std::string_view s) {
    const auto first = s.find_first_not_of(" \t\r\n\f\v");
    if (first == std::string_view::npos) return {};
    return s.substr(first, s.find_last_not_of(" \t\r\n\f\v") - first + 1);
}

// Rust `str::parse::<u16>`: an optional `+`, then decimal digits that fit.
bool is_port(std::string_view s) {
    if (s.starts_with('+')) s.remove_prefix(1);
    if (s.empty() || !std::ranges::all_of(s, [](char c) { return c >= '0' && c <= '9'; })) return false;
    std::uint16_t value = 0;
    const auto [end, ec] = std::from_chars(s.data(), s.data() + s.size(), value);
    return ec == std::errc{} && end == s.data() + s.size();
}

} // namespace

// ---- browser ---------------------------------------------------------------

BrowserProvider::BrowserProvider(std::optional<browser::BrowserConfig> config,
                                 std::expected<VirtualDisplaySetup, std::string> virtual_display, CaptureMode capture)
    : config_(std::move(config)), virtual_display_(std::move(virtual_display)), capture_(capture) {}

std::string_view BrowserProvider::name() const { return browser::SERVER_NAME; }

Result<VirtualDisplaySetup> BrowserProvider::display() const {
    if (!virtual_display_) return std::unexpected(HubError::no_virtual_display(virtual_display_.error()));
    return *virtual_display_;
}

CaptureMode BrowserProvider::capture() const {
    std::lock_guard lock(mutex_);
    return capture_;
}

Result<void> BrowserProvider::set_capture(CaptureMode mode) {
    if (mode == CaptureMode::VirtualDisplay) {
        if (auto setup = display(); !setup) return std::unexpected(std::move(setup.error()));
    }
    std::lock_guard lock(mutex_);
    capture_ = mode;
    return {};
}

bool BrowserProvider::capture_pending() const {
    std::lock_guard lock(mutex_);
    return server_ && mode_of(*server_) != capture_ && !server_->tabs().empty();
}

Awaitable<Result<std::shared_ptr<browser::BrowserServer>>> BrowserProvider::server() {
    const CaptureMode mode = capture();
    std::shared_ptr<browser::BrowserServer> current;
    {
        std::lock_guard lock(mutex_);
        current = server_;
    }
    if (current) {
        if (mode_of(*current) == mode || !current->tabs().empty()) co_return current;
        if (auto stopped = co_await current->shutdown(); !stopped) {
            runtime::log::warn(LOG, "stopping the browser to switch its capture: " + stopped.error().message());
        }
        std::lock_guard lock(mutex_);
        if (server_ == current) server_.reset();
    }
    if (!config_) co_return std::unexpected(HubError::no_browser());
    auto config = *config_;
    if (mode == CaptureMode::VirtualDisplay) {
        auto setup = display();
        if (!setup) co_return std::unexpected(std::move(setup.error()));
        config.executable = setup->chrome;
        config.capture = browser::VirtualDisplayConfig::make(setup->xvfb);
    }
    if (config.profile_root) {
        std::error_code ec;
        std::filesystem::create_directories(*config.profile_root, ec);
        if (ec) co_return std::unexpected(HubError::browser(browser::BrowserError::io(ec.message())));
    }
    auto launched = co_await browser::BrowserServer::launch(std::move(config));
    if (!launched) co_return std::unexpected(HubError::browser(launched.error()));
    std::lock_guard lock(mutex_);
    server_ = *launched;
    co_return std::move(*launched);
}

Result<std::shared_ptr<browser::Tab>> BrowserProvider::tab(const cs::SurfaceUri& uri) const {
    std::shared_ptr<browser::BrowserServer> server;
    {
        std::lock_guard lock(mutex_);
        server = server_;
    }
    if (!server) return std::unexpected(HubError::not_found(uri));
    auto tab = server->tab(uri);
    if (!tab) return std::unexpected(HubError::not_found(uri));
    return std::move(*tab);
}

Awaitable<Result<Opened>> BrowserProvider::open(OpenRequest req) {
    const auto* url = std::get_if<open_request::Url>(&req);
    if (!url) co_return std::unexpected(unsupported(name(), req));
    auto server = co_await this->server();
    if (!server) co_return std::unexpected(std::move(server.error()));
    auto tab = co_await (*server)->open_tab(normalize_url(url->url));
    if (!tab) co_return std::unexpected(HubError::browser(tab.error()));
    co_return Opened::make(std::move(*tab), Origin::browser());
}

Awaitable<Result<void>> BrowserProvider::close(cs::SurfaceUri uri) {
    std::shared_ptr<browser::BrowserServer> server;
    {
        std::lock_guard lock(mutex_);
        server = server_;
    }
    if (server) {
        if (auto closed = co_await server->close_tab(std::move(uri)); !closed) co_return std::unexpected(HubError::browser(closed.error()));
    }
    co_return Result<void>{};
}

Awaitable<void> BrowserProvider::shutdown() {
    std::shared_ptr<browser::BrowserServer> server;
    {
        std::lock_guard lock(mutex_);
        server = std::exchange(server_, nullptr);
    }
    if (server) {
        if (auto stopped = co_await server->shutdown(); !stopped) runtime::log::warn(LOG, "stopping the browser: " + stopped.error().message());
    }
}

// ---- docs ------------------------------------------------------------------

std::string_view DocsProvider::name() const { return docs::SERVER_NAME; }

Result<std::shared_ptr<docs::DocSurface>> DocsProvider::document(const cs::SurfaceUri& uri) const {
    auto surface = server_.surface(uri);
    if (!surface) return std::unexpected(HubError::not_found(uri));
    return std::move(*surface);
}

Awaitable<Result<Opened>> DocsProvider::open(OpenRequest req) {
    const auto* file = std::get_if<open_request::File>(&req);
    if (!file) co_return std::unexpected(unsupported(name(), req));
    auto surface = co_await server_.open_file(file->path);
    if (!surface) co_return std::unexpected(HubError::docs(surface.error()));
    co_return Opened::make(std::move(*surface), Origin::document());
}

Awaitable<Result<void>> DocsProvider::close(cs::SurfaceUri uri) {
    server_.close(uri);
    co_return Result<void>{};
}

// ---- vnc -------------------------------------------------------------------

VncProvider::VncProvider(vnc::RfbOptions options)
    : passwords_(std::make_shared<Passwords>()),
      server_({},
              vnc::password_fn([passwords = passwords_](std::string_view endpoint) -> std::optional<std::string> {
                  std::lock_guard lock(passwords->mutex);
                  const auto it = passwords->by_endpoint.find(std::string(endpoint));
                  if (it == passwords->by_endpoint.end()) return std::nullopt;
                  return it->second;
              }),
              options) {}

std::string_view VncProvider::name() const { return vnc::SERVER_NAME; }

Awaitable<std::vector<Listed>> VncProvider::list() {
    auto infos = co_await server_.list();
    std::vector<Listed> listed;
    if (infos) {
        for (auto& info : *infos) listed.push_back(Listed{std::move(info), Origin::vm(), false});
    }
    co_return listed;
}

Awaitable<Result<Opened>> VncProvider::open(OpenRequest req) {
    std::optional<cs::SurfaceUri> uri;
    if (const auto* listed = std::get_if<open_request::Uri>(&req)) {
        uri = listed->uri;
    } else if (const auto* vm = std::get_if<open_request::Vm>(&req)) {
        const std::string address(trim(vm->address));
        const auto colon = address.rfind(':');
        const std::string_view host = colon == std::string::npos ? std::string_view() : std::string_view(address).substr(0, colon);
        const std::string_view port = colon == std::string::npos ? std::string_view() : std::string_view(address).substr(colon + 1);
        if (host.empty() || host.find('/') != std::string_view::npos || !is_port(port)) {
            co_return std::unexpected(HubError::address(address));
        }
        {
            std::lock_guard lock(passwords_->mutex);
            if (vm->password && !vm->password->empty()) {
                passwords_->by_endpoint.insert_or_assign(address, *vm->password);
            } else {
                passwords_->by_endpoint.erase(address);
            }
        }
        auto added = server_.add_endpoint(vnc::RfbEndpoint{address, address});
        if (!added) co_return std::unexpected(HubError::surface(std::move(added.error())));
        uri = std::move(*added);
    } else {
        co_return std::unexpected(unsupported(name(), req));
    }
    auto surface = co_await server_.open_rfb(std::move(*uri));
    if (!surface) co_return std::unexpected(HubError::surface(std::move(surface.error())));
    co_return Opened::make(std::move(*surface), Origin::vm());
}

// ---- mcp -------------------------------------------------------------------

std::map<std::string, std::vector<cs::SurfaceInfo>> McpDeclared::declared() const {
    std::lock_guard lock(mutex_);
    return declared_;
}

Awaitable<std::vector<std::pair<std::string, std::string>>> McpDeclared::refresh(mcp::Manager manager) {
    {
        std::lock_guard lock(mutex_);
        manager_ = manager;
    }
    using Declared = std::map<std::string, std::vector<cs::SurfaceInfo>>;
    using Failed = std::vector<std::pair<std::string, std::string>>;
    auto [declared, failed] = co_await detail::off_thread<std::pair<Declared, Failed>>([manager = std::move(manager)] {
        Declared declared;
        Failed failed;
        for (auto& view : manager.view(mcp::AllowAll{})) {
            if (view.status.kind != mcp::Status::Kind::Running) continue;
            auto name = view.spec.name;
            auto surfaces = manager.surfaces(name);
            if (!surfaces) {
                failed.emplace_back(std::move(name), surfaces.error().to_string());
                continue;
            }
            std::vector<cs::SurfaceInfo> infos;
            for (auto& [uri, resource] : *surfaces) infos.push_back(declared_info(std::move(uri), resource));
            if (!infos.empty()) declared.emplace(std::move(name), std::move(infos));
        }
        return std::pair{std::move(declared), std::move(failed)};
    });
    {
        std::lock_guard lock(mutex_);
        declared_ = std::move(declared);
    }
    co_return failed;
}

std::vector<Listed> McpDeclared::listed() const {
    std::lock_guard lock(mutex_);
    std::vector<Listed> all;
    for (const auto& [server, infos] : declared_) {
        for (const auto& info : infos) all.push_back(Listed{info, Origin::mcp(server), false});
    }
    return all;
}

Awaitable<std::vector<Listed>> McpDeclared::list() { co_return listed(); }

Awaitable<Result<Opened>> McpDeclared::open(OpenRequest req) {
    const auto* wanted = std::get_if<open_request::Uri>(&req);
    if (!wanted) co_return std::unexpected(unsupported(name(), req));
    auto all = listed();
    const auto found = std::ranges::find_if(all, [&](const Listed& l) { return l.info.uri == wanted->uri; });
    if (found == all.end()) co_return std::unexpected(HubError::not_found(wanted->uri));
    const std::string server(found->origin.server());
    std::optional<mcp::Manager> manager;
    {
        std::lock_guard lock(mutex_);
        manager = manager_;
    }
    if (!manager) co_return std::unexpected(HubError::no_mcp());
    const auto executor = co_await asio::this_coro::executor;
    auto surface = std::make_shared<McpSurface>(std::move(*manager), server, found->info, executor);
    co_return Opened::make(std::move(surface), Origin::mcp(server));
}

cs::SurfaceInfo declared_info(cs::SurfaceUri uri, const mcp::model::Resource& resource) {
    if (auto parsed = parse_resource(resource.raw); parsed && *parsed) return std::move(**parsed);
    const auto* title = resource.raw.find("title");
    std::string name = title && title->as_string() ? *title->as_string() : resource.name;
    const auto mode = resource.mime_type ? cs::SurfaceMode::Document : cs::SurfaceMode::Pixel;
    return cs::SurfaceInfo{std::move(uri), std::move(name), 0, 0, mode, resource.mime_type};
}

} // namespace loa::surfaces::hub
