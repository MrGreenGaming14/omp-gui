#pragma once

// A minimal HTTP/1.1 server on cpp-httplib, private to loa::mcp: the toolkit's streamable HTTP
// transport and the omp bridge's scripted provider. `httpd.cpp` is the one TU including httplib.

#include <cstdint>
#include <expected>
#include <functional>
#include <memory>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace loa::mcp::detail {

struct HttpRequest {
    std::string method;
    std::string path;
    std::vector<std::pair<std::string, std::string>> headers;
    std::string body;

    // The first header named `name` (case-insensitive).
    [[nodiscard]] const std::string* header(std::string_view name) const;
};

struct HttpResponse {
    int status = 200;
    std::vector<std::pair<std::string, std::string>> headers;
    std::string body;
    // Empty: no body is sent.
    std::string content_type;
};

using HttpHandler = std::function<HttpResponse(const HttpRequest&)>;

// Serves every method and path through `handler` on its own threads; stops when destroyed.
class HttpServer {
  public:
    // Binds `host:port` (`0` picks a free port) and starts listening.
    [[nodiscard]] static std::expected<std::unique_ptr<HttpServer>, std::string> start(const std::string& host, std::uint16_t port,
                                                                                       HttpHandler handler);
    ~HttpServer();
    HttpServer(const HttpServer&) = delete;
    HttpServer& operator=(const HttpServer&) = delete;

    [[nodiscard]] std::uint16_t port() const noexcept;
    void stop();

  private:
    struct Impl;
    explicit HttpServer(std::unique_ptr<Impl> impl);
    std::unique_ptr<Impl> impl_;
};

} // namespace loa::mcp::detail
