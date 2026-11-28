#include "loa/p2p/tcp.hpp"

#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>

#include <openssl/bn.h>
#include <openssl/err.h>
#include <openssl/rand.h>
#include <openssl/evp.h>
#include <openssl/ssl.h>
#include <openssl/x509.h>
#include <openssl/x509v3.h>

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <condition_variable>
#include <cstring>
#include <deque>
#include <format>
#include <mutex>
#include <csignal>
#include <thread>

#include "loa/p2p/yamux.hpp"
#include "loa/runtime/log.hpp"

namespace loa::p2p {
namespace {

template <auto F>
struct Free {
    template <class T>
    void operator()(T* p) const noexcept { F(p); }
};
using Pkey = std::unique_ptr<EVP_PKEY, Free<EVP_PKEY_free>>;
using Cert = std::unique_ptr<X509, Free<X509_free>>;
using Ctx = std::unique_ptr<SSL_CTX, Free<SSL_CTX_free>>;
using Ssl = std::unique_ptr<SSL, Free<SSL_free>>;

std::string ssl_errors() {
    std::string out;
    while (const auto code = ERR_get_error()) {
        char buf[256];
        ERR_error_string_n(code, buf, sizeof buf);
        if (!out.empty()) out += "; ";
        out += buf;
    }
    return out.empty() ? "TLS failure" : out;
}

std::unexpected<NodeError> net_error(std::string_view what) {
    return fail(NodeError::Kind::net, std::format("{}: {}", what, ssl_errors()));
}

struct Identity {
    Pkey key;
    Cert cert;
};

// rcgen's defaults: `CN=rcgen self signed cert`, valid 1975..4096, SAN `localhost`.
Result<Identity> make_identity(const std::array<std::uint8_t, 32>& seed) {
    Pkey key(EVP_PKEY_new_raw_private_key(EVP_PKEY_ED25519, nullptr, seed.data(), seed.size()));
    if (!key) return net_error("ed25519 key");
    Cert cert(X509_new());
    if (!cert) return net_error("certificate");
    std::array<std::uint8_t, 16> serial{};
    RAND_bytes(serial.data(), serial.size());
    serial[0] &= 0x7f;
    std::unique_ptr<BIGNUM, Free<BN_free>> bn(BN_bin2bn(serial.data(), serial.size(), nullptr));
    X509_NAME* name = X509_get_subject_name(cert.get());
    const bool ok =
        X509_set_version(cert.get(), X509_VERSION_3) == 1 && bn &&
        BN_to_ASN1_INTEGER(bn.get(), X509_get_serialNumber(cert.get())) &&
        ASN1_TIME_set_string_X509(X509_getm_notBefore(cert.get()), "19750101000000Z") == 1 &&
        ASN1_TIME_set_string_X509(X509_getm_notAfter(cert.get()), "40960101000000Z") == 1 &&
        X509_NAME_add_entry_by_txt(name, "CN", MBSTRING_UTF8,
                                   reinterpret_cast<const unsigned char*>("rcgen self signed cert"), -1, -1, 0) == 1 &&
        X509_set_issuer_name(cert.get(), name) == 1 && X509_set_pubkey(cert.get(), key.get()) == 1;
    if (!ok) return net_error("certificate");
    X509V3_CTX v3;
    X509V3_set_ctx_nodb(&v3);
    X509V3_set_ctx(&v3, cert.get(), cert.get(), nullptr, nullptr, 0);
    std::unique_ptr<X509_EXTENSION, Free<X509_EXTENSION_free>> san(
        X509V3_EXT_conf_nid(nullptr, &v3, NID_subject_alt_name, "DNS:localhost"));
    if (!san || X509_add_ext(cert.get(), san.get(), -1) != 1) return net_error("certificate");
    if (X509_sign(cert.get(), key.get(), nullptr) <= 0) return net_error("certificate signature");
    return Identity{std::move(key), std::move(cert)};
}

Result<Bytes> der_of(X509* cert) {
    unsigned char* out = nullptr;
    const int len = i2d_X509(cert, &out);
    if (len <= 0) return net_error("certificate encoding");
    Bytes der(out, out + len);
    OPENSSL_free(out);
    return der;
}

Result<NodeId> id_of(X509* cert) {
    const auto invalid = [] { return fail(NodeError::Kind::invalid_key, "not an Ed25519 node certificate"); };
    unsigned char* out = nullptr;
    const int len = i2d_X509_PUBKEY(X509_get_X509_PUBKEY(cert), &out);
    if (len <= 0) return invalid();
    const std::span<const std::uint8_t> spki(out, static_cast<std::size_t>(len));
    const bool ed25519 = spki.size() == ED25519_SPKI_PREFIX.size() + 32 &&
                         std::equal(ED25519_SPKI_PREFIX.begin(), ED25519_SPKI_PREFIX.end(), spki.begin());
    std::string hex;
    if (ed25519)
        for (auto b : spki.subspan(ED25519_SPKI_PREFIX.size())) hex += std::format("{:02x}", b);
    OPENSSL_free(out);
    if (!ed25519) return invalid();
    auto key = NodeId::parse(hex); // curve check, as `NodeId::from_bytes`
    if (!key) return fail(NodeError::Kind::invalid_key, key.error().to_string());
    return *key;
}

// What the verifier of one handshake checks and found.
struct Verify {
    std::optional<NodeId> expected;
    std::string error;
};

// The one verifier of both sides: a single Ed25519 leaf, of `expected` when set.
int verify_node(X509_STORE_CTX* store, void*) {
    auto* ssl = static_cast<SSL*>(X509_STORE_CTX_get_ex_data(store, SSL_get_ex_data_X509_STORE_CTX_idx()));
    auto* verify = ssl ? static_cast<Verify*>(SSL_get_app_data(ssl)) : nullptr;
    const auto refuse = [&](std::string why) {
        if (verify) verify->error = std::move(why);
        X509_STORE_CTX_set_error(store, X509_V_ERR_APPLICATION_VERIFICATION);
        return 0;
    };
    STACK_OF(X509)* chain = X509_STORE_CTX_get0_untrusted(store);
    if (!chain || sk_X509_num(chain) != 1) return refuse("node certificate must be a single leaf");
    auto id = id_of(X509_STORE_CTX_get0_cert(store));
    if (!id) return refuse(id.error().to_string());
    if (verify && verify->expected && *verify->expected != *id)
        return refuse("remote node key differs from requested peer");
    return 1;
}

int select_alpn(SSL*, const unsigned char** out, unsigned char* outlen, const unsigned char* in, unsigned inlen,
                void* arg) {
    const auto& served = *static_cast<const std::vector<std::string>*>(arg);
    if (served.empty()) return SSL_TLSEXT_ERR_NOACK;
    for (const auto& label : served) {
        for (unsigned i = 0; i < inlen; i += 1u + in[i]) {
            const unsigned len = in[i];
            if (i + 1 + len > inlen) break;
            if (len == label.size() && std::memcmp(in + i + 1, label.data(), len) == 0) {
                *out = in + i + 1;
                *outlen = static_cast<unsigned char>(len);
                return SSL_TLSEXT_ERR_OK;
            }
        }
    }
    return SSL_TLSEXT_ERR_ALERT_FATAL;
}

Result<Ctx> make_ctx(const Identity& id) {
    Ctx ctx(SSL_CTX_new(TLS_method()));
    const bool ok = ctx && SSL_CTX_set_min_proto_version(ctx.get(), TLS1_3_VERSION) == 1 &&
                    SSL_CTX_set_max_proto_version(ctx.get(), TLS1_3_VERSION) == 1 &&
                    SSL_CTX_use_certificate(ctx.get(), id.cert.get()) == 1 &&
                    SSL_CTX_use_PrivateKey(ctx.get(), id.key.get()) == 1 &&
                    SSL_CTX_set1_sigalgs_list(ctx.get(), "ed25519") == 1 &&
                    SSL_CTX_set1_client_sigalgs_list(ctx.get(), "ed25519") == 1 &&
                    SSL_CTX_set_num_tickets(ctx.get(), 0) == 1;
    if (!ok) return net_error("TLS config");
    SSL_CTX_set_verify(ctx.get(), SSL_VERIFY_PEER | SSL_VERIFY_FAIL_IF_NO_PEER_CERT, nullptr);
    SSL_CTX_set_cert_verify_callback(ctx.get(), verify_node, nullptr);
    SSL_CTX_set_options(ctx.get(), SSL_OP_IGNORE_UNEXPECTED_EOF | SSL_OP_NO_TICKET);
    SSL_CTX_set_mode(ctx.get(), SSL_MODE_ACCEPT_MOVING_WRITE_BUFFER);
    return ctx;
}

// ---- addresses ----

struct Addr {
    sockaddr_storage storage{};
    socklen_t len = 0;
    int family() const { return storage.ss_family; }
    const sockaddr* get() const { return reinterpret_cast<const sockaddr*>(&storage); }
};

Result<Addr> parse_addr(const SocketAddr& text) {
    const auto bad = [&] { return fail(NodeError::Kind::invalid_address, std::format("invalid address \"{}\"", text)); };
    std::string host;
    std::string port;
    if (text.starts_with('[')) {
        const auto close = text.find("]:");
        if (close == std::string::npos) return bad();
        host = text.substr(1, close - 1);
        port = text.substr(close + 2);
    } else {
        const auto colon = text.rfind(':');
        if (colon == std::string::npos) return bad();
        host = text.substr(0, colon);
        port = text.substr(colon + 1);
    }
    unsigned long p = 0;
    if (port.empty() || port.size() > 5 || !std::all_of(port.begin(), port.end(), [](char c) { return c >= '0' && c <= '9'; }) ||
        (p = std::stoul(port)) > 65535)
        return bad();
    Addr addr;
    if (text.starts_with('[')) {
        auto* v6 = reinterpret_cast<sockaddr_in6*>(&addr.storage);
        v6->sin6_family = AF_INET6;
        v6->sin6_port = htons(static_cast<std::uint16_t>(p));
        if (inet_pton(AF_INET6, host.c_str(), &v6->sin6_addr) != 1) return bad();
        addr.len = sizeof(sockaddr_in6);
    } else {
        auto* v4 = reinterpret_cast<sockaddr_in*>(&addr.storage);
        v4->sin_family = AF_INET;
        v4->sin_port = htons(static_cast<std::uint16_t>(p));
        if (inet_pton(AF_INET, host.c_str(), &v4->sin_addr) != 1) return bad();
        addr.len = sizeof(sockaddr_in);
    }
    return addr;
}

SocketAddr format_addr(const sockaddr_storage& s) {
    char buf[INET6_ADDRSTRLEN] = {};
    if (s.ss_family == AF_INET6) {
        const auto& v6 = reinterpret_cast<const sockaddr_in6&>(s);
        inet_ntop(AF_INET6, &v6.sin6_addr, buf, sizeof buf);
        return std::format("[{}]:{}", buf, ntohs(v6.sin6_port));
    }
    const auto& v4 = reinterpret_cast<const sockaddr_in&>(s);
    inet_ntop(AF_INET, &v4.sin_addr, buf, sizeof buf);
    return std::format("{}:{}", buf, ntohs(v4.sin_port));
}

std::string errno_text() { return std::strerror(errno); }

// ---- TLS byte stream ----

// One TLS session shared by the mux's reader and writer threads; calls into
// the SSL object are serialized, waits on the non-blocking socket are not.
class TlsSocket final : public yamux::Socket {
  public:
    TlsSocket(Ssl ssl, int fd, std::unique_ptr<Verify> verify)
        : ssl_(std::move(ssl)), fd_(fd), verify_(std::move(verify)) {
        ::fcntl(fd_, F_SETFL, ::fcntl(fd_, F_GETFL) | O_NONBLOCK);
    }
    ~TlsSocket() override {
        ssl_.reset();
        ::close(fd_);
    }

