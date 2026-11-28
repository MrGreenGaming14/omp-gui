#include "loa/core/patch.hpp"

namespace loa::core {

Draft Draft::insert(NodeId parent, ElementKind kind, Attrs attrs, std::string text) {
    return {Insert{parent, std::nullopt, kind, std::move(attrs), std::move(text)}};
}

Draft Draft::insert_at(NodeId parent, std::uint32_t index, ElementKind kind, Attrs attrs, std::string text) {
    return {Insert{parent, index, kind, std::move(attrs), std::move(text)}};
}

Draft Draft::set(NodeId node, std::string name, Value value) {
    return {Set{node, std::move(name), std::move(value)}};
}

Draft Draft::clear(NodeId node, std::string name) { return {Set{node, std::move(name), std::nullopt}}; }

Draft Draft::set_text(NodeId node, std::string text) { return set(node, std::string(TEXT), Value(std::move(text))); }

Draft Draft::append(NodeId node, std::string text) { return {Append{node, std::move(text)}}; }

Draft Draft::remove(NodeId node) { return {Remove{node}}; }

} // namespace loa::core
