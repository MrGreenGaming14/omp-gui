#include "loa/surfaces/hub/dom.hpp"

#include "loa/core/schema.hpp"
#include "loa/surfaces/hub/service.hpp"

namespace loa::surfaces::hub::dom {

core::Draft opened(core::NodeId parent, const loa::contracts::surface::SurfaceInfo& info, const Origin& origin) {
    core::Attrs attrs{
        {std::string(core::attr::URI), core::Value(info.uri.as_str())},
        {std::string(core::attr::MODE), core::Value(core::as_str(info.mode))},
        {std::string(core::attr::NAME), core::Value(info.name)},
        {std::string(core::attr::SERVER), core::Value(origin.server())},
        {std::string(core::attr::STATUS), core::Value(core::as_str(core::SurfaceStatus::Open))},
    };
    if (info.width > 0 && info.height > 0) {
        attrs.insert_or_assign(std::string(core::attr::WIDTH), core::Value(static_cast<std::int64_t>(info.width)));
        attrs.insert_or_assign(std::string(core::attr::HEIGHT), core::Value(static_cast<std::int64_t>(info.height)));
    }
    return core::Draft::insert(parent, core::ElementKind::Surface, std::move(attrs));
}

core::Draft closed(core::NodeId node) {
    return core::Draft::set(node, std::string(core::attr::STATUS), core::Value(core::as_str(core::SurfaceStatus::Closed)));
}

} // namespace loa::surfaces::hub::dom