    Result<std::size_t> read(std::span<std::uint8_t> buf) override {
        for (;;) {
            int code = 0;
            {
                std::lock_guard lock(mutex_);
                if (down_) return std::size_t{0};
                ERR_clear_error();
                const int n = SSL_read(ssl_.get(), buf.data(), static_cast<int>(std::min<std::size_t>(buf.size(), INT32_MAX)));
                if (n > 0) return static_cast<std::size_t>(n);
                code = SSL_get_error(ssl_.get(), n);
                if (code == SSL_ERROR_ZERO_RETURN) return std::size_t{0};
                if (code != SSL_ERROR_WANT_READ && code != SSL_ERROR_WANT_WRITE) {
                    if (code == SSL_ERROR_SYSCALL && ERR_peek_error() == 0) return std::size_t{0};
                    return fail(NodeError::Kind::io, ssl_errors());
                }
            }
            wait(code == SSL_ERROR_WANT_READ ? POLLIN : POLLOUT);
        }
    }

    Result<> write_all(std::span<const std::uint8_t> data) override {
        while (!data.empty()) {
            int code = 0;
            {
                std::lock_guard lock(mutex_);
                if (down_) return fail(NodeError::Kind::io, "connection is closed");
                ERR_clear_error();
                const int n = SSL_write(ssl_.get(), data.data(), static_cast<int>(std::min<std::size_t>(data.size(), INT32_MAX)));
                if (n > 0) {
                    data = data.subspan(static_cast<std::size_t>(n));
                    continue;
                }
                code = SSL_get_error(ssl_.get(), n);
                if (code != SSL_ERROR_WANT_READ && code != SSL_ERROR_WANT_WRITE)
                    return fail(NodeError::Kind::io, ssl_errors());
            }
            wait(code == SSL_ERROR_WANT_READ ? POLLIN : POLLOUT);
        }
        return {};
    }

