#include "loa/app/registry.hpp"

#include <algorithm>

namespace loa::app {

std::pair<Registry, Task> Registry::make(Ctx& ctx, std::vector<AreaBoot> boots) {
    Registry registry;
    std::vector<Task> tasks;
    for (const auto& boot : boots) {
        if (std::ranges::find(AREA_ORDER, boot.id) == AREA_ORDER.end())
            ctx.notify("No area " + std::string(boot.id) + " in the app's order");
        else if (std::ranges::count(boots, boot.id, &AreaBoot::id) > 1)
            ctx.notify("Area " + std::string(boot.id) + " is registered twice; the first one runs");
    }
    for (auto id : AREA_ORDER) {
        auto it = std::ranges::find(boots, id, &AreaBoot::id);
        if (it == boots.end()) continue;
        auto [area, task] = it->boot(ctx);
        registry.areas_.push_back(std::move(area));
        tasks.push_back(std::move(task));
    }
    return {std::move(registry), Task::batch(std::move(tasks))};
}

std::vector<std::string_view> Registry::ids() const {
    std::vector<std::string_view> out;
    for (const auto& area : areas_) out.push_back(area->id());
    return out;
}

Area* Registry::find(std::string_view id) {
    auto it = std::ranges::find_if(areas_, [&](const auto& area) { return area->id() == id; });
    return it == areas_.end() ? nullptr : it->get();
}

const Area* Registry::find(std::string_view id) const { return const_cast<Registry*>(this)->find(id); }

Task Registry::update(std::string_view id, AreaMsg message, Ctx& ctx) {
    if (auto* area = find(id)) return area->update(std::move(message), ctx);
    ctx.notify("No area " + std::string(id) + " for " + message.type_name());
    return Task::none();
}

std::vector<loa::view::session::composer::Command> Registry::slash_rows() const {
    std::vector<loa::view::session::composer::Command> rows;
    for (const auto& area : areas_) {
        auto each = area->slash_rows();
        rows.insert(rows.end(), std::make_move_iterator(each.begin()), std::make_move_iterator(each.end()));
    }
    return rows;
}

} // namespace loa::app
