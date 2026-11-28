#include "loa/ui/session/transcript/items.hpp"

#include <array>

namespace loa::ui::session::transcript {

namespace {

namespace core = loa::core;
namespace attr = loa::core::attr;
using core::ElementKind;

constexpr std::string_view OMP_HARNESS = "omp";
constexpr std::string_view SUBAGENT_PROGRESS_EXT = "subagent_progress";

bool is_suppressed_ext(const core::Node& node) {
    const auto kind = node.str_attr(attr::TYPE);
    return kind == COMMANDS_EXT || (node.str_attr(attr::HARNESS) == OMP_HARNESS && kind == SUBAGENT_PROGRESS_EXT);
}

ItemKind message_kind(const core::Node& node) {
    const auto role = node.str_attr(attr::ROLE).and_then([](std::string_view s) { return core::parse<core::Role>(s); });
    if (role == core::Role::User) return ItemKind::User;
    if (role == core::Role::System) return ItemKind::System;
    const auto kind = node.str_attr(attr::KIND).and_then([](std::string_view s) { return core::parse<core::MessageKind>(s); });
    return kind == core::MessageKind::Thinking ? ItemKind::Thinking : ItemKind::Assistant;
}

struct Emitter {
    const core::Dom& dom;
    NodeId top;
    const Own& own;

    void run(std::vector<Item>& out, const core::Node& node) {
        ItemKind kind{};
        switch (node.kind) {
        case ElementKind::Turn: return children(out, node);
        case ElementKind::Call: return call(out, node);
        case ElementKind::Message: kind = message_kind(node); break;
        case ElementKind::Decision: kind = ItemKind::Decision; break;
        case ElementKind::Job: kind = ItemKind::Job; break;
        case ElementKind::Todo: kind = ItemKind::Todo; break;
        case ElementKind::Surface: kind = ItemKind::Surface; break;
        case ElementKind::Compaction: kind = ItemKind::Compaction; break;
        case ElementKind::Ext:
            if (is_suppressed_ext(node)) return;
            kind = ItemKind::Ext;
            break;
        default: return;
        }
        out.push_back(Item{node.id, top, kind, {}});
        own(node.id, node.id);
        children(out, node);
    }

    void children(std::vector<Item>& out, const core::Node& node) {
        for (const auto* child : dom.children(node.id)) run(out, *child);
    }

    void call(std::vector<Item>& out, const core::Node& node) {
        const auto tool = node.str_attr(attr::TOOL);
        NodeId key = node.id;
        bool joined = false;
        if (!out.empty()) {
            auto& last = out.back();
            if (last.kind == ItemKind::Calls && last.top == top) {
                const auto* first = dom.get(last.calls.front());
                const auto first_tool = first ? first->str_attr(attr::TOOL) : std::nullopt;
                if (first_tool == tool) {
                    last.calls.push_back(node.id);
                    key = last.key;
                    joined = true;
                }
            }
        }
        if (!joined) out.push_back(Item{node.id, top, ItemKind::Calls, {node.id}});
        own(node.id, key);
        for (const auto* child : dom.children(node.id)) {
            switch (child->kind) {
            case ElementKind::Input:
            case ElementKind::Result:
            case ElementKind::Usage:
                for (const auto& part : dom.descendants(child->id)) own(part.id, key);
                break;
            default: run(out, *child);
            }
        }
    }
};

} // namespace

std::string_view name(ItemKind k) {
    static constexpr std::array<std::string_view, 11> NAMES = {"User", "Assistant", "Thinking", "System", "Calls", "Decision",
                                                               "Job", "Todo", "Surface", "Compaction", "Ext"};
    return NAMES[static_cast<std::size_t>(k)];
}

void flatten(const core::Dom& dom, NodeId top, const core::Node& node, std::vector<Item>& out, const Own& own) {
    Emitter{dom, top, own}.run(out, node);
}

std::optional<NodeId> top_of(const core::Dom& dom, NodeId id) {
    const auto* top = dom.get(id);
    if (!top || top->kind == ElementKind::Session) return std::nullopt;
    for (const auto* ancestor : dom.ancestors(id)) {
        if (ancestor->kind == ElementKind::Session) return top->id;
        top = ancestor;
    }
    return std::nullopt;
}

} // namespace loa::ui::session::transcript