    void shutdown() override {
        std::lock_guard lock(mutex_);
        if (down_) return;
        down_ = true;
        ERR_clear_error();
        SSL_shutdown(ssl_.get()); // best effort close_notify
        ::shutdown(fd_, SHUT_RDWR);
    }

  private:
    // A short poll: another thread's SSL call may have buffered what we wait for.
    void wait(short events) const {
        pollfd p{fd_, events, 0};
        ::poll(&p, 1, 50);
    }

    std::mutex mutex_;
    Ssl ssl_;
    int fd_;
    std::unique_ptr<Verify> verify_;
    bool down_ = false;
};

// ---- connection ----

struct Inbound {
    std::mutex m;
    std::condition_variable cv;
    std::deque<Stream> queue;
    bool done = false;
};

NodeError closed_error() { return NodeError(NodeError::Kind::net, "TCP connection ended"); }

class TcpConnection final : public PeerConnection {
  public:
    TcpConnection(std::shared_ptr<yamux::Session> session, NodeId peer, Bytes alpn)
        : session_(std::move(session)), peer_(peer), alpn_(std::move(alpn)), inbound_(std::make_shared<Inbound>()) {
        // Hands out inbound streams once their first byte names an application stream.
        std::thread([session = session_, inbound = inbound_] {
            while (auto stream = session->accept()) {
                std::thread([inbound, s = std::move(*stream)]() mutable {
                    std::uint8_t kind = 0xff;
                    if (!s.recv->read_exact(std::span(&kind, 1)) || kind != STREAM_FRAME) return;
                    std::unique_lock lock(inbound->m);
                    inbound->cv.wait(lock, [&] { return inbound->done || inbound->queue.size() < TCP_QUEUE; });
                    if (inbound->done) return;
                    inbound->queue.push_back(std::move(s));
                    inbound->cv.notify_all();
                }).detach();
            }
            std::lock_guard lock(inbound->m);
            inbound->done = true;
            inbound->cv.notify_all();
        }).detach();
    }
    ~TcpConnection() override { session_->close("connection handles dropped"); }

