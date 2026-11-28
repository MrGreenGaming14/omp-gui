#include "loa/projects/sessions.hpp"

#include <algorithm>

#include "loa/bus/writer.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/sink.hpp"
#include "loa/runtime/log.hpp"

namespace loa::projects {

namespace attr = core::attr;
using DomPtr = std::shared_ptr<const core::Dom>;

namespace {

Error port(const bus::JournalError& e) { return Error::port(bus::to_string(e)); }

bool missing(const bus::JournalError& e) {
    return std::holds_alternative<bus::journal_error::NotFound>(e) ||
           std::holds_alternative<bus::journal_error::InvalidSessionId>(e);
}

} // namespace

struct LiveSessions::Inner {
    Inner(std::shared_ptr<bus::Bus> b, bus::Store s) : bus(std::move(b)), store(std::move(s)) {}

    std::shared_ptr<bus::Bus> bus;
    bus::Store store;
    // Stored sessions by id with the journal sequence they were loaded at.
    mutable std::mutex mutex;
    mutable std::map<std::string, std::pair<std::uint64_t, DomPtr>, std::less<>> cache;

    DomPtr live(std::string_view session) const {
        auto writer = bus->get(session);
        if (!writer) return nullptr;
        return writer->with_dom([](const core::Dom& dom) { return std::make_shared<const core::Dom>(dom); });
    }

    DomPtr cached(std::string_view session, std::uint64_t seq) const {
        std::scoped_lock lock(mutex);
        auto it = cache.find(session);
        return it != cache.end() && it->second.first == seq ? it->second.second : nullptr;
    }

    void remember(std::string_view session, std::uint64_t seq, DomPtr dom) const {
        std::scoped_lock lock(mutex);
        cache.insert_or_assign(std::string(session), std::pair{seq, std::move(dom)});
    }

    Result<DomPtr> load(std::string_view session) const {
        if (auto dom = live(session)) return dom;
        auto info = store.inspect(session);
        if (!info) {
            if (missing(info.error())) return DomPtr{};
            if (auto dom = live(session)) return dom;
            return std::unexpected(port(info.error()));
        }
        if (auto dom = cached(session, info->last_seq)) return dom;
        auto read = store.read(session);
        if (!read) {
            if (auto dom = live(session)) return dom;
            return std::unexpected(port(read.error()));
        }
        auto dom = std::make_shared<const core::Dom>(std::move(*read));
        remember(session, dom->seq(), dom);
        return dom;
    }

    Result<std::vector<std::pair<SessionInfo, DomPtr>>> all() const {
        std::vector<std::string> ids;
        for (const auto& id : bus->ids()) ids.push_back(id.str());
        auto listings = store.list();
        if (!listings) return std::unexpected(port(listings.error()));
        for (const auto& listing : *listings) {
            if (const auto* info = std::get_if<bus::SessionInfo>(&listing)) {
                if (std::ranges::find(ids, info->id) == ids.end()) ids.push_back(info->id);
            } else {
                const auto& bad = std::get<bus::listing::Unreadable>(listing);
                runtime::log::warn("loa_projects", "skipping an unreadable session journal: session=" + bad.id +
                                                       " error=" + bus::to_string(bad.error));
            }
        }
        std::vector<std::pair<SessionInfo, DomPtr>> out;
        for (const auto& id : ids) {
            auto dom = load(id);
            if (!dom) {
                runtime::log::warn("loa_projects",
                                   "skipping a session that can't be read: session=" + id + " error=" + dom.error().to_string());
                continue;
            }
            if (!*dom) continue;
            if (auto info = SessionInfo::from_dom(**dom)) out.emplace_back(std::move(*info), std::move(*dom));
        }
        return out;
    }

    Result<core::Patch> commit(std::string_view session, core::Draft draft) const {
        if (auto writer = bus->get(session)) {
            auto patch = writer->commit_at(std::move(draft), core::now_ms());
            if (!patch) return std::unexpected(Error::commit(core::to_string(patch.error())));
            return std::move(*patch);
        }
        auto opened = store.open(session);
        if (!opened) {
            if (missing(opened.error())) return std::unexpected(Error::session_not_found(std::string(session)));
            return std::unexpected(port(opened.error()));
        }
        auto& journal = opened->first;
        auto loaded = journal.load();
        if (!loaded) return std::unexpected(port(loaded.error()));
        auto dom = std::move(*loaded);
        auto patch = dom.commit(std::move(draft), core::now_ms());
        if (!patch) return std::unexpected(Error::commit(core::to_string(core::CommitError{core::commit_error::Apply{patch.error()}})));
        if (auto r = journal.append_applied(*patch, dom); !r) return std::unexpected(port(r.error()));
        if (auto r = journal.sync(); !r) return std::unexpected(port(r.error()));
        const auto seq = journal.last_seq();
        { auto release = std::move(opened->first); }  // closes the journal and its lock before caching
        remember(session, seq, std::make_shared<const core::Dom>(std::move(dom)));
        return std::move(*patch);
    }
};

LiveSessions::LiveSessions(std::shared_ptr<bus::Bus> bus, bus::Store store)
    : inner_(std::make_shared<Inner>(std::move(bus), std::move(store))) {}

Result<std::vector<std::pair<SessionInfo, DomPtr>>> LiveSessions::all() const { return inner_->all(); }

Result<std::map<std::string, std::string>> LiveSessions::native_sessions(std::string_view harness) const {
    const auto native_id = [&](const core::Dom& dom) -> std::optional<std::string> {
        const auto* root = dom.root();
        if (!root || root->str_attr(attr::HARNESS) != harness) return std::nullopt;
        auto id = root->str_attr(attr::NATIVE_ID);
        return id ? std::optional<std::string>(std::string(*id)) : std::nullopt;
    };
    auto listings = inner_->store.list();
    if (!listings) return std::unexpected(port(listings.error()));
    std::map<std::string, std::string> imported;
    for (const auto& listing : *listings) {
        const auto* info = std::get_if<bus::SessionInfo>(&listing);
        if (!info) continue;
        std::optional<std::string> native;
        if (auto writer = inner_->bus->get(info->id)) {
            native = writer->with_dom(native_id);
        } else if (auto dom = inner_->cached(info->id, info->last_seq)) {
            native = native_id(*dom);
        } else if (auto read = inner_->store.read(info->id)) {
            native = native_id(*read);
        }
        if (native) imported.try_emplace(std::move(*native), info->id);
    }
    return imported;
}

Result<std::vector<SessionInfo>> LiveSessions::list(std::string_view project) const {
    auto all = inner_->all();
    if (!all) return std::unexpected(all.error());
    std::vector<SessionInfo> out;
    for (auto& [info, _] : *all)
        if (info.project == project) out.push_back(std::move(info));
    return out;
}

Result<DomPtr> LiveSessions::load(std::string_view session) const { return inner_->load(session); }

Result<core::Patch> LiveSessions::commit(std::string_view session, core::Draft draft) const {
    return inner_->commit(session, std::move(draft));
}

} // namespace loa::projects
