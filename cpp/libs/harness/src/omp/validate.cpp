#include "loa/harness/omp/validate.hpp"

#include <asio/awaitable.hpp>

#include <fstream>
#include <utility>

#include "loa/runtime/tempfile.hpp"

namespace loa::harness::omp {
namespace {
std::string trim(std::string text) {
    const auto first = text.find_first_not_of(" \t\r\n");
    if (first == std::string::npos) return {};
    const auto last = text.find_last_not_of(" \t\r\n");
    return text.substr(first, last - first + 1);
}
} // namespace

contracts::Awaitable<settings::models::Result<std::string>> OmpSettingsValidator::command(
    std::string text, std::string_view file, std::vector<std::string> args) const {
    namespace models = settings::models;
    auto dir = runtime::TempDir::create();
    if (!dir) co_return std::unexpected(models::Error::io("temporary omp config", dir.error()));
    const auto path = dir->path() / file;
    {
        std::ofstream out(path, std::ios::binary | std::ios::trunc);
        if (!out || !(out << text) || !(out.flush()))
            co_return std::unexpected(models::Error::io(path, loa::Error(loa::ErrorCode::io, "write omp candidate")));
    }
    auto spec = contracts::CommandSpec::make(program, std::move(args));
    spec.env = {{"PI_CODING_AGENT_DIR", dir->path().string()}, {"OMP_PROFILE", "default"}, {"PI_CONFIG_FILES", ""}};
    spec.current_dir = dir->path();
    auto result = co_await runner->output(std::move(spec));
    if (!result) co_return std::unexpected(result.error());
    if (result->code != 0 || (file == "models.yml" && result->err.find("models.yml") != std::string::npos))
        co_return std::unexpected(models::Error::rejected(std::string(file), trim(result->err)));
    co_return std::move(result->out);
}

contracts::Awaitable<settings::models::Result<>> OmpSettingsValidator::check_config(
    std::string text, std::vector<std::pair<std::string, runtime::json::Value>> expected) const {
    auto out = co_await command(std::move(text), "config.yml", {"config", "list", "--scope", "global", "--json"});
    if (!out) co_return std::unexpected(out.error());
    auto parsed = runtime::json::parse(*out);
    if (!parsed) co_return std::unexpected(settings::models::Error::parse("omp config list --json", parsed.error().message()));
    for (const auto& [key, value] : expected) {
        const auto* item = parsed->find(key);
        const auto* actual = item ? item->find("value") : nullptr;
        if ((actual ? *actual : runtime::json::Value{}) != value)
            co_return std::unexpected(settings::models::Error::rejected("config.yml", "omp did not accept setting " + key));
    }
    co_return settings::models::Result<>{};
}

contracts::Awaitable<settings::models::Result<>> OmpSettingsValidator::check_models(std::string text) const {
    auto result = co_await command(std::move(text), "models.yml", {"models", "ls", "--json", "--no-extensions"});
    if (!result) co_return std::unexpected(result.error());
    co_return settings::models::Result<>{};
}

} // namespace loa::harness::omp
