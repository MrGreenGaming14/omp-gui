#include "loa/p2p/protocols/extra.hpp"

#include <format>

#include "loa/p2p/auth.hpp"

namespace loa::p2p {

std::string protocol_label(std::string_view name, std::uint32_t version) { return std::format("{}/{}", name, version); }

namespace {

// Stream halves that hold their connection open while they live.
class HeldSend final : public SendHalf {
  public:
    HeldSend(std::unique_ptr<SendHalf> half, Connection conn) : half_(std::move(half)), conn_(std::move(conn)) {}
    Result<> write_all(std::span<const std::uint8_t> data) override { return half_->write_all(data); }
    Result<> shutdown() override { return half_->shutdown(); }

  private:
    std::unique_ptr<SendHalf> half_;
    Connection conn_;
};

class HeldRecv final : public RecvHalf {
  public:
    HeldRecv(std::unique_ptr<RecvHalf> half, Connection conn) : half_(std::move(half)), conn_(std::move(conn)) {}
    Result<std::size_t> read(std::span<std::uint8_t> buf) override { return half_->read(buf); }

  private:
    std::unique_ptr<RecvHalf> half_;
    Connection conn_;
};

} // namespace

namespace extra {

Result<> serve(NodeHost& host, const Connection& conn, Stream stream, const AddedProtocol& protocol) {
    const auto peer = conn->remote();
    // The requester's opening frame makes the stream visible here.
    if (auto opening = wire::expect<wire::Frame>(*stream.recv); !opening) return std::unexpected(opening.error());
    if (auto r = host.grants().challenge(peer, stream); !r) return r;
    const auto& [resource, action] = protocol.required;
    std::optional<std::string> error;
    if (!host.grants().covers(peer, resource, action)) error = std::format("no grant for {} on this node", protocol.name);
    if (auto r = wire::ack<wire::Frame>(*stream.send, error); !r) return r;
    if (error) return fail(NodeError::Kind::refused, *error);
    protocol.handler->serve(peer, std::move(stream));
    return {};
}

Result<Stream> open(NodeHost& host, const NodeId& peer, std::string_view name, std::uint32_t version) {
    auto conn = host.dial(peer, protocol_label(name, version));
    if (!conn) return std::unexpected(conn.error());
    auto stream = (*conn)->open_stream();
    if (!stream) return std::unexpected(stream.error());
    if (auto r = wire::send(*stream->send, wire::Frame{wire::Body(wire::Ack{})}); !r) return std::unexpected(r.error());
    if (auto r = host.grants().present(peer, *stream); !r) return std::unexpected(r.error());
    if (auto r = wire::expect<wire::Frame>(*stream->recv); !r) return std::unexpected(r.error());
    return Stream{std::make_unique<HeldSend>(std::move(stream->send), *conn),
                  std::make_unique<HeldRecv>(std::move(stream->recv), *conn)};
}

} // namespace extra
} // namespace loa::p2p
