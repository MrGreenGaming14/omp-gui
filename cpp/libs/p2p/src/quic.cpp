#include "loa/p2p/quic.hpp"

#include <atomic>
#include <format>
#include <mutex>
#include <thread>

#include "loa/iroh_ffi/iroh.hpp"
#include "loa/runtime/log.hpp"

namespace loa::p2p {
namespace {

namespace ffi = loa::iroh_ffi;

NodeId to_node(const ffi::EndpointId& id) { return NodeId{id}; }
std::string describe(const ffi::Error& e) { return e.message; }

class QuicSend final : public SendHalf {
  public:
    explicit QuicSend(std::shared_ptr<ffi::Stream> s) : s_(std::move(s)) {}
    Result<> write_all(std::span<const std::uint8_t> data) override {
        if (auto r = s_->write(data); !r) return fail(NodeError::Kind::io, describe(r.error()));
        return {};
    }
    Result<> shutdown() override {
        if (auto r = s_->finish(); !r) return fail(NodeError::Kind::io, describe(r.error()));
        return {};
    }

  private:
    std::shared_ptr<ffi::Stream> s_;
};

class QuicRecv final : public RecvHalf {
  public:
    explicit QuicRecv(std::shared_ptr<ffi::Stream> s) : s_(std::move(s)) {}
    Result<std::size_t> read(std::span<std::uint8_t> buf) override {
        auto n = s_->read(buf);
        if (!n) return fail(NodeError::Kind::io, describe(n.error()));
        return *n;
    }

  private:
    std::shared_ptr<ffi::Stream> s_;
};

Stream split(ffi::Stream&& s) {
    auto shared = std::make_shared<ffi::Stream>(std::move(s));
    return Stream{std::make_unique<QuicSend>(shared), std::make_unique<QuicRecv>(shared)};
}

class QuicConnection final : public PeerConnection {
  public:
    explicit QuicConnection(ffi::Connection c) : c_(std::move(c)), remote_(to_node(c_.remote_id())), alpn_(c_.alpn()) {}

    NodeId remote() const override { return remote_; }
    Bytes alpn() const override { return alpn_; }
    TransportKind kind() const override {
        return c_.selected_path_is_relay() ? TransportKind::relay : TransportKind::quic;
    }
    Result<Stream> open_stream() override {
        auto s = c_.open_bi();
        if (!s) return fail(NodeError::Kind::net, describe(s.error()));
        return split(std::move(*s));
    }
    Result<Stream> accept_stream() override {
        auto s = c_.accept_bi();
        if (!s) return fail(NodeError::Kind::net, describe(s.error()));
        return split(std::move(*s));
    }
    void close(std::uint32_t code, std::string_view reason) override { c_.close(code, reason); }
    std::string closed() override { return c_.closed(); }

  private:
    ffi::Connection c_;
    NodeId remote_;
    Bytes alpn_;
};

} // namespace

struct Quic::Impl {
    ffi::Endpoint endpoint;
    Router router;
    std::atomic<bool> closing{false};
    std::once_flag closed;
    std::thread accept;

    Impl(ffi::Endpoint ep, Router r) : endpoint(std::move(ep)), router(std::move(r)) {}

    void accept_loop() {
        while (!closing.load()) {
            auto conn = endpoint.accept();
            if (!conn) {
                if (closing.load()) return;
                loa::runtime::log::debug("p2p", std::format("QUIC handshake failed: {}", conn.error().message));
                continue;
            }
            Connection shared = std::make_shared<QuicConnection>(std::move(*conn));
            std::thread([router = router, shared] { router.dispatch(shared); }).detach();
        }
    }

    void shutdown() {
        std::call_once(closed, [&] {
            closing = true;
            endpoint.shutdown();
            if (accept.joinable()) accept.join();
        });
    }
};

Quic::Quic(std::unique_ptr<Impl> impl) : impl_(std::move(impl)) {}

Quic::~Quic() { impl_->shutdown(); }

Result<std::shared_ptr<Quic>> Quic::bind(const std::array<std::uint8_t, 32>& seed, const SocketAddr& bind,
                                         const RelayConfig& relay, Router router) {
    ffi::EndpointOptions options;
    options.seed = seed;
    options.alpns = router.alpns();
    if (!relay.self_hosted.empty()) {
        const auto& url = relay.self_hosted;
        if (!url.starts_with("http://") && !url.starts_with("https://"))
            return fail(NodeError::Kind::invalid_address, std::format("invalid address \"{}\": not a relay URL", url));
        options.relay_urls = {url};
    }
    options.clear_ip_transports = true;
    if (bind.starts_with('['))
        return fail(NodeError::Kind::invalid_address,
                    std::format("invalid address \"{}\": the QUIC endpoint binds IPv4", bind));
    options.ipv4_bind = bind;
    auto endpoint = ffi::Endpoint::bind(options);
    if (!endpoint) return fail(NodeError::Kind::net, describe(endpoint.error()));
    auto impl = std::make_unique<Impl>(std::move(*endpoint), std::move(router));
    auto* raw = impl.get();
    impl->accept = std::thread([raw] { raw->accept_loop(); });
    return std::shared_ptr<Quic>(new Quic(std::move(impl)));
}

Result<Connection> Quic::dial(const NodeId& peer, std::span<const SocketAddr> hints, std::string_view alpn) {
    auto conn = impl_->endpoint.connect(peer.as_bytes(), hints, alpn);
    if (!conn)
        return fail(NodeError::Kind::connect,
                    std::format("connecting to {}: {}", peer.to_string(), describe(conn.error())));
    const auto proved = to_node(conn->remote_id());
    if (proved != peer) {
        conn->close(CLOSE_WRONG_PEER, "wrong node identity");
        return fail(NodeError::Kind::wrong_peer,
                    std::format("expected node {}, the peer proved {}", peer.to_string(), proved.to_string()));
    }
    const auto got = conn->alpn();
    if (auto ok = check_alpn(peer, alpn, &got); !ok) return std::unexpected(ok.error());
    return Connection(std::make_shared<QuicConnection>(std::move(*conn)));
}

Result<Connection> Quic::dial_any(const SocketAddr& addr, std::string_view) {
    return fail(NodeError::Kind::invalid_address, std::format("invalid address \"{}\": QUIC needs the node's key", addr));
}

std::vector<SocketAddr> Quic::local_addrs() const { return impl_->endpoint.ip_addrs(); }

void Quic::close() { impl_->shutdown(); }

} // namespace loa::p2p
