#include "loa/harness/opencode/client.hpp"

#include <curl/curl.h>

#include <atomic>
#include <deque>
#include <mutex>
#include <utility>
#include <vector>

namespace loa::harness::opencode {
namespace {
using loa::contracts::AdapterError;
template <typename T> using Result = loa::contracts::Result<T>;

void curl_init() {
    static const bool initialized = [] { return curl_global_init(CURL_GLOBAL_DEFAULT) == CURLE_OK; }();
    (void)initialized;
}

struct Easy {
    CURL* handle = nullptr;
    curl_slist* headers = nullptr;
    Easy() { curl_init(); handle = curl_easy_init(); }
    ~Easy() { if (handle) curl_easy_cleanup(handle); if (headers) curl_slist_free_all(headers); }
    Easy(const Easy&) = delete;
    Easy& operator=(const Easy&) = delete;
};

size_t write_body(char* bytes, size_t size, size_t count, void* data) {
    auto* body = static_cast<std::string*>(data);
    body->append(bytes, size * count);
    return size * count;
}

void configure(Easy& easy, std::string_view url, std::string_view directory,
               const std::optional<ServerAuth>& auth) {
    curl_easy_setopt(easy.handle, CURLOPT_URL, std::string(url).c_str());
    curl_easy_setopt(easy.handle, CURLOPT_NOSIGNAL, 1L);
    curl_easy_setopt(easy.handle, CURLOPT_FOLLOWLOCATION, 0L);
    curl_easy_setopt(easy.handle, CURLOPT_CONNECTTIMEOUT_MS, 5000L);
    easy.headers = curl_slist_append(easy.headers, ("x-opencode-directory: " + std::string(directory)).c_str());
    if (auth) {
        curl_easy_setopt(easy.handle, CURLOPT_HTTPAUTH, CURLAUTH_BASIC);
        curl_easy_setopt(easy.handle, CURLOPT_USERNAME, auth->username.c_str());
        curl_easy_setopt(easy.handle, CURLOPT_PASSWORD, auth->password.c_str());
    }
    curl_easy_setopt(easy.handle, CURLOPT_HTTPHEADER, easy.headers);
}

AdapterError protocol(std::string message) { return AdapterError(AdapterError::Protocol{std::move(message)}); }
std::string url_for(const std::string& base, std::string_view path) { return base + std::string(path); }
} // namespace

Client::Client(std::string base, std::string directory, std::optional<ServerAuth> auth)
    : base_(std::move(base)), directory_(std::move(directory)), auth_(std::move(auth)) {
    while (!base_.empty() && base_.back() == '/') base_.pop_back();
}

Result<json::Value> Client::request(std::string_view method, std::string_view path,
                                    std::optional<json::Value> body) const {
    Easy easy;
    const std::string what = std::string(method) + " " + std::string(path);
    if (!easy.handle) return std::unexpected(protocol(what + ": curl initialization failed"));
    const auto url = url_for(base_, path);
    configure(easy, url, directory_, auth_);
    const std::string verb(method);
    curl_easy_setopt(easy.handle, CURLOPT_CUSTOMREQUEST, verb.c_str());
    std::string payload;
    if (body) {
        payload = json::dump(*body);
        easy.headers = curl_slist_append(easy.headers, "Content-Type: application/json");
        curl_easy_setopt(easy.handle, CURLOPT_HTTPHEADER, easy.headers);
        curl_easy_setopt(easy.handle, CURLOPT_POSTFIELDS, payload.c_str());
        curl_easy_setopt(easy.handle, CURLOPT_POSTFIELDSIZE_LARGE, static_cast<curl_off_t>(payload.size()));
    }
    std::string response;
    curl_easy_setopt(easy.handle, CURLOPT_WRITEFUNCTION, write_body);
    curl_easy_setopt(easy.handle, CURLOPT_WRITEDATA, &response);
    const auto result = curl_easy_perform(easy.handle);
    if (result != CURLE_OK) return std::unexpected(protocol(what + ": " + curl_easy_strerror(result)));
    long status = 0;
    curl_easy_getinfo(easy.handle, CURLINFO_RESPONSE_CODE, &status);
    if (status < 200 || status >= 300)
        return std::unexpected(protocol(what + ": HTTP " + std::to_string(status) + ": " + response));
    if (response.empty()) return json::Value{};
    auto parsed = json::parse(response);
    if (!parsed) return std::unexpected(protocol(std::string(path) + ": " + parsed.error().message()));
    return std::move(*parsed);
}

Result<std::pair<long, json::Value>> Client::probe(std::string_view path, std::chrono::milliseconds timeout) const {
    Easy easy;
    if (!easy.handle) return std::unexpected(protocol("probe: curl initialization failed"));
    const auto url = url_for(base_, path);
    configure(easy, url, directory_, auth_);
    curl_easy_setopt(easy.handle, CURLOPT_TIMEOUT_MS, static_cast<long>(timeout.count()));
    std::string response;
    curl_easy_setopt(easy.handle, CURLOPT_WRITEFUNCTION, write_body);
    curl_easy_setopt(easy.handle, CURLOPT_WRITEDATA, &response);
    const auto result = curl_easy_perform(easy.handle);
    if (result != CURLE_OK) return std::unexpected(protocol(std::string(path) + ": " + curl_easy_strerror(result)));
    long status = 0;
    curl_easy_getinfo(easy.handle, CURLINFO_RESPONSE_CODE, &status);
    auto parsed = json::parse(response);
    return std::pair{status, parsed ? std::move(*parsed) : json::Value{}};
}

struct EventStream::Impl {
    Easy easy;
    CURLM* multi = nullptr;
    std::atomic<bool> stopped{false};
    SseDecoder decoder;
    std::deque<std::string> ready;
    std::string error_body;
    bool finished = false;
    CURLcode transfer = CURLE_OK;
    explicit Impl(std::string_view url, std::string_view directory, const std::optional<ServerAuth>& auth) {
        if (!easy.handle) return;
        configure(easy, url, directory, auth);
        easy.headers = curl_slist_append(easy.headers, "Accept: text/event-stream");
        curl_easy_setopt(easy.handle, CURLOPT_HTTPHEADER, easy.headers);
        curl_easy_setopt(easy.handle, CURLOPT_WRITEFUNCTION, +[](char* bytes, size_t size, size_t count, void* context) -> size_t {
            auto& self = *static_cast<Impl*>(context);
            const auto length = size * count;
            long status = 0;
            curl_easy_getinfo(self.easy.handle, CURLINFO_RESPONSE_CODE, &status);
            if (status >= 300) self.error_body.append(bytes, length);
            else {
                std::vector<std::string> decoded;
                self.decoder.feed(std::string_view(bytes, length), decoded);
                for (auto& data : decoded) self.ready.push_back(std::move(data));
            }
            return length;
        });
        curl_easy_setopt(easy.handle, CURLOPT_WRITEDATA, this);
        multi = curl_multi_init();
        if (multi && curl_multi_add_handle(multi, easy.handle) != CURLM_OK) {
            curl_multi_cleanup(multi);
            multi = nullptr;
        }
    }
    ~Impl() {
        if (multi) { curl_multi_remove_handle(multi, easy.handle); curl_multi_cleanup(multi); }
    }
};

EventStream::EventStream(std::unique_ptr<Impl> impl) : impl_(std::move(impl)) {}
EventStream::~EventStream() = default;
EventStream::EventStream(EventStream&&) noexcept = default;
EventStream& EventStream::operator=(EventStream&&) noexcept = default;
void EventStream::stop() noexcept {
    if (!impl_) return;
    impl_->stopped.store(true, std::memory_order_release);
    if (impl_->multi) curl_multi_wakeup(impl_->multi);
}

Result<std::optional<Event>> EventStream::next() {
    if (!impl_) return std::unexpected(protocol("event stream: moved-from stream"));
    auto& s = *impl_;
    while (!s.stopped.load(std::memory_order_acquire)) {
        while (!s.ready.empty()) {
            auto data = std::move(s.ready.front());
            s.ready.pop_front();
            auto parsed = json::parse(data);
            if (parsed) if (auto event = parse_event(*parsed)) return event;
            // As in the Rust stream, malformed event payloads are skipped.
        }
        if (s.finished) {
            long status = 0;
            curl_easy_getinfo(s.easy.handle, CURLINFO_RESPONSE_CODE, &status);
            if (status < 200 || status >= 300)
                return std::unexpected(protocol("GET /event: HTTP " + std::to_string(status) + ": " + s.error_body));
            if (s.transfer != CURLE_OK)
                return std::unexpected(protocol(std::string("event stream: ") + curl_easy_strerror(s.transfer)));
            return std::optional<Event>{};
        }
        int active = 0;
        const auto running = curl_multi_perform(s.multi, &active);
        if (running != CURLM_OK) return std::unexpected(protocol(std::string("event stream: ") + curl_multi_strerror(running)));
        int messages = 0;
        while (auto* message = curl_multi_info_read(s.multi, &messages)) {
            if (message->msg == CURLMSG_DONE) { s.finished = true; s.transfer = message->data.result; }
        }
        if (!s.ready.empty() || s.finished) continue;
        int fds = 0;
        const auto polled = curl_multi_poll(s.multi, nullptr, 0, 100, &fds);
        if (polled != CURLM_OK) return std::unexpected(protocol(std::string("event stream: ") + curl_multi_strerror(polled)));
    }
    return std::optional<Event>{};
}

Result<EventStream> Client::events() const {
    auto stream = std::make_unique<EventStream::Impl>(url_for(base_, "/event"), directory_, auth_);
    if (!stream->multi) return std::unexpected(protocol("GET /event: curl initialization failed"));
    return EventStream(std::move(stream));
}

} // namespace loa::harness::opencode
