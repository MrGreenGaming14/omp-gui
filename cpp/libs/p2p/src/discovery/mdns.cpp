// The mDNS adapter: announces this node and browses for others on the LAN
// (Rust `loa_p2p::discovery::mdns`, on mjansson/mdns instead of mdns-sd).

#include "loa/p2p/discovery.hpp"

#include <algorithm>
#include <array>
#include <cerrno>
#include <charconv>
#include <chrono>
#include <condition_variable>
#include <cstring>
#include <deque>
#include <map>
#include <mutex>
#include <set>
#include <stop_token>
#include <thread>
#include <tuple>

#include <arpa/inet.h>
#include <ifaddrs.h>
#include <net/if.h>
#include <netinet/in.h>
#include <poll.h>
#include <sys/socket.h>

#include <mdns.h>

namespace loa::p2p {
namespace {

constexpr std::string_view KEY = "node";
constexpr std::string_view TCP = "tcp";
constexpr std::string_view NAME = "name";
// DNS labels hold 63 bytes: labels carry a key prefix, TXT the whole key.
constexpr std::size_t LABEL_KEY_CHARS = 16;
constexpr std::uint32_t TTL = 120;
constexpr auto BROWSE_EVERY = std::chrono::seconds(10);
constexpr int POLL_MS = 200;
constexpr std::size_t BUFFER_WORDS = 2048; // 8 KiB, 32-bit aligned as mdns.h requires

// An IP ordered like Rust `IpAddr`: every V4 before every V6, then by octets.
struct Ip {
    bool v6 = false;
    std::array<std::uint8_t, 16> bytes{};
    friend auto operator<=>(const Ip&, const Ip&) = default;

    static std::optional<Ip> parse(const std::string& text) {
        Ip ip;
        if (inet_pton(AF_INET, text.c_str(), ip.bytes.data()) == 1) return ip;
        ip.v6 = true;
        if (inet_pton(AF_INET6, text.c_str(), ip.bytes.data()) == 1) return ip;
        return std::nullopt;
    }

    [[nodiscard]] std::string to_string() const {
        char text[INET6_ADDRSTRLEN] = {};
        inet_ntop(v6 ? AF_INET6 : AF_INET, bytes.data(), text, sizeof text);
        return text;
    }