    NodeId remote() const override { return peer_; }
    Bytes alpn() const override { return alpn_; }
    TransportKind kind() const override { return TransportKind::tcp; }

    Result<Stream> open_stream() override {
        auto stream = session_->open();
        if (!stream) return std::unexpected(closed_error());
        const std::uint8_t kind = STREAM_FRAME;
        if (auto ok = stream->send->write_all(std::span(&kind, 1)); !ok) return std::unexpected(ok.error());
        return stream;
    }

    Result<Stream> accept_stream() override {
        std::unique_lock lock(inbound_->m);
        inbound_->cv.wait(lock, [&] { return inbound_->done || !inbound_->queue.empty(); });
        if (inbound_->queue.empty()) return std::unexpected(closed_error());
        auto s = std::move(inbound_->queue.front());
        inbound_->queue.pop_front();
        inbound_->cv.notify_all();
        return s;
    }

    void close(std::uint32_t code, std::string_view reason) override {
        session_->close(std::format("{}: {}", code, reason));
    }

    std::string closed() override { return session_->closed(); }

  private:
    std::shared_ptr<yamux::Session> session_;
    NodeId peer_;
    Bytes alpn_;
    std::shared_ptr<Inbound> inbound_;
};

struct Handshaken {
    std::shared_ptr<TlsSocket> socket;
    NodeId peer;
    Bytes alpn;
};

// Runs the TLS handshake on the connected socket `fd`, which it owns from here on.
Result<Handshaken> handshake(SSL_CTX* ctx, int fd, std::optional<NodeId> expected, std::string_view alpn, bool client) {
    Ssl ssl(SSL_new(ctx));
    if (!ssl) {
        ::close(fd);
        return net_error("TLS session");
    }
    auto verify = std::make_unique<Verify>(Verify{expected, {}});
    SSL_set_app_data(ssl.get(), verify.get());
    SSL_set_fd(ssl.get(), fd);
    if (client) {
        Bytes protos{static_cast<std::uint8_t>(alpn.size())};
        protos.insert(protos.end(), alpn.begin(), alpn.end());
        SSL_set_tlsext_host_name(ssl.get(), "localhost");
        SSL_set_alpn_protos(ssl.get(), protos.data(), static_cast<unsigned>(protos.size()));
    }
    ERR_clear_error();
    if ((client ? SSL_connect(ssl.get()) : SSL_accept(ssl.get())) != 1) {
        std::string why = verify->error.empty() ? ssl_errors() : verify->error;
        ssl.reset();
        ::close(fd);
        return fail(NodeError::Kind::net, why);
    }
    X509* cert = SSL_get0_peer_certificate(ssl.get());
    if (!cert) {
        ::close(fd);
        return fail(NodeError::Kind::net, "the peer sent no certificate");
    }
    auto peer = id_of(cert);
    const unsigned char* selected = nullptr;
    unsigned selected_len = 0;
    SSL_get0_alpn_selected(ssl.get(), &selected, &selected_len);
    Bytes negotiated(selected, selected + selected_len);
    auto socket = std::make_shared<TlsSocket>(std::move(ssl), fd, std::move(verify));
    if (!peer) return std::unexpected(peer.error());
    return Handshaken{std::move(socket), *peer, std::move(negotiated)};
}

} // namespace

Result<Bytes> node_certificate(const std::array<std::uint8_t, 32>& seed) {
    auto id = make_identity(seed);
    if (!id) return std::unexpected(id.error());
    return der_of(id->cert.get());
}

Result<NodeId> certificate_id(std::span<const std::uint8_t> der) {
    const unsigned char* p = der.data();
    Cert cert(d2i_X509(nullptr, &p, static_cast<long>(der.size())));
    if (!cert) return fail(NodeError::Kind::invalid_key, "not an X.509 certificate");
    return id_of(cert.get());
}

struct Tcp::Impl {
    Ctx server;
    Ctx client;
    std::vector<std::string> alpns;
    Router router;
    int listener = -1;
    SocketAddr addr;
    std::mutex m;
    bool closing = false;
    std::thread accept_thread;

