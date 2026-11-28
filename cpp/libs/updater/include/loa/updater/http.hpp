#pragma once

// The updater's HTTP client: libcurl with the system CA store, following redirects to
// release CDNs. Calls block; one client may be used from several threads.

#include <cstdint>
#include <functional>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/updater/error.hpp"

namespace loa::updater {

// Response headers with lowercase names.
struct HttpHeaders {
    std::vector<std::pair<std::string, std::string>> entries;
    // The first value of `name` (lowercase), trimmed.
    [[nodiscard]] std::optional<std::string> get(std::string_view name) const;
    // `get` parsed as an unsigned integer.
    [[nodiscard]] std::optional<std::uint64_t> number(std::string_view name) const;
};

struct HttpResponse {
    long status = 0;
    HttpHeaders headers;
    std::string body;
};

class Http {
  public:
    using RequestHeaders = std::vector<std::pair<std::string, std::string>>;
    // Called once the final response's headers arrived; an error aborts the transfer.
    using OnHead = std::function<std::optional<Error>(long status, const HttpHeaders& headers, std::optional<std::uint64_t> content_length)>;
    // Called per body chunk; an error aborts the transfer.
    using OnChunk = std::function<std::optional<Error>(std::span<const char> chunk)>;

    // `user_agent` defaults to `loa-updater/<version>`.
    Http();

    // A GET whose whole body is kept.
    [[nodiscard]] Result<HttpResponse> get(const std::string& url, const RequestHeaders& headers = {}) const;
    // A GET streamed through `on_head` and `on_chunk`; the first error they return is the result.
    [[nodiscard]] Result<> stream(const std::string& url, const RequestHeaders& headers, const OnHead& on_head, const OnChunk& on_chunk) const;
};

} // namespace loa::updater
