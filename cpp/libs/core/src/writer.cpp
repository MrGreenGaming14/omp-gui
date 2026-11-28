#include "loa/core/writer.hpp"

namespace loa::core {

namespace {
std::expected<void, CommitError> drop(std::expected<NodeId, CommitError> r) {
    if (!r) return std::unexpected(std::move(r.error()));
    return {};
}
} // namespace

std::expected<NodeId, CommitError> DomWriter::commit(Draft draft) const {
    auto patch = sink_->commit_at(std::move(draft), now());
    if (!patch) return std::unexpected(std::move(patch.error()));
    return patch->node;
}

std::expected<NodeId, CommitError> DomWriter::insert(NodeId parent, ElementKind kind, Attrs attrs,
                                                     std::string text) const {
    return commit(Draft::insert(parent, kind, std::move(attrs), std::move(text)));
}

std::expected<void, CommitError> DomWriter::set(NodeId node, std::string_view name, Value value) const {
    return drop(commit(Draft::set(node, std::string(name), std::move(value))));
}

std::expected<void, CommitError> DomWriter::clear(NodeId node, std::string_view name) const {
    return drop(commit(Draft::clear(node, std::string(name))));
}

std::expected<void, CommitError> DomWriter::remove(NodeId node) const { return drop(commit(Draft::remove(node))); }

std::expected<void, CommitError> DomWriter::append(NodeId node, std::string text) const {
    return drop(commit(Draft::append(node, std::move(text))));
}

std::expected<void, CommitError> DomWriter::set_text(NodeId node, std::string text) const {
    return drop(commit(Draft::set_text(node, std::move(text))));
}

std::expected<void, CommitError> DomWriter::close(NodeId node, Value status) const {
    if (auto ok = set(node, attr::STATUS, std::move(status)); !ok) return ok;
    return set(node, attr::ENDED, now());
}

std::expected<NodeId, CommitError> DomWriter::ext(NodeId parent, std::string_view harness, std::string_view kind,
                                                  std::string raw) const {
    Attrs attrs{{std::string(attr::HARNESS), Value(harness)}, {std::string(attr::TYPE), Value(kind)}};
    return insert(parent, ElementKind::Ext, std::move(attrs), std::move(raw));
}

} // namespace loa::core
