#pragma once

// The app's sessions as a `SessionSource` and `SessionWriter`: live ones through their bus writer, the rest
// from their journal, opened only while read or written. Stored DOMs are cached by journal sequence, so
// listing a project doesn't replay unchanged journals.

#include <cstdint>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/bus/bus.hpp"
#include "loa/bus/store.hpp"
#include "loa/projects/rebuild.hpp"
#include "loa/projects/source.hpp"

namespace loa::projects {

// Every session of this node, live or stored. Cheap to copy; copies share the cache.
class LiveSessions final : public SessionSource, public SessionWriter {
  public:
    LiveSessions(std::shared_ptr<bus::Bus> bus, bus::Store store);

    // Every session that belongs to a project, live ones first, with its DOM. Unreadable ones are logged.
    [[nodiscard]] Result<std::vector<std::pair<SessionInfo, std::shared_ptr<const core::Dom>>>> all() const;
    // Native ids already in Loa for `harness`, mapped to their session ids.
    [[nodiscard]] Result<std::map<std::string, std::string>> native_sessions(std::string_view harness) const;

    [[nodiscard]] Result<std::vector<SessionInfo>> list(std::string_view project) const override;
    [[nodiscard]] Result<std::shared_ptr<const core::Dom>> load(std::string_view session) const override;
    [[nodiscard]] Result<core::Patch> commit(std::string_view session, core::Draft draft) const override;

  private:
    struct Inner;
    std::shared_ptr<Inner> inner_;
};

} // namespace loa::projects