    void accept_loop(const std::shared_ptr<Impl>& self) {
        for (;;) {
            const int fd = ::accept4(listener, nullptr, nullptr, SOCK_CLOEXEC);
            if (fd < 0) {
                {
                    std::lock_guard lock(m);
                    if (closing) return;
                }
                if (errno == EINTR || errno == ECONNABORTED || errno == EMFILE || errno == ENFILE) continue;
                return;
            }
            std::thread([self, fd] {
                auto done = handshake(self->server.get(), fd, std::nullopt, {}, false);
                if (done && done->alpn.empty()) done = fail(NodeError::Kind::protocol, "no shared ALPN");
                if (!done) {
                    loa::runtime::log::debug("p2p", std::format("TCP peer handshake ended: {}", done.error().to_string()));
                    return;
                }
                auto session = yamux::Session::start(done->socket, yamux::Mode::server);
                Connection conn = std::make_shared<TcpConnection>(session, done->peer, std::move(done->alpn));
                self->router.dispatch(conn);
                // Keep the mux alive until the peer closes, so queued answers are flushed.
                conn->closed();
            }).detach();
        }
    }

    void shutdown() {
        {
            std::lock_guard lock(m);
            if (closing) return;
            closing = true;
            ::shutdown(listener, SHUT_RDWR);
        }
        if (accept_thread.joinable()) accept_thread.join();
        ::close(listener);
    }

