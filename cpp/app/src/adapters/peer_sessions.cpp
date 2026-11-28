#include "loa/app/adapters/peer_sessions.hpp"

#include <algorithm>
#include <variant>

#include "loa/bus/writer.hpp"
#include "loa/session/library.hpp"

namespace loa::app::adapters {

LibrarySessions::LibrarySessions(std::shared_ptr<session::Library> library) : library_(std::move(library)) {}

std::optional<p2p::LocalSession> LibrarySessions::closed(const std::string& id, std::uint64_t seq) const {
    std::lock_guard lock(mutex_);
    if (auto it = cache_.find(id); it != cache_.end() && it->second.last_seq == seq) return it->second;
    auto dom = library_->load(id);
    if (!dom) return std::nullopt;
    auto session = p2p::LocalSession::from_dom(*dom);
    if (!session) return std::nullopt;
    cache_.insert_or_assign(id, *session);
    return session;
}

std::vector<p2p::LocalSession> LibrarySessions::sessions() const {
    auto listings = library_->store().list();
    if (!listings) return {};
    std::vector<p2p::LocalSession> out;
    for (const auto& listing : *listings) {
        const auto* info = std::get_if<bus::SessionInfo>(&listing);
        if (!info) continue;
        std::optional<p2p::LocalSession> session;
        if (auto live = library_->live(info->id)) {
            if (live->writer->is_writer())
                session = live->writer->with_dom([](const core::Dom& dom) { return p2p::LocalSession::from_dom(dom); });
        } else {
            session = closed(info->id, info->last_seq);
        }
        if (session) out.push_back(std::move(*session));
    }
    std::ranges::sort(out, [](const p2p::LocalSession& a, const p2p::LocalSession& b) {
        if (a.facts.created_ms != b.facts.created_ms) return a.facts.created_ms > b.facts.created_ms;
        return a.facts.id < b.facts.id;
    });
    return out;
}

std::shared_ptr<bus::SessionWriter> LibrarySessions::writer(std::string_view id) const {
    auto live = library_->open(id);
    if (!live || !live->writer->is_writer()) return nullptr;
    return live->writer;
}

} // namespace loa::app::adapters
