#include "loa/app/adapters/models.hpp"

#include "loa/harness/account_support/opener.hpp"
#include "loa/harness/account_support/process.hpp"
#include "loa/harness/omp/validate.hpp"
#include "loa/settings/models/keys.hpp"

namespace loa::app::adapters {

namespace omp = loa::harness::omp;
namespace account_support = loa::harness::account_support;

models::Parts models_parts(const omp::OmpLaunch& launch, const app_runtime::Harnesses& harnesses,
                           const std::vector<std::shared_ptr<contracts::HarnessFactory>>& factories) {
    auto rpc = std::make_shared<omp::OmpService>(launch);
    auto opener = std::make_shared<account_support::SystemUrlOpener>();
    auto omp_account = std::make_shared<omp::OmpAccount>(rpc, opener, launch);
    std::vector<std::shared_ptr<contracts::HarnessAccount>> accounts;
    for (const auto& factory : factories) {
        if (factory->id() == models::OMP) continue;
        std::optional<std::filesystem::path> executable;
        if (const auto* harness = harnesses.get(factory->id())) executable = harness->executable;
        if (auto account = factory->account(executable)) accounts.push_back(std::move(account));
    }
    return models::Parts{
        .rpc = rpc,
        .omp_account = omp_account,
        .files = std::make_shared<omp::OmpLaunch>(launch),
        .validator = std::make_shared<omp::OmpSettingsValidator>(
            launch.executable, std::make_shared<account_support::AsioProcessRunner>()),
        .opener = opener,
        .harnesses = std::move(accounts),
        .environment = std::make_shared<settings::models::keys::ProcessEnvironment>(),
    };
}

} // namespace loa::app::adapters