    [[nodiscard]] SocketAddr with_port(std::uint16_t port) const {
        auto host = to_string();
        return (v6 ? "[" + host + "]" : host) + ":" + std::to_string(port);
    }
};

std::optional<std::uint16_t> parse_port(std::string_view text) {
    std::uint16_t port = 0;
    auto [end, ec] = std::from_chars(text.data(), text.data() + text.size(), port);
    if (ec != std::errc{} || end != text.data() + text.size() || port == 0) return std::nullopt;
    return port;
}

bool iequals(std::string_view a, std::string_view b) {
    return std::ranges::equal(a, b, [](char x, char y) {
        auto lower = [](char c) { return c >= 'A' && c <= 'Z' ? static_cast<char>(c - 'A' + 'a') : c; };
        return lower(x) == lower(y);
    });
}

std::string lowered(std::string text) {
    for (auto& c : text)
        if (c >= 'A' && c <= 'Z') c = static_cast<char>(c - 'A' + 'a');
    return text;
}

mdns_string_t view(const std::string& text) { return {text.data(), text.size()}; }

// The non-loopback addresses of this machine's interfaces that are up.
void local_addresses(std::vector<sockaddr_in>& v4, std::vector<sockaddr_in6>& v6) {
    v4.clear();
    v6.clear();
    ifaddrs* list = nullptr;
    if (getifaddrs(&list) != 0) return;
    for (auto* it = list; it; it = it->ifa_next) {
        if (!it->ifa_addr || !(it->ifa_flags & IFF_UP) || (it->ifa_flags & IFF_LOOPBACK)) continue;
        if (it->ifa_addr->sa_family == AF_INET) {
            sockaddr_in addr;
            std::memcpy(&addr, it->ifa_addr, sizeof addr);
            if (addr.sin_addr.s_addr != htonl(INADDR_LOOPBACK)) v4.push_back(addr);
        } else if (it->ifa_addr->sa_family == AF_INET6) {
            sockaddr_in6 addr;
            std::memcpy(&addr, it->ifa_addr, sizeof addr);
            if (!IN6_IS_ADDR_LOOPBACK(&addr.sin6_addr) && !IN6_IS_ADDR_V4MAPPED(&addr.sin6_addr)) v6.push_back(addr);
        }
    }
    freeifaddrs(list);
}

// What a browse has heard about one instance so far.
struct Heard {
    std::optional<std::uint16_t> port;
    std::string target;
    std::optional<std::vector<std::pair<std::string, std::string>>> txt;
};

} // namespace

// ---- record and parsing ----

const std::string* ServiceRecord::property(std::string_view key) const {
    for (const auto& [k, v] : txt)
        if (k == key) return &v;
    return nullptr;
}

Result<ServiceRecord> Announce::info() const {
    if (udp_port == 0 || tcp_port == 0) return fail(NodeError::Kind::net, "mDNS ports must not be zero");
    auto key = node.to_string();
    auto label = "loa-" + key.substr(0, LABEL_KEY_CHARS);
    ServiceRecord record;
    record.ty_domain = std::string(MDNS_SERVICE_TYPE);
    record.fullname = label + "." + record.ty_domain;
    record.hostname = label + ".local.";
    record.port = udp_port;
    record.txt = {{std::string(KEY), key}, {std::string(TCP), std::to_string(tcp_port)}};
    if (!name.empty()) record.txt.emplace_back(std::string(NAME), name);
    return record;
}

std::optional<NearbyPeer> parse_service(const ServiceRecord& service) {
    auto port = service.port;
    if (service.ty_domain != MDNS_SERVICE_TYPE || port == 0) return std::nullopt;
    const auto* key = service.property(KEY);
    if (!key) return std::nullopt;
    auto node = NodeId::parse(*key);
    if (!node) return std::nullopt;
    auto tcp = port;
    if (const auto* text = service.property(TCP)) {
        auto parsed = parse_port(*text);
        if (!parsed) return std::nullopt;
        tcp = *parsed;
    }
    std::vector<Ip> ips;
    for (const auto& text : service.addresses)
        if (auto ip = Ip::parse(text)) ips.push_back(*ip);
    std::ranges::sort(ips);
    std::vector<SocketAddr> addresses;
    for (const auto& ip : ips) addresses.push_back(ip.with_port(port));
    if (tcp != port)
        for (const auto& ip : ips) addresses.push_back(ip.with_port(tcp));
    std::optional<std::string> name;
    if (const auto* text = service.property(NAME); text && !text->empty()) name = *text;
    return NearbyPeer::make(*node, NearbySource::mdns, std::move(name), std::move(addresses));
}

// ---- the socket thread ----

struct Mdns::Impl {
    ServiceRecord own;
    NodeId own_node;
    std::vector<int> sockets;

    // Our addresses; the records' `mdns_string_t`s point into `own`.
    std::vector<sockaddr_in> local_v4;
    std::vector<sockaddr_in6> local_v6;

    // Browse state, touched only by the thread.
    std::map<std::string, Heard> heard;                         // fullname (lowered) -> what's known
    std::map<std::string, std::set<std::string>> host_ips;      // hostname (lowered) -> IPs
    std::map<std::string, NearbyPeer> sent;                     // fullname -> last pushed

    std::mutex mutex;
    std::condition_variable ready;
    std::deque<NearbyPeer> queue;
    bool done = false;

    std::jthread thread;

    ~Impl() {
        thread = {}; // request_stop + join
        goodbye();
        for (int sock : sockets) mdns_socket_close(sock);
    }

