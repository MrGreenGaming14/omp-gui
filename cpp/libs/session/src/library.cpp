#include "loa/session/library.hpp"

#include <asio.hpp>

#include <cerrno>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <limits>

#include "detail.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/xml.hpp"

namespace loa::session {

namespace fs = std::filesystem;
using core::Draft;
using core::ElementKind;
using core::NodeId;
using core::Value;
namespace attr = core::attr;

namespace {

// Root attribute of an imported child: modification time (unix ms) of the
// native file it was read from.
constexpr std::string_view SOURCE_MODIFIED = "x-loa-source-modified";

// Stable session id for a native child session file (FNV-1a over the path bytes).
std::string child_id(const fs::path& path) {
    std::uint64_t hash = 0xcbf29ce484222325ULL;
    for (const unsigned char byte : path.native()) {
        hash ^= byte;
        hash *= 0x100000001b3ULL;
    }
    char out[32];
    std::snprintf(out, sizeof out, "child-%016llx", static_cast<unsigned long long>(hash));
    return out;
}

template <typename T>
Result<T> journal(bus::JournalResult<T> r) {
    if (!r) return std::unexpected(Error::journal(r.error()));
    if constexpr (std::is_void_v<T>) return {};
    else return std::move(*r);
}

std::int64_t modified_ms(const fs::path& path) {
    std::error_code ec;
    const auto time = fs::last_write_time(path, ec);
    if (ec) return 0;
    const auto sys = std::chrono::file_clock::to_sys(time);
    const auto ms = std::chrono::duration_cast<std::chrono::milliseconds>(sys.time_since_epoch()).count();
    return ms < 0 ? 0 : ms;
}

} // namespace

Library::Library(bus::Store store, std::shared_ptr<bus::Bus> bus, bus::NodeKey local,
                 std::shared_ptr<const bus::Authorize> authorize)
    : store_(std::move(store)), bus_(std::move(bus)), local_(local), authorize_(std::move(authorize)) {}

Result<Live> Library::create(std::string_view id) const {
    auto created = journal(store_.create(id));
    if (!created) return std::unexpected(created.error());
    return attach(id, std::move(*created), core::Dom());
}

Result<Live> Library::open(std::string_view id) const {
    if (auto found = live(id)) return std::move(*found);
    auto opened = journal(store_.open(id));
    if (!opened) return std::unexpected(opened.error());
    auto dom = journal(opened->first.load());
    if (!dom) return std::unexpected(dom.error());
    return attach(id, std::move(opened->first), std::move(*dom));
}

Result<Live> Library::attach(std::string_view id, bus::Journal journal, core::Dom dom) const {
    auto writer_journal = std::make_shared<bus::JournalWriter>(std::move(journal), sync_);
    bus::WriterConfig config{
        .session = bus::SessionId(id),
        .owner = local_,
        .local = local_,
        .dom = std::move(dom),
        .persist = std::make_unique<bus::JournalPersist>(writer_journal),
        .authorize = authorize_,
    };
    auto writer = bus_->open(std::move(config));
    if (!writer) return std::unexpected(Error::bus(writer.error()));
    {
        std::scoped_lock lock(mutex_);
        journals_.insert_or_assign(std::string(id), writer_journal);
    }
    return Live{std::move(*writer), std::move(writer_journal)};
}

std::optional<Live> Library::live(std::string_view id) const {
    std::shared_ptr<bus::JournalWriter> journal;
    {
        std::scoped_lock lock(mutex_);
        auto it = journals_.find(id);
        if (it == journals_.end()) return std::nullopt;
        journal = it->second;
    }
    auto writer = bus_->get(id);
    if (!writer) return std::nullopt;
    return Live{std::move(writer), std::move(journal)};
}

Result<bool> Library::close(std::string_view id) const {
    std::shared_ptr<bus::JournalWriter> journal;
    {
        std::scoped_lock lock(mutex_);
        if (auto it = journals_.find(id); it != journals_.end()) {
            journal = std::move(it->second);
            journals_.erase(it);
        }
    }
    auto writer = bus_->close(id);
    if (journal)
        if (auto r = journal->close(); !r) return std::unexpected(Error::journal(r.error()));
    return journal != nullptr || writer != nullptr;
}

Result<> Library::remove(std::string_view id) const {
    if (auto r = close(id); !r) return std::unexpected(r.error());
    return journal(store_.remove(id));
}

Result<core::Dom> Library::load(std::string_view id) const {
    if (auto found = live(id)) return found->writer->with_dom([](const core::Dom& dom) { return dom; });
    auto opened = journal(store_.open(id));
    if (!opened) return std::unexpected(opened.error());
    return journal(opened->first.load());
}

Result<core::Dom> Library::load_at(std::string_view id, std::uint64_t seq) const {
    if (auto found = live(id)) return journal(found->journal->load_at(seq));
    auto opened = journal(store_.open(id));
    if (!opened) return std::unexpected(opened.error());
    return journal(opened->first.load_at(seq));
}

Result<> Library::each_patch(std::string_view id, const std::function<void(const core::Patch&)>& f) const {
    std::optional<bus::Journal> held;
    bus::JournalResult<bus::Patches> patches = std::unexpected(bus::JournalError(bus::journal_error::Closed{}));
    if (auto found = live(id)) {
        patches = found->journal->patches(1);
    } else {
        auto opened = journal(store_.open(id));
        if (!opened) return std::unexpected(opened.error());
        held.emplace(std::move(opened->first));
        patches = held->patches(1);
    }
    if (!patches) return std::unexpected(Error::journal(patches.error()));
    while (auto next = patches->next()) {
        if (!*next) return std::unexpected(Error::journal(next->error()));
        f(**next);
    }
    return {};
}

Result<std::vector<std::pair<NodeId, std::uint64_t>>> Library::turn_seqs(std::string_view id) const {
    auto dom = load(id);
    if (!dom) return std::unexpected(dom.error());
    std::vector<std::pair<NodeId, std::uint64_t>> out;
    const auto* root = dom->root();
    if (root == nullptr) return out;
    const NodeId root_id = root->id;
    auto walked = each_patch(id, [&](const core::Patch& patch) {
        const auto* insert = std::get_if<core::op::Insert>(&patch.op);
        if (insert != nullptr && insert->kind == ElementKind::Turn && insert->parent == root_id &&
            dom->contains(patch.node))
            out.emplace_back(patch.node, patch.seq);
    });
    if (!walked) return std::unexpected(walked.error());
    return out;
}

Result<std::optional<std::uint64_t>> Library::inserted_at(std::string_view id, NodeId node) const {
    std::optional<std::uint64_t> found;
    auto walked = each_patch(id, [&](const core::Patch& patch) {
        if (!found && patch.node == node && std::holds_alternative<core::op::Insert>(patch.op)) found = patch.seq;
    });
    if (!walked) return std::unexpected(walked.error());
    return found;
}

Result<core::Dom> Library::before_turn(std::string_view id, NodeId node) const {
    auto dom = load(id);
    if (!dom) return std::unexpected(dom.error());
    std::optional<NodeId> turn;
    auto is_turn = [&](NodeId n) {
        const auto* found = dom->get(n);
        return found != nullptr && found->kind == ElementKind::Turn;
    };
    if (is_turn(node)) {
        turn = node;
    } else {
        for (const auto* ancestor : dom->ancestors(node)) {
            if (is_turn(ancestor->id)) {
                turn = ancestor->id;
                break;
            }
        }
    }
    if (!turn) return std::unexpected(Error::invalid_sequence());
    auto seq = inserted_at(id, *turn);
    if (!seq) return std::unexpected(seq.error());
    if (!*seq) return std::unexpected(Error::invalid_sequence());
    return load_at(id, **seq - 1);
}

Result<core::Dom> Library::through_turn(std::string_view id, NodeId turn) const {
    auto turns = turn_seqs(id);
    if (!turns) return std::unexpected(turns.error());
    auto at = std::ranges::find_if(*turns, [&](const auto& t) { return t.first == turn; });
    if (at == turns->end()) return std::unexpected(Error::invalid_sequence());
    if (++at != turns->end()) return load_at(id, at->second - 1);
    return load(id);
}

Result<Live> Library::import(const contracts::Adapter& adapter, const fs::path& native, std::string_view id,
                             std::string_view node) const {
    return import_with(id, node, std::nullopt, [&](const Live& live) -> Result<> {
        auto r = adapter.import(native, *live.writer);
        if (!r) return std::unexpected(Error::adapter(std::move(r.error())));
        return {};
    });
}

Result<> Library::import_staged(std::string_view id, std::string_view node, std::string_view project,
                                const StagedLoad& load) const {
    auto io = [](std::string_view what) {
        return std::unexpected(Error::port("Can't stage the imported session: " + std::string(what)));
    };
    std::string tmpl = (store_.root() / ".import-XXXXXX").string();
    if (::mkdtemp(tmpl.data()) == nullptr) return io(std::strerror(errno));
    const fs::path staging(tmpl);
    struct Cleanup {
        fs::path path;
        ~Cleanup() {
            std::error_code ec;
            fs::remove_all(path, ec);
        }
    } cleanup{staging};

    auto store = journal(store_.with_root(staging));
    if (!store) return std::unexpected(store.error());
    Library scratch(std::move(*store), std::make_shared<bus::Bus>(), local_, authorize_);
    scratch.with_sync(sync_);
    const fs::path target = store_.root() / std::string(id);
    const fs::path staged = staging / std::string(id);
    auto imported = scratch.import_with(id, node, project, [&](const Live& live) -> Result<> {
        return load(staged, target, *live.writer);
    });
    if (!imported) return std::unexpected(imported.error());
    imported = {};
    if (auto r = scratch.close(id); !r) return std::unexpected(r.error());
    std::error_code ec;
    fs::rename(staged, target, ec);
    if (ec) return io(ec.message());
    return {};
}

Result<Live> Library::import_with(std::string_view id, std::string_view node, std::optional<std::string_view> project,
                                  const std::function<Result<>(const Live&)>& load) const {
    auto created = create(id);
    if (!created) return std::unexpected(created.error());
    Live live = std::move(*created);
    auto finish = [&]() -> Result<> {
        if (auto r = load(live); !r) return r;
        struct Root {
            std::optional<NodeId> id;
            std::optional<std::string> native_id;
            std::optional<std::string> owner;
        };
        const auto found = live.writer->with_dom([](const core::Dom& dom) {
            Root out;
            if (const auto* root = dom.root()) {
                out.id = root->id;
                if (auto v = root->str_attr(attr::ID)) out.native_id = std::string(*v);
                if (auto v = root->str_attr(attr::NODE)) out.owner = std::string(*v);
            }
            return out;
        });
        if (!found.id) return std::unexpected(Error::no_session());
        const NodeId root = *found.id;
        auto& sink = static_cast<core::PatchSink&>(*live.writer);
        if (found.native_id && *found.native_id != id) {
            const bool missing = live.writer->with_dom([&](const core::Dom& dom) {
                const auto* r = dom.get(root);
                return r == nullptr || r->attr(attr::NATIVE_ID) == nullptr;
            });
            if (missing)
                if (auto r = detail::commit(sink, Draft::set(root, std::string(attr::NATIVE_ID), Value(*found.native_id)));
                    !r)
                    return std::unexpected(r.error());
            if (auto r = detail::commit(sink, Draft::set(root, std::string(attr::ID), Value(id))); !r)
                return std::unexpected(r.error());
        }
        if (found.owner != node)
            if (auto r = detail::commit(sink, Draft::set(root, std::string(attr::NODE), Value(node))); !r)
                return std::unexpected(r.error());
        if (project)
            if (auto r = detail::commit(sink, Draft::set(root, std::string(attr::PROJECT), Value(*project))); !r)
                return std::unexpected(r.error());
        return journal(live.journal->sync());
    };
    if (auto r = finish(); !r) {
        // Leave no half imported session behind.
        (void)remove(id);
        return std::unexpected(r.error());
    }
    return live;
}

Result<std::string> Library::import_child(const contracts::Adapter& adapter, std::string_view parent, NodeId job,
                                          const fs::path& native, std::string_view node) const {
    const auto id = child_id(native);
    const auto modified = modified_ms(native);
    auto listing = journal(store_.list());
    if (!listing) return std::unexpected(listing.error());
    const bool existing = std::ranges::any_of(*listing, [&](const bus::Listing& l) {
        const auto* info = std::get_if<bus::SessionInfo>(&l);
        return info != nullptr && info->id == id;
    });
    if (existing) {
        auto dom = load(id);
        if (!dom) return std::unexpected(dom.error());
        const auto* root = dom->root();
        if (root != nullptr && root->int_attr(SOURCE_MODIFIED) == modified) return id;
        if (auto r = remove(id); !r) return std::unexpected(r.error());
    }
    auto inserted = inserted_at(parent, job);
    if (!inserted) return std::unexpected(inserted.error());
    const auto forked_at = inserted->value_or(0);
    auto live = import(adapter, native, id, node);
    if (!live) return std::unexpected(live.error());
    const auto root = live->writer->with_dom([](const core::Dom& dom) -> std::optional<NodeId> {
        if (const auto* r = dom.root()) return r->id;
        return std::nullopt;
    });
    if (!root) return std::unexpected(Error::no_session());
    auto& sink = static_cast<core::PatchSink&>(*live->writer);
    if (auto r = detail::commit(sink, Draft::set(*root, std::string(attr::PARENT_SESSION), Value(parent))); !r)
        return std::unexpected(r.error());
    if (auto r = detail::commit(sink, Draft::set(*root, std::string(attr::FORKED_AT), detail::seq_value(forked_at))); !r)
        return std::unexpected(r.error());
    if (auto r = detail::commit(sink, Draft::set(*root, std::string(SOURCE_MODIFIED), Value(modified))); !r)
        return std::unexpected(r.error());
    if (auto r = journal(live->journal->sync()); !r) return std::unexpected(r.error());
    return id;
}

Result<> Library::export_(const contracts::Adapter& adapter, std::string_view id, const fs::path& out) const {
    auto dom = load(id);
    if (!dom) return std::unexpected(dom.error());
    if (auto r = adapter.export_(*dom, out); !r) return std::unexpected(Error::adapter(std::move(r.error())));
    return {};
}

Result<> Library::export_xml(std::string_view id, const fs::path& out) const {
    auto dom = load(id);
    if (!dom) return std::unexpected(dom.error());
    const auto text = core::xml::to_string(*dom);
    std::ofstream file(out, std::ios::binary | std::ios::trunc);
    if (file) file.write(text.data(), static_cast<std::streamsize>(text.size()));
    if (!file) return std::unexpected(Error::port(out.string() + ": " + std::strerror(errno)));
    return {};
}

Result<std::pair<core::Dom, std::shared_ptr<core::PatchSink>>> Library::fork_now(std::string_view source_id,
                                                                                std::uint64_t seq,
                                                                                std::string_view new_id) const {
    bus::JournalResult<bus::Journal> forked = std::unexpected(bus::JournalError(bus::journal_error::Closed{}));
    if (auto found = live(source_id)) {
        forked = found->journal->fork(seq, new_id);
    } else {
        auto opened = journal(store_.open(source_id));
        if (!opened) return std::unexpected(opened.error());
        forked = opened->first.fork(seq, new_id);
    }
    if (!forked) return std::unexpected(Error::journal(forked.error()));
    auto dom = journal(forked->load());
    if (!dom) return std::unexpected(dom.error());
    auto attached = attach(new_id, std::move(*forked), *dom);
    if (!attached) return std::unexpected(attached.error());
    return std::pair{std::move(*dom), std::shared_ptr<core::PatchSink>(std::move(attached->writer))};
}

Awaitable<Result<std::pair<core::Dom, std::shared_ptr<core::PatchSink>>>> Library::fork(std::string source_id,
                                                                                       std::uint64_t seq,
                                                                                       std::string new_id) const {
    co_return fork_now(source_id, seq, new_id);
}

Awaitable<Result<std::shared_ptr<core::PatchSink>>> Library::fresh(std::string new_id) const {
    auto created = create(new_id);
    if (!created) co_return std::unexpected(created.error());
    co_return std::shared_ptr<core::PatchSink>(std::move(created->writer));
}

} // namespace loa::session
