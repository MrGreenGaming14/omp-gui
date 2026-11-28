// Streamable HTTP transport over libcurl: a POST per message (JSON or SSE reply), a GET stream
// for server notifications, the session closed with a DELETE. No redirects: one would replay the
// configured headers.

#include <curl/curl.h>

#include <algorithm>
#include <atomic>
#include <cctype>
#include <condition_variable>
#include <mutex>
#include <thread>

#include "connect.hpp"

namespace loa::mcp::detail {

namespace {

std::string lower(std::string_view s) {
    std::string out(s);
    std::ranges::transform(out, out.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return out;
}

std::string trim(std::string_view s) {
    const auto first = s.find_first_not_of(" \t\r\n");
    if (first == std::string_view::npos) return {};
    return std::string(s.substr(first, s.find_last_not_of(" \t\r\n") - first + 1));
}

// Incremental `text/event-stream` parser: hands each event's data (and remembers its id).
class SseParser {
  public:
    explicit SseParser(std::function<void(std::string_view data, const std::string& id)> on_event) : on_event_(std::move(on_event)) {}

    void feed(std::string_view chunk) {
        buffer_.append(chunk);
        std::size_t start = 0;
        for (auto nl = buffer_.find('\n', start); nl != std::string::npos; nl = buffer_.find('\n', start)) {
            std::string_view line(buffer_.data() + start, nl - start);
            if (!line.empty() && line.back() == '\r') line.remove_suffix(1);
            this->line(line);
            start = nl + 1;
        }
        buffer_.erase(0, start);
    }

    void finish() {
        if (!buffer_.empty()) line(std::exchange(buffer_, {}));
        line("");
    }

  private:
    void line(std::string_view line) {
        if (line.empty()) {
            if (has_data_) on_event_(data_, id_);
            data_.clear();
            has_data_ = false;
            return;
        }
        if (line.front() == ':') return;
        const auto colon = line.find(':');
        const auto field = line.substr(0, colon);
        std::string_view value = colon == std::string_view::npos ? std::string_view() : line.substr(colon + 1);
        if (!value.empty() && value.front() == ' ') value.remove_prefix(1);
        if (field == "data") {
            if (has_data_) data_.push_back('\n');
            data_.append(value);
            has_data_ = true;
        } else if (field == "id") {
            id_ = std::string(value);
        }
    }

    std::function<void(std::string_view, const std::string&)> on_event_;
    std::string buffer_;
    std::string data_;
    std::string id_;
    bool has_data_ = false;
};

struct Exchange {
    long status = 0;
    std::string content_type;
    std::string session_id;
    std::string body;
    std::optional<SseParser> sse;
    // Receives every SSE event; none for a request whose reply is ignored.
    std::function<void(std::string_view, const std::string&)> on_event;
    std::function<bool()> abort;
    bool started = false;
};

size_t on_header(char* data, size_t size, size_t count, void* user) {
    auto* exchange = static_cast<Exchange*>(user);
    std::string_view line(data, size * count);
    const auto colon = line.find(':');
    if (colon != std::string_view::npos) {
        const auto name = lower(trim(line.substr(0, colon)));
        if (name == "content-type") exchange->content_type = lower(trim(line.substr(colon + 1)));
        else if (name == "mcp-session-id") exchange->session_id = trim(line.substr(colon + 1));
    }
    return size * count;
}

size_t on_body(char* data, size_t size, size_t count, void* user) {
    auto* exchange = static_cast<Exchange*>(user);
    std::string_view chunk(data, size * count);
    if (!exchange->started) {
        exchange->started = true;
        if (exchange->on_event && exchange->content_type.starts_with("text/event-stream")) exchange->sse.emplace(exchange->on_event);
    }
    if (exchange->sse) exchange->sse->feed(chunk);
    else exchange->body.append(chunk);
    return size * count;
}

int on_progress(void* user, curl_off_t, curl_off_t, curl_off_t, curl_off_t) {
    auto* exchange = static_cast<Exchange*>(user);
    return exchange->abort && exchange->abort() ? 1 : 0;
}

struct SlistDeleter {
    void operator()(curl_slist* list) const noexcept { curl_slist_free_all(list); }
};
struct EasyDeleter {
    void operator()(CURL* easy) const noexcept { curl_easy_cleanup(easy); }
};

class HttpTransport final : public Transport {
  public:
    HttpTransport(HttpSpec spec, Sink sink) : spec_(std::move(spec)), sink_(std::move(sink)) {}
    ~HttpTransport() override { close(); }

    bool synchronous() const noexcept override { return true; }

    void initialized(std::string version) override {
        {
            std::lock_guard lock(mutex_);
            protocol_version_ = std::move(version);
        }
        listener_ = std::jthread([this](std::stop_token stop) { listen(stop); });
    }

    std::expected<void, std::string> send(const std::string& message, const Cancel& cancel) override {
        if (closing_) return std::unexpected(std::string("Transport closed"));
        Exchange exchange;
        exchange.abort = [&] { return closing_.load() || cancel.expired(); };
        std::unique_ptr<CURL, EasyDeleter> easy(curl_easy_init());
        if (!easy) return std::unexpected(std::string("curl_easy_init failed"));
        auto headers = base_headers("application/json, text/event-stream");
        headers.reset(curl_slist_append(headers.release(), "Content-Type: application/json"));
        configure(easy.get(), headers.get(), exchange);
        curl_easy_setopt(easy.get(), CURLOPT_POST, 1L);
        curl_easy_setopt(easy.get(), CURLOPT_POSTFIELDS, message.c_str());
        curl_easy_setopt(easy.get(), CURLOPT_POSTFIELDSIZE_LARGE, static_cast<curl_off_t>(message.size()));
        // SSE replies are parsed as they stream; a JSON reply is handed over whole.
        exchange.on_event = [this](std::string_view data, const std::string&) { sink_.message(data); };
        const auto rc = curl_easy_perform(easy.get());
        if (rc != CURLE_OK) return std::unexpected(std::string("Transport send error: ") + curl_easy_strerror(rc));
        curl_easy_getinfo(easy.get(), CURLINFO_RESPONSE_CODE, &exchange.status);
        if (exchange.status < 200 || exchange.status >= 300)
            return std::unexpected("Transport send error: unexpected server response: HTTP status " + std::to_string(exchange.status));
        if (!exchange.session_id.empty()) {
            std::lock_guard lock(mutex_);
            session_id_ = exchange.session_id;
        }
        if (exchange.sse) exchange.sse->finish();
        else if (!trim(exchange.body).empty()) sink_.message(exchange.body);
        return {};
    }

    void close() override {
        if (closing_.exchange(true)) return;
        if (listener_.joinable()) {
            listener_.request_stop();
            listener_.join();
        }
        std::string session;
        {
            std::lock_guard lock(mutex_);
            session = session_id_;
        }
        if (!session.empty()) {
            // Best effort: end the session on the server.
            Exchange exchange;
            std::unique_ptr<CURL, EasyDeleter> easy(curl_easy_init());
            if (easy) {
                auto headers = base_headers("application/json, text/event-stream");
                configure(easy.get(), headers.get(), exchange);
                curl_easy_setopt(easy.get(), CURLOPT_CUSTOMREQUEST, "DELETE");
                curl_easy_setopt(easy.get(), CURLOPT_TIMEOUT_MS, 2000L);
                exchange.abort = nullptr;
                (void)curl_easy_perform(easy.get());
            }
        }
        sink_.closed();
    }

  private:
    std::unique_ptr<curl_slist, SlistDeleter> base_headers(std::string_view accept) {
        curl_slist* list = nullptr;
        // User headers first; the four the client owns were refused by `HttpSpec::validate`.
        for (const auto& [name, value] : spec_.headers) list = curl_slist_append(list, (name + ": " + value).c_str());
        list = curl_slist_append(list, ("Accept: " + std::string(accept)).c_str());
        std::lock_guard lock(mutex_);
        if (!session_id_.empty()) list = curl_slist_append(list, ("Mcp-Session-Id: " + session_id_).c_str());
        if (!protocol_version_.empty()) list = curl_slist_append(list, ("Mcp-Protocol-Version: " + protocol_version_).c_str());
        if (!last_event_id_.empty()) list = curl_slist_append(list, ("Last-Event-Id: " + last_event_id_).c_str());
        return std::unique_ptr<curl_slist, SlistDeleter>(list);
    }

    void configure(CURL* easy, curl_slist* headers, Exchange& exchange) {
        curl_easy_setopt(easy, CURLOPT_URL, spec_.url.c_str());
        curl_easy_setopt(easy, CURLOPT_HTTPHEADER, headers);
        curl_easy_setopt(easy, CURLOPT_FOLLOWLOCATION, 0L);
        curl_easy_setopt(easy, CURLOPT_NOSIGNAL, 1L);
        curl_easy_setopt(easy, CURLOPT_FORBID_REUSE, 1L);
        curl_easy_setopt(easy, CURLOPT_HEADERFUNCTION, &on_header);
        curl_easy_setopt(easy, CURLOPT_HEADERDATA, &exchange);
        curl_easy_setopt(easy, CURLOPT_WRITEFUNCTION, &on_body);
        curl_easy_setopt(easy, CURLOPT_WRITEDATA, &exchange);
        curl_easy_setopt(easy, CURLOPT_NOPROGRESS, 0L);
        curl_easy_setopt(easy, CURLOPT_XFERINFOFUNCTION, &on_progress);
        curl_easy_setopt(easy, CURLOPT_XFERINFODATA, &exchange);
    }

    // The GET stream carrying server notifications; resumes with `Last-Event-Id` after a drop.
    // A server without one (405) ends it.
    void listen(std::stop_token stop) {
        while (!stop.stop_requested() && !closing_) {
            Exchange exchange;
            exchange.abort = [&] { return stop.stop_requested() || closing_.load(); };
            exchange.on_event = [this](std::string_view data, const std::string& id) {
                if (!id.empty()) {
                    std::lock_guard lock(mutex_);
                    last_event_id_ = id;
                }
                sink_.message(data);
            };
            std::unique_ptr<CURL, EasyDeleter> easy(curl_easy_init());
            if (!easy) return;
            auto headers = base_headers("text/event-stream");
            configure(easy.get(), headers.get(), exchange);
            curl_easy_setopt(easy.get(), CURLOPT_HTTPGET, 1L);
            (void)curl_easy_perform(easy.get());
            curl_easy_getinfo(easy.get(), CURLINFO_RESPONSE_CODE, &exchange.status);
            if (exchange.status == 405 || exchange.status == 404) return;
            std::mutex m;
            std::unique_lock lock(m);
            std::condition_variable_any().wait_for(lock, stop, std::chrono::seconds(1), [] { return false; });
        }
    }

    HttpSpec spec_;
    Sink sink_;
    std::mutex mutex_;
    std::string session_id_;
    std::string protocol_version_;
    std::string last_event_id_;
    std::atomic<bool> closing_{false};
    std::jthread listener_;
};

} // namespace

std::expected<void, std::string> init_http() {
    static const CURLcode rc = curl_global_init(CURL_GLOBAL_DEFAULT);
    if (rc != CURLE_OK) return std::unexpected(std::string(curl_easy_strerror(rc)));
    return {};
}

std::unique_ptr<Transport> open_http(const HttpSpec& spec, Sink sink) { return std::make_unique<HttpTransport>(spec, std::move(sink)); }

} // namespace loa::mcp::detail
