#pragma once

// Owner-ordered patch sharing (Rust `protocols::projection`): a peer sees an
// independently numbered projection of the owner's journal, never the patches
// its grant hides.

#include <cstdint>
#include <memory>
#include <optional>
#include <span>
#include <string_view>
#include <unordered_map>
#include <vector>

#include "loa/bus/writer.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/patch.hpp"
#include "loa/p2p/error.hpp"
#include "loa/trust/scope.hpp"

namespace loa::bus {
class Journal;
class Store;
} // namespace loa::bus

namespace loa::p2p {

// Port: whether `peer` may take `action` on the session or on `node` in it.
class Access {
  public:
    virtual ~Access() = default;
    [[nodiscard]] virtual bool allows(const bus::NodeKey& peer, const core::Dom& dom, trust::Action action,
                                      std::optional<core::NodeId> node) const = 0;
};

// A source patch and what it changed in the projection; skipped ones still advance `source_seq`.
struct Update {
    std::uint64_t source_seq = 0;
    std::optional<core::Patch> patch;
    friend bool operator==(const Update&, const Update&) = default;
};

// Filters the source journal in order, from a `Projection::seeded` start.
class Projection {
  public:
    explicit Projection(bus::NodeKey peer) : peer_(peer) {}

    // A projection of the owner's current DOM and the patches that build the peer's view.
    [[nodiscard]] static Result<std::pair<Projection, std::vector<core::Patch>>> seeded(const bus::NodeKey& peer,
                                                                                         const core::Dom& source,
                                                                                         const Access& access);

    [[nodiscard]] std::uint64_t source_seq() const noexcept { return source_.seq(); }
    [[nodiscard]] const core::Dom& visible_dom() const noexcept { return visible_; }

    // The owner's id of the node the peer sees as `visible`.
    [[nodiscard]] std::optional<core::NodeId> source_node(core::NodeId visible) const;
    // A draft against the visible DOM in the owner's ids; none when it names a hidden node.
    [[nodiscard]] std::optional<core::Draft> source_draft(core::Draft draft) const;

    [[nodiscard]] Result<Update> accept(const core::Patch& source_patch, const Access& access);

  private:
    bus::NodeKey peer_;
    core::Dom source_;
    core::Dom visible_;
    // Source id -> visible id.
    std::unordered_map<core::NodeId, core::NodeId> ids_;
};

// A read-only copy of a peer's session: its seed, then every `Update` in order, gaps refused.
class Mirror {
  public:
    // Fails with `not_owner` on a writer this node orders.
    [[nodiscard]] static Result<Mirror> make(std::shared_ptr<bus::SessionWriter> writer, std::uint64_t source_seq);
    // A mirror on an empty `writer` built from the owner's seed patches.
    [[nodiscard]] static Result<Mirror> seeded(std::shared_ptr<bus::SessionWriter> writer, std::uint64_t source_seq,
                                               std::vector<core::Patch> patches);

    [[nodiscard]] std::uint64_t source_seq() const noexcept { return source_seq_; }
    [[nodiscard]] const std::shared_ptr<bus::SessionWriter>& writer() const noexcept { return writer_; }

    Result<> receive(Update update);

  private:
    Mirror(std::shared_ptr<bus::SessionWriter> writer, std::uint64_t source_seq)
        : writer_(std::move(writer)), source_seq_(source_seq) {}

    std::shared_ptr<bus::SessionWriter> writer_;
    std::uint64_t source_seq_;
};

// Copies mirrored `patches` up to `at_seq` into a new journal this node owns, re-rooted as a fork.
[[nodiscard]] Result<bus::Journal> fork_mirror_to_own_node(const bus::Store& store, std::span<const core::Patch> patches,
                                                           std::uint64_t at_seq, std::string_view new_id,
                                                           std::string_view local_node);

} // namespace loa::p2p
