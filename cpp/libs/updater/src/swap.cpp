#include "loa/updater/swap.hpp"

#include <algorithm>
#include <filesystem>
#include <vector>

#include "util.hpp"

namespace loa::updater {

namespace stdfs = std::filesystem;

Result<std::optional<loa::settings::Active>> Swapper::active(std::string_view product) const {
    return detail::read_json<loa::settings::Active>(active_path(product), [](const detail::Json& v) -> detail::Decoded<loa::settings::Active> {
        auto decoded = loa::settings::from_json<loa::settings::Active>(v);
        if (!decoded) return std::unexpected(decoded.error().message());
        return std::move(*decoded);
    });
}

Path Swapper::active_path(std::string_view product) const { return stage::product_dir(root, product) / "active.json"; }

Result<Staged> Swapper::verified(std::string_view product, const Version& version,
                                std::optional<std::span<const VerifyingKey>> trusted) const {
    auto found = stage::staged(root, product, version);
    if (!found) return std::unexpected(found.error());
    if (!*found || (**found).manifest.product != product || (**found).manifest.version != version)
        return std::unexpected(Error::not_staged(product, version));
    const Staged& s = **found;
    if (trusted) {
        if (auto r = signature::verify_staged(s, *trusted); !r) return std::unexpected(r.error());
    } else if (product == app_product) {
        if (auto r = signature::verify_staged(s, {}); !r) return std::unexpected(r.error());
    }
    return std::move(**found);
}

Result<SwapOutcome> Swapper::try_swap(std::string_view product, const Version& to, std::optional<std::span<const VerifyingKey>> trusted,
                                     const Activity& activity) const {
    if (activity.turn_running()) return SwapOutcome::deferred();
    auto old = active(product);
    if (!old) return std::unexpected(old.error());
    if (*old && (**old).current == to) return SwapOutcome::already_active();
    auto valid = verified(product, to, trusted);
    if (!valid) return std::unexpected(valid.error());
    std::optional<Version> from;
    if (*old) from = (**old).current;
    const loa::settings::Active next{to, from};
    if (auto r = detail::write_json(active_path(product), loa::settings::to_json(next)); !r) return std::unexpected(r.error());
    prune(product, next);
    return SwapOutcome::swapped(std::move(from), to);
}

Result<SwapOutcome> Swapper::rollback(std::string_view product, std::optional<std::span<const VerifyingKey>> trusted,
                                     const Activity& activity) const {
    if (activity.turn_running()) return SwapOutcome::deferred();
    auto current = active(product);
    if (!current) return std::unexpected(current.error());
    if (!*current || !(**current).previous) return std::unexpected(Error::no_previous(product));
    const auto previous = *(**current).previous;
    auto valid = verified(product, previous, trusted);
    if (!valid) return std::unexpected(valid.error());
    const auto from = (**current).current;
    if (auto r = detail::write_json(active_path(product), loa::settings::to_json(loa::settings::Active{previous, from})); !r)
        return std::unexpected(r.error());
    return SwapOutcome::swapped(from, previous);
}

void Swapper::prune(std::string_view product, const loa::settings::Active& active) const {
    if (!active.previous) return;
    std::error_code ec;
    const auto dir = stage::product_dir(root, product);
    std::vector<std::pair<Version, Path>> older;
    for (stdfs::directory_iterator it(dir, ec), end; !ec && it != end; it.increment(ec)) {
        if (!it->is_directory(ec) || ec) {
            ec.clear();
            continue;
        }
        auto v = Version::parse(it->path().filename().string());
        if (v && *v < *active.previous && *v < active.current) older.emplace_back(std::move(*v), it->path());
    }
    std::ranges::sort(older, [](const auto& a, const auto& b) { return a.first > b.first; });
    for (std::size_t i = keep; i < older.size(); ++i) {
        stdfs::remove_all(older[i].second, ec);
        ec.clear(); // Rust warns but does not fail a successful swap on a pruning error.
    }
}

} // namespace loa::updater
