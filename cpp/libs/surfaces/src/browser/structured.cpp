#include "loa/surfaces/browser/structured.hpp"

#include <algorithm>
#include <array>
#include <string_view>
#include <unordered_map>

namespace loa::surfaces::browser {

namespace json = loa::runtime::json;

namespace {

// Accessibility roles the agent can act on.
constexpr std::array<std::string_view, 18> INTERACTIVE_ROLES = {
    "button",   "checkbox",   "combobox",   "link",   "listbox", "menuitem", "menuitemcheckbox", "menuitemradio",
    "option",   "radio",      "searchbox",  "slider", "spinbutton", "switch", "tab", "textbox", "treeitem",
    "MenuListPopup",
};

double number(const json::Value* v, double fallback = 0) {
    if (!v) return fallback;
    if (auto d = v->as_f64()) return *d;
    return fallback;
}

std::string trimmed(std::string_view s) {
    constexpr std::string_view ws = " \t\n\r\f\v";
    const auto b = s.find_first_not_of(ws);
    if (b == std::string_view::npos) return {};
    return std::string(s.substr(b, s.find_last_not_of(ws) - b + 1));
}

// Layout box of every laid out node by backend node id, in viewport coordinates.
std::unordered_map<std::int64_t, Bounds> layout_boxes(const json::Value& document) {
    std::unordered_map<std::int64_t, Bounds> boxes;
    const double scroll_x = number(document.find("scrollOffsetX"));
    const double scroll_y = number(document.find("scrollOffsetY"));
    const auto* nodes = document.find("nodes");
    const auto* backend = nodes ? nodes->find("backendNodeId") : nullptr;
    const auto* backend_ids = backend ? backend->as_array() : nullptr;
    if (!backend_ids) return boxes;
    const auto* layout = document.find("layout");
    const auto* index = layout ? layout->find("nodeIndex") : nullptr;
    const auto* bounds = layout ? layout->find("bounds") : nullptr;
    if (!index || !bounds || !index->as_array() || !bounds->as_array()) return boxes;
    const auto& idx = *index->as_array();
    const auto& rects = *bounds->as_array();
    for (std::size_t i = 0; i < std::min(idx.size(), rects.size()); ++i) {
        const auto node = idx[i].as_i64();
        const auto* rect = rects[i].as_array();
        if (!node || *node < 0 || std::size_t(*node) >= backend_ids->size() || !rect || rect->size() != 4) continue;
        const auto id = (*backend_ids)[std::size_t(*node)].as_i64();
        if (!id) continue;
        // Text runs of an element come later with the same node; keep the element's own box.
        boxes.try_emplace(*id, Bounds{float(number(&(*rect)[0]) - scroll_x), float(number(&(*rect)[1]) - scroll_y),
                                      float(number(&(*rect)[2])), float(number(&(*rect)[3]))});
    }
    return boxes;
}

const std::string* value_string(const json::Value& node, std::string_view field) {
    const auto* f = node.find(field);
    const auto* v = f ? f->find("value") : nullptr;
    return v ? v->as_string() : nullptr;
}

} // namespace

json::Value PageInfo::to_json() const { return json::Object{{"url", url}, {"title", title}}; }

json::Value Bounds::to_json() const {
    return json::Object{{"x", double(x)}, {"y", double(y)}, {"width", double(width)}, {"height", double(height)}};
}

json::Value OutlineNode::to_json() const {
    return json::Object{{"role", role}, {"name", name}, {"bounds", bounds.to_json()}, {"visible", visible}};
}

std::vector<OutlineNode> outline_from(const json::Value& ax_nodes, const json::Value& snapshot, std::uint32_t width,
                                      std::uint32_t height) {
    std::vector<OutlineNode> out;
    const auto* documents = snapshot.find("documents");
    if (!documents || !documents->as_array() || documents->as_array()->empty()) return out;
    const auto boxes = layout_boxes(documents->as_array()->front());
    const auto* nodes = ax_nodes.as_array();
    if (!nodes) return out;
    for (const auto& node : *nodes) {
        if (const auto* ignored = node.find("ignored"); ignored && ignored->as_bool() && *ignored->as_bool()) continue;
        const auto* role = value_string(node, "role");
        if (!role || std::ranges::find(INTERACTIVE_ROLES, std::string_view(*role)) == INTERACTIVE_ROLES.end()) continue;
        const auto* backend = node.find("backendDOMNodeId");
        const auto id = backend ? backend->as_i64() : std::nullopt;
        if (!id) continue;
        const auto box = boxes.find(*id);
        if (box == boxes.end()) continue;
        const auto b = box->second;
        if (b.width <= 0 || b.height <= 0) continue;
        const auto* name = value_string(node, "name");
        const bool visible = b.x < float(width) && b.y < float(height) && b.x + b.width > 0 && b.y + b.height > 0;
        out.push_back({*role, trimmed(name ? *name : std::string()), b, visible});
    }
    return out;
}

} // namespace loa::surfaces::browser
