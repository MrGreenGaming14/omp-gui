#include "httpd.hpp"

#include <httplib.h>

#include <algorithm>
#include <cctype>
#include <thread>

namespace loa::mcp::detail {

namespace {

bool iequals(std::string_view a, std::string_view b) {
    return std::ranges::equal(a, b, [](char x, char y) {
        return std::tolower(static_cast<unsigned char>(x)) == std::tolower(static_cast<unsigned char>(y));
    });
}

} // namespace

const std::string* HttpRequest::header(std::string_view name) const {
    for (const auto& [key, value] : headers) {
        if (iequals(key, name)) return &value;
    }
    return nullptr;
}

struct HttpServer::Impl {
    httplib::Server server;
    std::uint16_t port = 0;
    std::thread thread;
};

HttpServer::HttpServer(std::unique_ptr<Impl> impl) : impl_(std::move(impl)) {}

HttpServer::~HttpServer() { stop(); }

std::uint16_t HttpServer::port() const noexcept { return impl_->port; }

void HttpServer::stop() {
    impl_->server.stop();
    if (impl_->thread.joinable()) impl_->thread.join();
}

std::expected<std::unique_ptr<HttpServer>, std::string> HttpServer::start(const std::string& host, std::uint16_t port,
                                                                          HttpHandler handler) {
    auto impl = std::make_unique<Impl>();
    auto serve = [handler = std::move(handler)](const httplib::Request& req, httplib::Response& res) {
        HttpRequest request{req.method, req.path, {}, req.body};
        for (const auto& [key, value] : req.headers) request.headers.emplace_back(key, value);
        auto response = handler(request);
        res.status = response.status;
        for (auto& [key, value] : response.headers) res.set_header(key, value);
        if (!response.content_type.empty()) res.set_content(std::move(response.body), response.content_type);
    };
    // Catch-all routes (a pre-routing handler would run before the body is read).
    const std::string any = ".*";
    impl->server.Get(any, serve).Post(any, serve).Put(any, serve).Delete(any, serve).Patch(any, serve).Options(any, serve);
    const int bound = port == 0 ? impl->server.bind_to_any_port(host) : (impl->server.bind_to_port(host, port) ? port : -1);
    if (bound <= 0) return std::unexpected("cannot bind " + host + ":" + std::to_string(port));
    impl->port = static_cast<std::uint16_t>(bound);
    auto* server = &impl->server;
    impl->thread = std::thread([server] { server->listen_after_bind(); });
    impl->server.wait_until_ready();
    return std::unique_ptr<HttpServer>(new HttpServer(std::move(impl)));
}

} // namespace loa::mcp::detail
