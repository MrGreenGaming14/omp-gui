#pragma once

// The journal-backed home of every session on this node (port of
// `loa_session::library`): live writers on the bus persisted through their
// journals, plus the `SessionStore` port that fork, rewind, compaction and
// subagents create sessions through.

#include <cstdint>
#include <filesystem>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/bus/bus.hpp"
#include "loa/bus/store.hpp"
#include "loa/bus/writer.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/core/dom.hpp"
#include "loa/session/session.hpp"

namespace loa::session {

// One live session: the writer every change goes through and the journal it
// persists to, for reads of earlier states and forks.
struct Live {
    std::shared_ptr<bus::SessionWriter> writer;
    std::shared_ptr<bus::JournalWriter> journal;
};

// The sessions under one journal `Store`, opened as writers on one `Bus`.
class Library final : public SessionStore {
  public:
    // `local` owns every session this library creates or opens; peer requests pass `authorize`.
    Library(bus::Store store, std::shared_ptr<bus::Bus> bus, bus::NodeKey local,
            std::shared_ptr<const bus::Authorize> authorize);
    Library(const Library&) = delete;
    Library& operator=(const Library&) = delete;

    Library& with_sync(bus::SyncPolicy sync) {
        sync_ = sync;
        return *this;
    }

    [[nodiscard]] const bus::Store& store() const noexcept { return store_; }
    [[nodiscard]] const std::shared_ptr<bus::Bus>& bus() const noexcept { return bus_; }

    // Creates an empty session and opens its writer.
    [[nodiscard]] Result<Live> create(std::string_view id) const;
    // The live session `id`, opening it from its journal when it isn't live.
    [[nodiscard]] Result<Live> open(std::string_view id) const;
    // The session if it is live.
    [[nodiscard]] std::optional<Live> live(std::string_view id) const;
    // Closes a live session: its writer refuses commits, its journal is synced
    // and released. Returns whether it was live.
    [[nodiscard]] Result<bool> close(std::string_view id) const;
    // Closes and deletes a session. Its forks keep working.
    [[nodiscard]] Result<> remove(std::string_view id) const;

    // The current DOM of a session, live or not.
    [[nodiscard]] Result<core::Dom> load(std::string_view id) const;
    // The session's DOM right after patch `seq`.
    [[nodiscard]] Result<core::Dom> load_at(std::string_view id, std::uint64_t seq) const;
    // Sequence of the patch that inserted each `<turn>` directly under the
    // session root, in journal order. Turns removed later are left out.
    [[nodiscard]] Result<std::vector<std::pair<core::NodeId, std::uint64_t>>> turn_seqs(std::string_view id) const;
    // Sequence of the patch that inserted `node`.
    [[nodiscard]] Result<std::optional<std::uint64_t>> inserted_at(std::string_view id, core::NodeId node) const;
    // The state right before the patch that inserted the turn holding `node`.
    [[nodiscard]] Result<core::Dom> before_turn(std::string_view id, core::NodeId node) const;
    // The state right before the turn after `turn` started, or the current one.
    [[nodiscard]] Result<core::Dom> through_turn(std::string_view id, core::NodeId turn) const;

    // Reads a native harness session into a new session `id` through `adapter`.
    [[nodiscard]] Result<Live> import(const contracts::Adapter& adapter, const std::filesystem::path& native,
                                      std::string_view id, std::string_view node) const;

    using StagedLoad = std::function<Result<>(const std::filesystem::path& staging,
                                              const std::filesystem::path& target, core::PatchSink& sink)>;
    // Builds a native import and its assets out of sight, then exposes the
    // complete journal in one rename.
    [[nodiscard]] Result<> import_staged(std::string_view id, std::string_view node, std::string_view project,
                                         const StagedLoad& load) const;

    // Imports a subagent's native session file as a child of `parent`, linked
    // at the patch that inserted its `<job>`; reused until the file changes.
    [[nodiscard]] Result<std::string> import_child(const contracts::Adapter& adapter, std::string_view parent,
                                                   core::NodeId job, const std::filesystem::path& native,
                                                   std::string_view node) const;

    // Writes session `id` as a native session of `adapter`'s harness.
    [[nodiscard]] Result<> export_(const contracts::Adapter& adapter, std::string_view id,
                                   const std::filesystem::path& out) const;
    // Writes session `id` as debugging XML.
    [[nodiscard]] Result<> export_xml(std::string_view id, const std::filesystem::path& out) const;

    Awaitable<Result<std::pair<core::Dom, std::shared_ptr<core::PatchSink>>>> fork(
        std::string source_id, std::uint64_t seq, std::string new_id) const override;
    Awaitable<Result<std::shared_ptr<core::PatchSink>>> fresh(std::string new_id) const override;

    // Synchronous body of `SessionStore::fork`.
    [[nodiscard]] Result<std::pair<core::Dom, std::shared_ptr<core::PatchSink>>> fork_now(
        std::string_view source_id, std::uint64_t seq, std::string_view new_id) const;

  private:
    [[nodiscard]] Result<Live> attach(std::string_view id, bus::Journal journal, core::Dom dom) const;
    [[nodiscard]] Result<> each_patch(std::string_view id, const std::function<void(const core::Patch&)>& f) const;
    [[nodiscard]] Result<Live> import_with(std::string_view id, std::string_view node,
                                           std::optional<std::string_view> project,
                                           const std::function<Result<>(const Live&)>& load) const;

    bus::Store store_;
    std::shared_ptr<bus::Bus> bus_;
    bus::NodeKey local_;
    std::shared_ptr<const bus::Authorize> authorize_;
    bus::SyncPolicy sync_;
    mutable std::mutex mutex_;
    mutable std::map<std::string, std::shared_ptr<bus::JournalWriter>, std::less<>> journals_;
};

} // namespace loa::session
