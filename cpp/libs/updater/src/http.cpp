#include "loa/updater/http.hpp"

#include <curl/curl.h>

#include <algorithm>
#include <charconv>
#include <memory>

namespace loa::updater {
namespace {

// The workspace version the Rust client reports in its user agent.
constexpr const char* USER_AGENT = "loa-updater/0.1.0";

std::string lower(std::string_view s) {
    std::string out(s);
    std::ranges::transform(out, out.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return out;
}

std::string_view trim(std::string_view s) {
    while (!s.empty() && (s.front() == ' ' || s.front() == '\t' || s.front() == '\r' || s.front() == '\n')) s.remove_prefix(1);
    while (!s.empty() && (s.back() == ' ' || s.back() == '\t' || s.back() == '\r' || s.back() == '\n')) s.remove_suffix(1);
    return s;
}

void global_init() {
    static const bool done = [] {
        curl_global_init(CURL_GLOBAL_DEFAULT);
        return true;
    }();
    (void)done;
}

struct Transfer {
    CURL* curl = nullptr;
    const Http::OnHead* on_head = nullptr;
    const Http::OnChunk* on_chunk = nullptr;
    HttpHeaders headers;
    bool head_done = false;
    std::optional<Error> error;

    // The status line of each response (redirects included) starts a fresh header set.
    static size_t header(char* data, size_t size, size_t n, void* self_ptr) {
        auto& self = *static_cast<Transfer*>(self_ptr);
        const std::string_view line(data, size * n);
        if (line.starts_with("HTTP/")) {
            self.headers.entries.clear();
            self.head_done = false;
        } else if (const auto colon = line.find(':'); colon != std::string_view::npos) {
            self.headers.entries.emplace_back(lower(trim(line.substr(0, colon))), std::string(trim(line.substr(colon + 1))));
        }
        return size * n;
    }

    // Runs `on_head` once, before the first body byte or at the end of a bodiless response.
    bool head() {
        if (head_done) return !error;
        head_done = true;
        long status = 0;
        curl_easy_getinfo(curl, CURLINFO_RESPONSE_CODE, &status);
        curl_off_t length = -1;
        curl_easy_getinfo(curl, CURLINFO_CONTENT_LENGTH_DOWNLOAD_T, &length);
        std::optional<std::uint64_t> content_length;
        if (length >= 0) content_length = static_cast<std::uint64_t>(length);
        if (auto e = (*on_head)(status, headers, content_length)) error = std::move(*e);
        return !error;
    }

    static size_t write(char* data, size_t size, size_t n, void* self_ptr) {
        auto& self = *static_cast<Transfer*>(self_ptr);
        long status = 0;
        curl_easy_getinfo(self.curl, CURLINFO_RESPONSE_CODE, &status);
        // libcurl calls this for redirect response bodies too. Ignore those bytes and
        // report only the final response to the caller (particularly for downloads).
        if (status < 200 || (status >= 300 && status < 400 && self.headers.get("location"))) return size * n;
        if (!self.head()) return 0;
        if (auto e = (*self.on_chunk)(std::span<const char>(data, size * n))) {
            self.error = std::move(*e);
            return 0;
        }
        return size * n;
    }
};

} // namespace

std::optional<std::string> HttpHeaders::get(std::string_view name) const {
    for (const auto& [k, v] : entries)
        if (k == name) return v;
    return std::nullopt;
}

std::optional<std::uint64_t> HttpHeaders::number(std::string_view name) const {
    const auto v = get(name);
    if (!v) return std::nullopt;
    const auto text = trim(*v);
    std::uint64_t out = 0;
    const auto [ptr, ec] = std::from_chars(text.data(), text.data() + text.size(), out);
    if (ec != std::errc{} || ptr != text.data() + text.size() || text.empty()) return std::nullopt;
    return out;
}

Http::Http() { global_init(); }

Result<> Http::stream(const std::string& url, const RequestHeaders& headers, const OnHead& on_head, const OnChunk& on_chunk) const {
    std::unique_ptr<CURL, decltype(&curl_easy_cleanup)> curl(curl_easy_init(), &curl_easy_cleanup);
    if (!curl) return std::unexpected(Error::http(url, "cannot create a curl handle"));
    curl_slist* list = nullptr;
    for (const auto& [k, v] : headers) list = curl_slist_append(list, (k + ": " + v).c_str());
    std::unique_ptr<curl_slist, decltype(&curl_slist_free_all)> list_guard(list, &curl_slist_free_all);
    Transfer t;
    t.curl = curl.get();
    t.on_head = &on_head;
    t.on_chunk = &on_chunk;
    char message[CURL_ERROR_SIZE] = {};
    CURL* h = curl.get();
    curl_easy_setopt(h, CURLOPT_URL, url.c_str());
    curl_easy_setopt(h, CURLOPT_HTTPHEADER, list);
    curl_easy_setopt(h, CURLOPT_USERAGENT, USER_AGENT);
    curl_easy_setopt(h, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(h, CURLOPT_MAXREDIRS, 10L);
    curl_easy_setopt(h, CURLOPT_CONNECTTIMEOUT, 15L);
    // reqwest's 60 s read timeout: abort when nothing arrives for a minute.
    curl_easy_setopt(h, CURLOPT_LOW_SPEED_LIMIT, 1L);
    curl_easy_setopt(h, CURLOPT_LOW_SPEED_TIME, 60L);
    curl_easy_setopt(h, CURLOPT_NOSIGNAL, 1L);
    // Like the Rust client: no Accept-Encoding, so artifacts arrive (and hash) as published.
    curl_easy_setopt(h, CURLOPT_HEADERFUNCTION, &Transfer::header);
    curl_easy_setopt(h, CURLOPT_HEADERDATA, &t);
    curl_easy_setopt(h, CURLOPT_WRITEFUNCTION, &Transfer::write);
    curl_easy_setopt(h, CURLOPT_WRITEDATA, &t);
    curl_easy_setopt(h, CURLOPT_ERRORBUFFER, message);
    const auto code = curl_easy_perform(h);
    if (t.error) return std::unexpected(std::move(*t.error));
    if (code != CURLE_OK) return std::unexpected(Error::http(url, message[0] ? message : curl_easy_strerror(code)));
    if (!t.head()) return std::unexpected(std::move(*t.error));
    return {};
}

Result<HttpResponse> Http::get(const std::string& url, const RequestHeaders& headers) const {
    HttpResponse response;
    const OnHead on_head = [&](long status, const HttpHeaders& h, std::optional<std::uint64_t>) -> std::optional<Error> {
        response.status = status;
        response.headers = h;
        return std::nullopt;
    };
    const OnChunk on_chunk = [&](std::span<const char> chunk) -> std::optional<Error> {
        response.body.append(chunk.data(), chunk.size());
        return std::nullopt;
    };
    if (auto r = stream(url, headers, on_head, on_chunk); !r) return std::unexpected(r.error());
    return response;
}

} // namespace loa::updater
