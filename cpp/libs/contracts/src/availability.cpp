#include "loa/contracts/availability.hpp"

#include <format>

#include "loa/runtime/exec.hpp"

namespace loa::contracts {

Availability Availability::probe(const std::filesystem::path& program, const std::vector<std::string>& version_args) {
    namespace exec = loa::runtime::exec;
    auto exe = exec::which(program);
    if (!exe) return unavailable(AvailabilityStatus::NotInstalled, std::format("`{}` not found", program.string()));
    auto stdout_text = exec::probe_cli(*exe, version_args, PROBE_TIMEOUT);
    if (!stdout_text) {
        auto failed = unavailable(AvailabilityStatus::Failed, stdout_text.error().to_string());
        failed.executable = std::move(*exe);
        return failed;
    }
    return ready(std::move(*exe), exec::parse_version(*stdout_text));
}

} // namespace loa::contracts
