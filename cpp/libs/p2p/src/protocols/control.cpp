#include "loa/p2p/protocols/control.hpp"

#include <asio.hpp>

#include <future>
#include <thread>

#include "loa/p2p/auth.hpp"
#include "loa/runtime/time.hpp"

namespace loa::p2p::control {

namespace {

using runtime::time::now_ms;

trust::GrantId grant_id(const Bytes& bytes) { return {std::string(bytes.begin(), bytes.end())}; }

Result<> hello(NodeHost& host, const Connection& conn, Stream& stream) {
    auto answer = wire::call<wire::ControlFrame>(stream, wire::ControlFrame{wire::Control(host.hello())});
    if (!answer) return std::unexpected(answer.error());
    const auto* peer_hello = answer->body ? std::get_if<wire::Hello>(&*answer->body) : nullptr;
    if (!peer_hello) return fail(NodeError::Kind::protocol, "first control frame must be hello");
    if (!valid_hello(*peer_hello)) return fail(NodeError::Kind::protocol, "invalid peer hello");
    host.observe(conn->remote(), *peer_hello, conn->kind());
    return {};
}

using RenewResult = std::expected<std::vector<std::uint8_t>, trust::RenewFailure>;

RenewResult renew_blocking(NodeHost& host, const trust::PublicKey& issuer, const trust::GrantId& grant) {
    const auto& id = grant.as_str();
    auto answer = call_peer(host, issuer, wire::Control(wire::GrantRenewal{Bytes(id.begin(), id.end())}));
    if (!answer) return std::unexpected(trust::RenewFailure::unreachable(answer.error().to_string()));
    auto* result = std::get_if<wire::GrantRenewalResult>(&*answer);
    if (!result) return std::unexpected(trust::RenewFailure::unreachable("unexpected answer"));
    if (result->token) return std::move(*result->token);
    return std::unexpected(trust::RenewFailure::refused(result->refusal));
}

} // namespace

bool valid_hello(const wire::Hello& hello) {
    if (hello.app_version.empty()) return false;
    for (const auto& p : hello.protocols)
        if (p.name.empty() || p.version == 0) return false;
    return true;
}

Result<wire::Control> call(NodeHost& host, const Connection& conn, wire::Control request) {
    auto stream = conn->open_stream();
    if (!stream) return std::unexpected(stream.error());
    if (auto r = hello(host, conn, *stream); !r) return std::unexpected(r.error());
    auto answer = wire::call<wire::ControlFrame>(*stream, wire::ControlFrame{std::move(request)});
    if (!answer) return std::unexpected(answer.error());
    if (!answer->body) return fail(NodeError::Kind::protocol, "empty answer");
    return std::move(*answer->body);
}

Result<wire::Control> call_peer(NodeHost& host, const NodeId& peer, wire::Control request) {
    auto conn = host.dial(peer, wire::CONTROL);
    if (!conn) return std::unexpected(conn.error());
    auto answer = call(host, *conn, std::move(request));
    (*conn)->close(0, "done");
    return answer;
}

wire::Control presence() { return wire::Presence{true, now_ms()}; }

Result<> serve(NodeHost& host, const Connection& conn, Stream stream) {
    const auto peer = conn->remote();
    if (auto r = hello(host, conn, stream); !r) return r;
    auto request = wire::recv<wire::ControlFrame>(*stream.recv);
    if (!request) return std::unexpected(request.error());
    if (!*request || !(*request)->body) return {};
    auto& grants = host.grants();
    Result<> result;
    auto& body = *(*request)->body;
    if (std::holds_alternative<wire::Presence>(body)) {
        // The hello already marked a saved peer online.
    } else if (auto* delivery = std::get_if<wire::GrantDelivery>(&body)) {
        result = grants.receive(peer, delivery->token, delivery->root);
    } else if (auto* revocation = std::get_if<wire::GrantRevocation>(&body)) {
        result = grants.revoked_by(peer, grant_id(revocation->grant_id));
    } else if (auto* renewal = std::get_if<wire::GrantRenewal>(&body)) {
        wire::GrantRenewalResult answer;
        if (auto issued = grants.store->renew(grants.identity, grant_id(renewal->grant_id), peer, now_ms()))
            answer.token = std::move(issued->token);
        else answer.refusal = issued.error().to_string();
        return wire::send(*stream.send, wire::ControlFrame{wire::Control(std::move(answer))});
    } else {
        result = fail(NodeError::Kind::protocol, "not a request");
    }
    if (result) host.changed();
    std::optional<std::string> error;
    if (!result) error = result.error().to_string();
    if (auto r = wire::ack<wire::ControlFrame>(*stream.send, std::move(error)); !r) return r;
    return stream.send->shutdown();
}

std::optional<NodeId> Probe::probe(const SocketAddr& address) {
    auto done = std::make_shared<std::promise<std::optional<NodeId>>>();
    auto answered = done->get_future();
    std::thread([host = host_, address, done] {
        std::optional<NodeId> proven;
        if (auto conn = host->dial_any(address, wire::CONTROL)) {
            if (call(*host, *conn, presence())) proven = (*conn)->remote();
            (*conn)->close(0, "done");
        }
        done->set_value(proven);
    }).detach();
    // The dial and the call each get `wait_`; a probe still running past that is left to finish alone.
    if (answered.wait_for(wait_ * 2) != std::future_status::ready) return std::nullopt;
    return answered.get();
}

trust::Awaitable<RenewResult> RenewVia::renew(trust::PublicKey issuer, trust::GrantId grant) const {
    // The control call blocks; it runs on its own thread and completes on the caller's executor.
    co_return co_await asio::async_initiate<decltype(asio::use_awaitable), void(RenewResult)>(
        [host = host_, issuer, grant = std::move(grant)](auto handler) mutable {
            std::thread([host, issuer, grant = std::move(grant), handler = std::move(handler)]() mutable {
                auto result = renew_blocking(*host, issuer, grant);
                auto ex = asio::get_associated_executor(handler);
                asio::post(ex, [handler = std::move(handler), result = std::move(result)]() mutable {
                    std::move(handler)(std::move(result));
                });
            }).detach();
        },
        asio::use_awaitable);
}

} // namespace loa::p2p::control
