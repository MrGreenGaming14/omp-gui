#include "loa/ui/app/local_fs.hpp"

#include "loa/runtime/gitignore.hpp"

namespace loa::ui::app {

std::expected<std::vector<loa::view::panel::port::Entry>, std::string> LocalFs::list(const std::filesystem::path& dir) const {
    auto listed = loa::runtime::gitignore::list_dir(dir);
    if (!listed) return std::unexpected(listed.error().message());
    std::vector<loa::view::panel::port::Entry> out;
    out.reserve(listed->size());
    for (auto& e : *listed) out.push_back({std::move(e.name), e.is_dir});
    return out;
}

} // namespace loa::ui::app