    // PTR answer plus SRV, TXT and address records as additionals.
    void records(std::uint32_t ttl, mdns_record_t& answer, std::vector<mdns_record_t>& extra) {
        local_addresses(local_v4, local_v6);
        answer = {};
        answer.name = view(own.ty_domain);
        answer.type = MDNS_RECORDTYPE_PTR;
        answer.data.ptr.name = view(own.fullname);
        answer.rclass = 0;
        answer.ttl = ttl;

        extra.clear();
        mdns_record_t srv{};
        srv.name = view(own.fullname);
        srv.type = MDNS_RECORDTYPE_SRV;
        srv.data.srv.priority = 0;
        srv.data.srv.weight = 0;
        srv.data.srv.port = own.port;
        srv.data.srv.name = view(own.hostname);
        srv.rclass = MDNS_CACHE_FLUSH;
        srv.ttl = ttl;
        extra.push_back(srv);
        for (const auto& [k, v] : own.txt) {
            mdns_record_t txt{};
            txt.name = view(own.fullname);
            txt.type = MDNS_RECORDTYPE_TXT;
            txt.data.txt.key = view(k);
            txt.data.txt.value = view(v);
            txt.rclass = MDNS_CACHE_FLUSH;
            txt.ttl = ttl;
            extra.push_back(txt);
        }
        for (const auto& addr : local_v4) {
            mdns_record_t a{};
            a.name = view(own.hostname);
            a.type = MDNS_RECORDTYPE_A;
            a.data.a.addr = addr;
            a.rclass = MDNS_CACHE_FLUSH;
            a.ttl = ttl;
            extra.push_back(a);
        }
        for (const auto& addr : local_v6) {
            mdns_record_t aaaa{};
            aaaa.name = view(own.hostname);
            aaaa.type = MDNS_RECORDTYPE_AAAA;
            aaaa.data.aaaa.addr = addr;
            aaaa.rclass = MDNS_CACHE_FLUSH;
            aaaa.ttl = ttl;
            extra.push_back(aaaa);
        }
    }

    void announce() {
        std::vector<std::uint32_t> buffer(BUFFER_WORDS);
        mdns_record_t answer;
        std::vector<mdns_record_t> extra;
        records(TTL, answer, extra);
        for (int sock : sockets)
            mdns_announce_multicast(sock, buffer.data(), buffer.size() * 4, answer, nullptr, 0, extra.data(),
                                    extra.size());
    }

    void goodbye() {
        std::vector<std::uint32_t> buffer(BUFFER_WORDS);
        mdns_record_t answer;
        std::vector<mdns_record_t> extra;
        records(0, answer, extra);
        for (int sock : sockets)
            mdns_goodbye_multicast(sock, buffer.data(), buffer.size() * 4, answer, nullptr, 0, extra.data(),
                                   extra.size());
    }

    void browse() {
        std::vector<std::uint32_t> buffer(BUFFER_WORDS);
        for (int sock : sockets)
            mdns_query_send(sock, MDNS_RECORDTYPE_PTR, own.ty_domain.data(), own.ty_domain.size(), buffer.data(),
                            buffer.size() * 4, 0);
    }

    void answer_question(int sock, const sockaddr* from, std::size_t addrlen, std::uint16_t query_id,
                         std::uint16_t rtype, std::uint16_t rclass, const std::string& name) {
        bool any = rtype == MDNS_RECORDTYPE_ANY;
        bool ours = (iequals(name, own.ty_domain) && (any || rtype == MDNS_RECORDTYPE_PTR)) ||
                    (iequals(name, own.fullname) &&
                     (any || rtype == MDNS_RECORDTYPE_SRV || rtype == MDNS_RECORDTYPE_TXT)) ||
                    (iequals(name, own.hostname) &&
                     (any || rtype == MDNS_RECORDTYPE_A || rtype == MDNS_RECORDTYPE_AAAA));
        if (!ours) return;
        std::vector<std::uint32_t> buffer(BUFFER_WORDS);
        mdns_record_t answer;
        std::vector<mdns_record_t> extra;
        records(TTL, answer, extra);
        if (rclass & MDNS_UNICAST_RESPONSE) {
            mdns_query_answer_unicast(sock, from, addrlen, buffer.data(), buffer.size() * 4, query_id,
                                      static_cast<mdns_record_type_t>(rtype), name.data(), name.size(), answer,
                                      nullptr, 0, extra.data(), extra.size());
        } else {
            mdns_query_answer_multicast(sock, buffer.data(), buffer.size() * 4, answer, nullptr, 0, extra.data(),
                                        extra.size());
        }
    }

