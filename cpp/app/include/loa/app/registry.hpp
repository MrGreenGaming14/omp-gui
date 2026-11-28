#pragma once

// The feature areas (port of `loa-app/src/areas/mod.rs`): every area's state, in the fixed
// order the app calls them.

#include <array>
#include <functional>
#include <memory>
#include <span>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/app/area.hpp"
#include "loa/app/routes.hpp"

namespace loa::app {

// Every area, in the order the app calls them.
inline constexpr std::array<std::string_view, 12> AREA_ORDER{
    area_id::HARNESS, area_id::SESSIONS, area_id::DECISIONS, area_id::TOOLS,   area_id::SURFACES, area_id::MODELS,
    area_id::PROJECTS, area_id::PEERS,   area_id::UPDATER,   area_id::SANDBOX, area_id::AGENTS,   area_id::WORKFLOWS};

// Makes an area and its first task.
struct AreaBoot {
    std::string_view id;
    std::function<std::pair<std::unique_ptr<Area>, Task>(Ctx&)> boot;
};

// The boot of `A` (a `TypedArea` with `static std::pair<std::unique_ptr<A>, Task> make(Ctx&)`).
template <typename A>
[[nodiscard]] AreaBoot boot() {
    return {A::ID, [](Ctx& ctx) -> std::pair<std::unique_ptr<Area>, Task> {
                auto [area, task] = A::make(ctx);
                return {std::move(area), std::move(task)};
            }};
}

// The areas compiled into `loa-qt`, each registered by its area's issue.
[[nodiscard]] std::vector<AreaBoot> builtin_areas();

// Every area's state, in `AREA_ORDER`.
class Registry {
  public:
    // Boots `boots` in `AREA_ORDER`, whatever order they come in; an id `AREA_ORDER` doesn't
    // name, or a second boot of one id, is refused with a notice.
    [[nodiscard]] static std::pair<Registry, Task> make(Ctx& ctx, std::vector<AreaBoot> boots);

    [[nodiscard]] std::span<const std::unique_ptr<Area>> iter() const { return areas_; }
    [[nodiscard]] std::span<std::unique_ptr<Area>> iter_mut() { return areas_; }
    // The ids of the areas, in call order.
    [[nodiscard]] std::vector<std::string_view> ids() const;

    // The area with id `id`.
    [[nodiscard]] Area* find(std::string_view id);
    [[nodiscard]] const Area* find(std::string_view id) const;

    // The area of type `A`; null when it isn't registered.
    template <typename A>
    [[nodiscard]] A* get() const {
        auto* area = find(A::ID);
        return area ? dynamic_cast<A*>(const_cast<Area*>(area)) : nullptr;
    }

    // The first area implementing port `P`; null when none does.
    template <typename P>
    [[nodiscard]] P* port() const {
        for (const auto& area : areas_)
            if (auto* p = dynamic_cast<P*>(area.get())) return p;
        return nullptr;
    }

    // Hands `message` to area `id`.
    Task update(std::string_view id, AreaMsg message, Ctx& ctx);

    // Every area's `/` commands, in order.
    [[nodiscard]] std::vector<loa::view::session::composer::Command> slash_rows() const;

  private:
    std::vector<std::unique_ptr<Area>> areas_;
};

} // namespace loa::app
