#pragma once

#include <memory>

#include "loa/contracts/login_channels.hpp"
#include "loa/settings/models/rpc_channel.hpp"

namespace loa::harness::omp {

class OmpOAuthLogin final : public contracts::LoginStrategy {
  public:
    OmpOAuthLogin(std::shared_ptr<settings::models::rpc::OmpRpc> rpc,
                  std::shared_ptr<contracts::UrlOpener> opener, settings::models::rpc::LoginProvider provider)
        : rpc(std::move(rpc)), opener(std::move(opener)), provider(std::move(provider)) {}
    std::shared_ptr<settings::models::rpc::OmpRpc> rpc;
    std::shared_ptr<contracts::UrlOpener> opener;
    settings::models::rpc::LoginProvider provider;
    [[nodiscard]] contracts::Awaitable<void> run(std::shared_ptr<contracts::LoginChannels> channels) override;

  private:
    [[nodiscard]] contracts::Awaitable<void> flow(std::shared_ptr<contracts::LoginChannels> channels);
};

} // namespace loa::harness::omp
