#include "loa/app/adapters/sandbox.hpp"

#include <variant>

#include "loa/bus/writer.hpp"
#include "loa/p2p/node.hpp"
#include "loa/sandbox/providers.hpp"
#include "loa/session/library.hpp"
#include "offload.hpp"

namespace loa::app::adapters {

namespace {

loa::Error peer_error(std::string message) { return sandbox::error::provider("peer", std::move(message)); }

// The p2p stream halves as the sandbox's byte streams.
class StreamReader final : public sandbox::ByteReader {
  public:
    explicit StreamReader(std::unique_ptr<p2p::RecvHalf> recv) : recv_(std::move(recv)) {}
    sandbox::Result<std::size_t> read(std::span<char> buf) override {
        auto n = recv_->read({reinterpret_cast<std::uint8_t*>(buf.data()), buf.size()});
        if (!n) return std::unexpected(loa::Error(ErrorCode::io, n.error().to_string()));
        return *n;
    }

  private:
    std::unique_ptr<p2p::RecvHalf> recv_;
};

class StreamWriter final : public sandbox::ByteWriter {
  public:
    explicit StreamWriter(std::unique_ptr<p2p::SendHalf> send) : send_(std::move(send)) {}
    sandbox::Result<> write_all(std::string_view bytes) override {
        return io(send_->write_all({reinterpret_cast<const std::uint8_t*>(bytes.data()), bytes.size()}));
    }
    sandbox::Result<> shutdown() override { return io(send_->shutdown()); }

  private:
    static sandbox::Result<> io(p2p::Result<> result) {
        if (!result) return std::unexpected(loa::Error(ErrorCode::io, result.error().to_string()));
        return {};
    }
    std::unique_ptr<p2p::SendHalf> send_;
};

sandbox::Channel channel(p2p::Stream stream) {
    return {std::make_unique<StreamReader>(std::move(stream.recv)), std::make_unique<StreamWriter>(std::move(stream.send))};
}

} // namespace

sandbox::Result<std::shared_ptr<core::PatchSink>> LibraryJournal::open(std::string_view session_id) const {
    auto live = library_->create(session_id);
    if (!live)
        return std::unexpected(loa::Error(ErrorCode::io, "can't create the worker session " + std::string(session_id) +
                                                             ": " + live.error().to_string()));
    return std::shared_ptr<core::PatchSink>(live->writer);
}

bool NodeLink::running() const {
    auto node = slot_.get();
    return node && std::holds_alternative<p2p::endpoint::Listening>(node->status().endpoint);
}

std::vector<std::string> NodeLink::peers() const {
    std::vector<std::string> out;
    if (auto node = slot_.get())
        for (const auto& key : node->granting_peers(REQUIRED)) out.push_back(key.to_string());
    return out;
}

std::optional<std::string> NodeLink::name(std::string_view peer) const {
    auto node = slot_.get();
    if (!node) return std::nullopt;
    auto key = p2p::Node::parse_key(peer);
    if (!key) return std::nullopt;
    auto name = node->peer_name(*key);
    if (name == peer) return std::nullopt;
    return name;
}

contracts::Awaitable<sandbox::Result<sandbox::Channel>> NodeLink::open(std::string peer) const {
    auto node = slot_.get();
    if (!node) co_return std::unexpected(peer_error("peer networking is not running"));
    auto key = p2p::Node::parse_key(peer);
    if (!key) co_return std::unexpected(peer_error(key.error().to_string()));
    auto stream = co_await detail::offload(
        [node, key = *key] { return node->open_stream(key, sandbox::PEER_PROTOCOL, VERSION); });
    if (!stream) co_return std::unexpected(peer_error(stream.error().to_string()));
    co_return channel(std::move(*stream));
}

void ServeStub::serve(const p2p::NodeId& peer, p2p::Stream stream) {
    auto result = stub_ ? sandbox::serve_stub_blocking(channel(std::move(stream)), *stub_)
                        : sandbox::Result<>(std::unexpected(peer_error("loa-sandbox-stub is not installed here")));
    if (!result && failures_) failures_(Served{peer.to_string(), result.error().message()});
}

void register_(p2p::NodeSlot<p2p::Node>& slot, std::shared_ptr<ServeStub> handler) {
    slot.add_protocol(p2p::AddedProtocol{.name = std::string(sandbox::PEER_PROTOCOL),
                                         .version = VERSION,
                                         .required = REQUIRED,
                                         .handler = std::move(handler)});
}

} // namespace loa::app::adapters
