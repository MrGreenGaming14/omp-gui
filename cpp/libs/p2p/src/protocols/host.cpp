#include "loa/p2p/protocols/host.hpp"

#include <thread>

#include "loa/runtime/log.hpp"

namespace loa::p2p {

namespace {

class Served final : public ProtocolHandler {
  public:
    Served(std::weak_ptr<NodeHost> host, ServeStream serve) : host_(std::move(host)), serve_(std::move(serve)) {}

    void handle(Connection conn) override {
        while (auto stream = conn->accept_stream()) {
            auto host = host_.lock();
            if (!host) return;
            std::thread([host = std::move(host), serve = serve_, conn, stream = std::move(*stream)]() mutable {
                if (auto done = serve(*host, conn, std::move(stream)); !done)
                    runtime::log::debug("p2p", "peer " + conn->remote().to_string() +
                                                   " stream ended: " + done.error().to_string());
            }).detach();
        }
    }

  private:
    std::weak_ptr<NodeHost> host_;
    ServeStream serve_;
};

} // namespace

std::shared_ptr<ProtocolHandler> served(std::weak_ptr<NodeHost> host, ServeStream serve) {
    return std::make_shared<Served>(std::move(host), std::move(serve));
}

} // namespace loa::p2p