    void heard_record(std::uint16_t rtype, std::uint32_t ttl, const void* data, std::size_t size,
                      const std::string& name, std::size_t record_offset, std::size_t record_length) {
        char text[256];
        auto key = lowered(name);
        switch (rtype) {
        case MDNS_RECORDTYPE_PTR: {
            if (!iequals(name, own.ty_domain)) return;
            auto target = mdns_record_parse_ptr(data, size, record_offset, record_length, text, sizeof text);
            auto instance = lowered(std::string(target.str, target.length));
            if (ttl == 0) {
                heard.erase(instance);
                sent.erase(instance);
            } else {
                heard[instance];
            }
            return;
        }
        case MDNS_RECORDTYPE_SRV: {
            if (!key.ends_with("." + own.ty_domain)) return;
            auto srv = mdns_record_parse_srv(data, size, record_offset, record_length, text, sizeof text);
            auto& entry = heard[key];
            entry.port = srv.port;
            entry.target = lowered(std::string(srv.name.str, srv.name.length));
            update(key);
            return;
        }
        case MDNS_RECORDTYPE_TXT: {
            if (!key.ends_with("." + own.ty_domain)) return;
            std::array<mdns_record_txt_t, 32> pairs{};
            auto count = mdns_record_parse_txt(data, size, record_offset, record_length, pairs.data(), pairs.size());
            std::vector<std::pair<std::string, std::string>> txt;
            for (std::size_t i = 0; i < count; ++i)
                txt.emplace_back(std::string(pairs[i].key.str, pairs[i].key.length),
                                 std::string(pairs[i].value.str, pairs[i].value.length));
            heard[key].txt = std::move(txt);
            update(key);
            return;
        }
        case MDNS_RECORDTYPE_A: {
            sockaddr_in addr{};
            mdns_record_parse_a(data, size, record_offset, record_length, &addr);
            char ip[INET_ADDRSTRLEN] = {};
            inet_ntop(AF_INET, &addr.sin_addr, ip, sizeof ip);
            add_ip(key, ip, ttl);
            return;
        }
        case MDNS_RECORDTYPE_AAAA: {
            sockaddr_in6 addr{};
            mdns_record_parse_aaaa(data, size, record_offset, record_length, &addr);
            char ip[INET6_ADDRSTRLEN] = {};
            inet_ntop(AF_INET6, &addr.sin6_addr, ip, sizeof ip);
            add_ip(key, ip, ttl);
            return;
        }
        default:
            return;
        }
    }

    void add_ip(const std::string& host, const std::string& ip, std::uint32_t ttl) {
        if (ttl == 0) {
            if (auto it = host_ips.find(host); it != host_ips.end()) it->second.erase(ip);
            return;
        }
        if (!host_ips[host].insert(ip).second) return;
        for (const auto& [fullname, entry] : heard)
            if (entry.target == host) update(fullname);
    }

    // Pushes the instance once it has a port, TXT and an address, unless unchanged.
    void update(const std::string& fullname) {
        auto it = heard.find(fullname);
        if (it == heard.end() || !it->second.port || !it->second.txt) return;
        auto ips = host_ips.find(it->second.target);
        if (ips == host_ips.end() || ips->second.empty()) return;
        ServiceRecord record;
        record.ty_domain = own.ty_domain;
        record.fullname = fullname;
        record.hostname = it->second.target;
        record.port = *it->second.port;
        record.txt = *it->second.txt;
        record.addresses.assign(ips->second.begin(), ips->second.end());
        auto peer = parse_service(record);
        if (!peer || peer->key == own_node) return;
        if (auto last = sent.find(fullname); last != sent.end()) {
            auto same = *peer;
            same.seen_ms = last->second.seen_ms;
            if (same == last->second) return;
        }
        sent[fullname] = *peer;
        {
            std::lock_guard lock(mutex);
            queue.push_back(std::move(*peer));
        }
        ready.notify_one();
    }

