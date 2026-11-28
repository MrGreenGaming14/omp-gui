#pragma once

// Ports for reading a project's sessions and checking which may be shown. Every path that shows sessions
// starts at `visible_sessions`, so a denied session never leaks.

#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/core/dom.hpp"
#include "loa/projects/error.hpp"

namespace loa::projects {

// What a project listing knows about a session.
struct SessionInfo {
    std::string id;
    std::string project;
    // Owner node key.
    std::string node;
    // Unix milliseconds.
    std::int64_t created = 0;
    // The `<session title>`, else the first line of the first user prompt.
    std::optional<std::string> title;

    // None without a `<session id>` or a project.
    [[nodiscard]] static std::optional<SessionInfo> from_dom(const core::Dom& dom);
    friend bool operator==(const SessionInfo&, const SessionInfo&) = default;
};

// The session's `<session project>`, trimmed. A session without one belongs to no project.
[[nodiscard]] std::optional<std::string> session_project(const core::Dom& dom);

// Loads the sessions of a project. Implementations block (journal reads), like the Rust
// `spawn_blocking` bodies; callers on the executor run them where blocking is fine.
class SessionSource {
  public:
    virtual ~SessionSource() = default;
    // Every session of `project`, in any order.
    [[nodiscard]] virtual Result<std::vector<SessionInfo>> list(std::string_view project) const = 0;
    [[nodiscard]] virtual Result<std::shared_ptr<const core::Dom>> load(std::string_view session) const = 0;
};

// Which sessions this node's user may see through projects.
class Access {
  public:
    virtual ~Access() = default;
    [[nodiscard]] virtual bool can_open(const SessionInfo& session) const = 0;
};

// Every local session is visible.
class LocalOnly final : public Access {
  public:
    [[nodiscard]] bool can_open(const SessionInfo&) const override { return true; }
};

// The visible sessions of `project`, newest first (ties by id).
[[nodiscard]] Result<std::vector<SessionInfo>> visible_sessions(const SessionSource& source, const Access& access,
                                                                std::string_view project);

// The visible session `id` of `project`; SessionNotFound when missing, elsewhere or denied.
[[nodiscard]] Result<std::shared_ptr<const core::Dom>> open_visible(const SessionSource& source, const Access& access,
                                                                    std::string_view project, std::string_view id);

} // namespace loa::projects