    Result<Connection> connect(std::optional<NodeId> expected, const SocketAddr& addr, std::string_view alpn) {
        const auto failed = [&](std::string reason) {
            return fail(NodeError::Kind::invalid_address, std::format("invalid address \"{}\": {}", addr, reason));
        };
        auto target = parse_addr(addr);
        if (!target) return std::unexpected(target.error());
        const int fd = ::socket(target->family(), SOCK_STREAM | SOCK_CLOEXEC, 0);
        if (fd < 0) return failed(errno_text());
        if (::connect(fd, target->get(), target->len) != 0) {
            auto reason = errno_text();
            ::close(fd);
            return failed(std::move(reason));
        }
        auto done = handshake(client.get(), fd, expected, alpn, true);
        if (!done) return failed(done.error().message());
        if (expected && *expected != done->peer)
            return fail(NodeError::Kind::wrong_peer, std::format("expected node {}, the peer proved {}",
                                                                 expected->to_string(), done->peer.to_string()));
        const Bytes* got = done->alpn.empty() ? nullptr : &done->alpn;
        if (auto ok = check_alpn(done->peer, alpn, got); !ok) return std::unexpected(ok.error());
        auto session = yamux::Session::start(done->socket, yamux::Mode::client);
        return Connection(std::make_shared<TcpConnection>(session, done->peer, std::move(done->alpn)));
    }
};

Tcp::Tcp(std::shared_ptr<Impl> impl) : impl_(std::move(impl)) {}

Tcp::~Tcp() { impl_->shutdown(); }

Result<std::shared_ptr<Tcp>> Tcp::bind(const std::array<std::uint8_t, 32>& seed, const SocketAddr& bind, Router router) {
    // TLS writes to a peer that hung up must fail, not kill the process: ignore
    // SIGPIPE as the Rust runtime does at startup.
    static std::once_flag sigpipe;
    std::call_once(sigpipe, [] { std::signal(SIGPIPE, SIG_IGN); });
    auto id = make_identity(seed);
    if (!id) return std::unexpected(id.error());
    auto server = make_ctx(*id);
    if (!server) return std::unexpected(server.error());
    auto client = make_ctx(*id);
    if (!client) return std::unexpected(client.error());
    auto target = parse_addr(bind);
    if (!target) return std::unexpected(target.error());

    auto impl = std::make_shared<Impl>();
    impl->server = std::move(*server);
    impl->client = std::move(*client);
    impl->alpns = router.alpns();
    impl->router = std::move(router);
    SSL_CTX_set_alpn_select_cb(impl->server.get(), select_alpn, &impl->alpns);

    const int fd = ::socket(target->family(), SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) return fail(NodeError::Kind::io, errno_text());
    const int one = 1;
    ::setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    if (::bind(fd, target->get(), target->len) != 0 || ::listen(fd, 1024) != 0) {
        auto reason = errno_text();
        ::close(fd);
        return fail(NodeError::Kind::io, reason);
    }
    sockaddr_storage local{};
    socklen_t len = sizeof local;
    ::getsockname(fd, reinterpret_cast<sockaddr*>(&local), &len);
    impl->listener = fd;
    impl->addr = format_addr(local);
    impl->accept_thread = std::thread([impl] { impl->accept_loop(impl); });
    return std::shared_ptr<Tcp>(new Tcp(std::move(impl)));
}

Result<Connection> Tcp::dial(const NodeId& peer, std::span<const SocketAddr> hints, std::string_view alpn) {
    NodeError last(NodeError::Kind::connect, std::format("connecting to {}: no TCP address hints", peer.to_string()));
    for (const auto& addr : hints) {
        auto conn = impl_->connect(peer, addr, alpn);
        if (conn) return conn;
        if (conn.error().kind() == NodeError::Kind::invalid_address) {
            const auto& msg = conn.error().message();
            const auto colon = msg.find("\": ");
            last = NodeError(NodeError::Kind::connect, std::format("connecting to {}: {}", peer.to_string(),
                                                                   colon == std::string::npos ? msg : msg.substr(colon + 3)));
        } else {
            last = conn.error();
        }
    }
    return std::unexpected(last);
}

Result<Connection> Tcp::dial_any(const SocketAddr& addr, std::string_view alpn) {
    return impl_->connect(std::nullopt, addr, alpn);
}

std::vector<SocketAddr> Tcp::local_addrs() const { return {impl_->addr}; }

void Tcp::close() { impl_->shutdown(); }

} // namespace loa::p2p
