#include "loa/p2p/protocols/projection.hpp"

#include <algorithm>
#include <format>
#include <limits>

#include "loa/bus/journal.hpp"
#include "loa/bus/store.hpp"
#include "loa/runtime/time.hpp"

namespace loa::p2p {

namespace {

using core::Draft;
using core::ElementKind;
using core::NodeId;
using trust::Action;
namespace attr = core::attr;

NodeError apply_error(const core::ApplyError& e) { return NodeError(NodeError::Kind::apply, core::to_string(e)); }

NodeError sequence_error(std::uint64_t last, std::uint64_t got) {
    return NodeError(NodeError::Kind::sequence, std::format("patch {} is not next after {}", got, last));
}

// Root attributes a peer sees: provenance always, the title with a whole-session grant.
bool root_attr_visible(std::string_view name, bool whole_session) {
    return name == attr::ID || name == attr::SCHEMA || name == attr::HARNESS || name == attr::NODE ||
           name == attr::CREATED || (whole_session && name == attr::TITLE);
}

core::Attrs root_attrs(const core::Attrs& attrs, bool whole) {
    core::Attrs out;
    for (const auto& [name, value] : attrs)
        if (root_attr_visible(name, whole)) out.insert_or_assign(name, value);
    return out;
}

} // namespace

Result<std::pair<Projection, std::vector<core::Patch>>> Projection::seeded(const bus::NodeKey& peer,
                                                                           const core::Dom& source,
                                                                           const Access& access) {
    Projection projection(peer);
    projection.source_ = source;
    const bool whole = access.allows(peer, source, Action::view, std::nullopt);
    std::vector<core::Patch> patches;
    const auto* root = source.root();
    const std::int64_t ts = root ? root->int_attr(attr::CREATED).value_or(0) : 0;
    for (const auto& node : source.iter()) {
        Draft draft;
        if (node.kind == ElementKind::Session) {
            draft = Draft{Draft::Insert{NodeId::DOCUMENT, std::nullopt, node.kind, root_attrs(node.attrs, whole), {}}};
        } else {
            auto parent = projection.ids_.find(node.parent);
            if (parent == projection.ids_.end()) continue;
            if (!access.allows(peer, source, Action::view, node.id)) continue;
            draft = Draft{Draft::Insert{parent->second, std::nullopt, node.kind, node.attrs, node.text}};
        }
        auto patch = projection.visible_.commit(std::move(draft), ts);
        if (!patch) return std::unexpected(apply_error(patch.error()));
        projection.ids_.insert_or_assign(node.id, patch->node);
        patches.push_back(std::move(*patch));
    }
    return std::pair{std::move(projection), std::move(patches)};
}

std::optional<NodeId> Projection::source_node(NodeId visible) const {
    for (const auto& [source, shown] : ids_)
        if (shown == visible) return source;
    return std::nullopt;
}

std::optional<Draft> Projection::source_draft(Draft draft) const {
    return std::visit(
        [&](auto&& d) -> std::optional<Draft> {
            using D = std::decay_t<decltype(d)>;
            if constexpr (std::is_same_v<D, Draft::Insert>) {
                auto parent = source_node(d.parent);
                if (!parent) return std::nullopt;
                // The peer counts only the children it sees; place the new
                // node after the source child it would follow.
                std::optional<std::uint32_t> index;
                if (d.index && *d.index == 0) index = 0;
                else if (d.index) {
                    const auto* shown = visible_.get(d.parent);
                    if (!shown || *d.index - 1 >= shown->children.size()) return std::nullopt;
                    auto prev = source_node(shown->children[*d.index - 1]);
                    if (!prev) return std::nullopt;
                    const auto* owner = source_.get(*parent);
                    if (!owner) return std::nullopt;
                    auto it = std::ranges::find(owner->children, *prev);
                    if (it == owner->children.end()) return std::nullopt;
                    const auto position = static_cast<std::size_t>(it - owner->children.begin()) + 1;
                    if (position > std::numeric_limits<std::uint32_t>::max()) return std::nullopt;
                    index = static_cast<std::uint32_t>(position);
                }
                return Draft{Draft::Insert{*parent, index, d.kind, std::move(d.attrs), std::move(d.text)}};
            } else {
                auto node = source_node(d.node);
                if (!node) return std::nullopt;
                d.node = *node;
                return Draft{std::move(d)};
            }
        },
        std::move(draft.v));
}

Result<Update> Projection::accept(const core::Patch& source_patch, const Access& access) {
    const bool is_remove = std::holds_alternative<core::op::Remove>(source_patch.op);
    std::vector<NodeId> removed_ids;
    if (is_remove)
        for (const auto& node : source_.descendants(source_patch.node)) removed_ids.push_back(node.id);
    const bool remove_allowed = is_remove && access.allows(peer_, source_, Action::view, source_patch.node);
    if (auto applied = source_.apply(source_patch); !applied) return std::unexpected(apply_error(applied.error()));
    const auto old = ids_.find(source_patch.node);
    const bool old_visible = old != ids_.end();
    const bool allowed = remove_allowed || access.allows(peer_, source_, Action::view, source_patch.node);
    Update update{source_patch.seq, std::nullopt};
    auto commit = [&](Draft draft) -> Result<> {
        auto patch = visible_.commit(std::move(draft), source_patch.ts);
        if (!patch) return std::unexpected(apply_error(patch.error()));
        update.patch = std::move(*patch);
        return {};
    };

    if (const auto* insert = std::get_if<core::op::Insert>(&source_patch.op)) {
        if (!allowed && insert->kind != ElementKind::Session) return update;
        Draft::Insert draft;
        if (insert->kind == ElementKind::Session) {
            // A turn-only grant must not reveal project paths, digest,
            // pre-existing context, or arbitrary root extension data.
            const bool whole = access.allows(peer_, source_, Action::view, std::nullopt);
            draft = {NodeId::DOCUMENT, std::nullopt, insert->kind, root_attrs(insert->attrs, whole), {}};
        } else {
            auto parent = ids_.find(insert->parent);
            if (parent == ids_.end()) return update;
            std::uint32_t preceding = 0;
            for (const auto* child : source_.children(insert->parent)) {
                if (child->id == source_patch.node) break;
                if (ids_.contains(child->id)) ++preceding;
            }
            draft = {parent->second, preceding, insert->kind, insert->attrs, insert->text};
        }
        if (auto r = commit(Draft{std::move(draft)}); !r) return std::unexpected(r.error());
        ids_.insert_or_assign(source_patch.node, update.patch->node);
    } else if (const auto* set = std::get_if<core::op::Set>(&source_patch.op)) {
        const auto* root = source_.root();
        if (old_visible && root && root->id == source_patch.node) {
            const bool whole = access.allows(peer_, source_, Action::view, std::nullopt);
            if (root_attr_visible(set->name, whole))
                if (auto r = commit(Draft{Draft::Set{old->second, set->name, set->value}}); !r)
                    return std::unexpected(r.error());
        } else if (allowed && old_visible) {
            if (auto r = commit(Draft{Draft::Set{old->second, set->name, set->value}}); !r)
                return std::unexpected(r.error());
        }
    } else if (const auto* append = std::get_if<core::op::Append>(&source_patch.op)) {
        if (allowed && old_visible)
            if (auto r = commit(Draft{Draft::Append{old->second, append->text}}); !r) return std::unexpected(r.error());
    } else if (old_visible) {
        const NodeId node = old->second;
        for (auto id : removed_ids) ids_.erase(id);
        if (auto r = commit(Draft{Draft::Remove{node}}); !r) return std::unexpected(r.error());
    }
    return update;
}

Result<Mirror> Mirror::make(std::shared_ptr<bus::SessionWriter> writer, std::uint64_t source_seq) {
    if (writer->is_writer()) return fail(NodeError::Kind::not_owner, {});
    return Mirror(std::move(writer), source_seq);
}

Result<Mirror> Mirror::seeded(std::shared_ptr<bus::SessionWriter> writer, std::uint64_t source_seq,
                              std::vector<core::Patch> patches) {
    if (writer->seq() != 0) return std::unexpected(sequence_error(writer->seq(), 0));
    auto mirror = make(std::move(writer), source_seq);
    if (!mirror) return mirror;
    for (auto& patch : patches)
        if (auto r = mirror->writer_->replicate(std::move(patch)); !r)
            return fail(NodeError::Kind::replicate, bus::to_string(r.error()));
    return mirror;
}

Result<> Mirror::receive(Update update) {
    if (update.source_seq != source_seq_ + 1) return std::unexpected(sequence_error(source_seq_, update.source_seq));
    if (update.patch)
        if (auto r = writer_->replicate(std::move(*update.patch)); !r)
            return fail(NodeError::Kind::replicate, bus::to_string(r.error()));
    source_seq_ = update.source_seq;
    return {};
}

Result<bus::Journal> fork_mirror_to_own_node(const bus::Store& store, std::span<const core::Patch> patches,
                                             std::uint64_t at_seq, std::string_view new_id,
                                             std::string_view local_node) {
    auto created = store.create(new_id);
    if (!created) {
        // A store that refuses the id (already present, invalid) is left untouched by `create` itself.
        const bool untouched = std::holds_alternative<bus::journal_error::InvalidSessionId>(created.error()) ||
                               std::holds_alternative<bus::journal_error::Exists>(created.error());
        if (!untouched) (void)store.remove(new_id);
        return fail(NodeError::Kind::journal, bus::to_string(created.error()));
    }
    auto built = [&]() -> Result<> {
        auto& journal = *created;
        auto journal_failed = [](const bus::JournalError& e) { return fail(NodeError::Kind::journal, bus::to_string(e)); };
        core::Dom dom;
        for (const auto& patch : patches) {
            if (patch.seq > at_seq) break;
            if (auto r = dom.apply(patch); !r) return std::unexpected(apply_error(r.error()));
            if (auto r = journal.append(patch); !r) return journal_failed(r.error());
        }
        if (dom.seq() != at_seq) return std::unexpected(sequence_error(dom.seq(), at_seq));
        const auto* root = dom.root();
        if (!root) return fail(NodeError::Kind::unavailable, "session has no root");
        const NodeId root_id = root->id;
        const std::string source(root->str_attr(attr::ID).value_or(""));
        const auto ts = runtime::time::now_ms();
        const auto forked_at = static_cast<std::int64_t>(std::min<std::uint64_t>(at_seq, std::numeric_limits<std::int64_t>::max()));
        Draft drafts[] = {
            Draft::set(root_id, std::string(attr::ID), core::Value(new_id)),
            Draft::set(root_id, std::string(attr::PARENT_SESSION), core::Value(source)),
            Draft::set(root_id, std::string(attr::FORKED_AT), core::Value(forked_at)),
            Draft::set(root_id, std::string(attr::NODE), core::Value(local_node)),
        };
        for (auto& draft : drafts) {
            auto patch = dom.commit(std::move(draft), ts);
            if (!patch) return std::unexpected(apply_error(patch.error()));
            if (auto r = journal.append_applied(*patch, dom); !r) return journal_failed(r.error());
        }
        if (auto r = journal.write_snapshot(dom); !r) return journal_failed(r.error());
        if (auto r = journal.sync(); !r) return journal_failed(r.error());
        return {};
    }();
    if (!built) {
        // Leave no half made session behind.
        { auto drop = std::move(*created); }
        (void)store.remove(new_id);
        return std::unexpected(built.error());
    }
    return std::move(*created);
}

} // namespace loa::p2p