    static int callback(int sock, const sockaddr* from, std::size_t addrlen, mdns_entry_type_t entry,
                        std::uint16_t query_id, std::uint16_t rtype, std::uint16_t rclass, std::uint32_t ttl,
                        const void* data, std::size_t size, std::size_t name_offset, std::size_t /*name_length*/,
                        std::size_t record_offset, std::size_t record_length, void* user_data) {
        auto* self = static_cast<Impl*>(user_data);
        char text[256];
        auto offset = name_offset;
        auto extracted = mdns_string_extract(data, size, &offset, text, sizeof text);
        std::string name(extracted.str, extracted.length);
        if (entry == MDNS_ENTRYTYPE_QUESTION)
            self->answer_question(sock, from, addrlen, query_id, rtype, rclass, name);
        else
            self->heard_record(rtype, ttl, data, size, name, record_offset, record_length);
        return 0;
    }

    void run(std::stop_token stop) {
        announce();
        browse();
        auto next_browse = std::chrono::steady_clock::now() + BROWSE_EVERY;
        std::vector<std::uint32_t> buffer(BUFFER_WORDS);
        std::vector<pollfd> fds;
        for (int sock : sockets) fds.push_back({sock, POLLIN, 0});
        while (!stop.stop_requested()) {
            if (poll(fds.data(), fds.size(), POLL_MS) > 0) {
                for (auto& fd : fds) {
                    if (fd.revents & POLLIN)
                        mdns_socket_listen(fd.fd, buffer.data(), buffer.size() * 4, &Impl::callback, this);
                    fd.revents = 0;
                }
            }
            if (std::chrono::steady_clock::now() >= next_browse) {
                browse();
                next_browse = std::chrono::steady_clock::now() + BROWSE_EVERY;
            }
        }
        {
            std::lock_guard lock(mutex);
            done = true;
        }
        ready.notify_all();
    }
};

Mdns::Mdns(std::unique_ptr<Impl> impl) : impl_(std::move(impl)) {}

Mdns::~Mdns() = default;

Result<std::unique_ptr<Mdns>> Mdns::start(const Announce& announce) {
    auto info = announce.info();
    if (!info) return std::unexpected(info.error());
    auto impl = std::make_unique<Impl>();
    impl->own = std::move(*info);
    impl->own_node = announce.node;

    sockaddr_in v4{};
    v4.sin_family = AF_INET;
    v4.sin_addr.s_addr = INADDR_ANY;
    v4.sin_port = htons(MDNS_PORT);
    int sock4 = mdns_socket_open_ipv4(&v4);
    int open_error = errno;
    if (sock4 >= 0) impl->sockets.push_back(sock4);

    sockaddr_in6 v6{};
    v6.sin6_family = AF_INET6;
    v6.sin6_addr = in6addr_any;
    v6.sin6_port = htons(MDNS_PORT);
    int sock6 = mdns_socket_open_ipv6(&v6);
    if (sock6 >= 0) impl->sockets.push_back(sock6);

    if (sock4 < 0) {
        for (int sock : impl->sockets) mdns_socket_close(sock);
        impl->sockets.clear();
        return fail(NodeError::Kind::net,
                    "mDNS: cannot open the IPv4 multicast socket on port 5353: " + std::string(std::strerror(open_error)));
    }

    auto* raw = impl.get();
    impl->thread = std::jthread([raw](std::stop_token stop) { raw->run(std::move(stop)); });
    return std::unique_ptr<Mdns>(new Mdns(std::move(impl)));
}

std::optional<NearbyPeer> Mdns::next() {
    std::unique_lock lock(impl_->mutex);
    impl_->ready.wait(lock, [&] { return !impl_->queue.empty() || impl_->done; });
    if (impl_->queue.empty()) return std::nullopt;
    auto peer = std::move(impl_->queue.front());
    impl_->queue.pop_front();
    return peer;
}

Availability Mdns::availability() const {
    return {Availability::State::active, "announcing and browsing on the LAN"};
}

void Mdns::stop() { impl_->thread.request_stop(); }

} // namespace loa::p2p
