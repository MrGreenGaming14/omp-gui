#pragma once

// The headless app runtime: the session library, the live sessions, the
// harnesses they run on and the active project. No UI (port of `loa_runtime`).

#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/app_runtime/harnesses.hpp"
#include "loa/app_runtime/lifecycle.hpp"
#include "loa/app_runtime/session.hpp"
#include "loa/session/library.hpp"

namespace loa::app_runtime {

// What a `Runtime` starts from.
struct RuntimeParts {
    // Session journals, opened as writers on one bus.
    std::shared_ptr<session::Library> library;
    Harnesses harnesses;
    // Public key of this node, written to `<session node>`.
    std::string node;
    // Directory new sessions run in.
    std::filesystem::path cwd;
    // The active project, which new sessions join; `nullopt` for no project.
    std::optional<std::string> project;
    // Approval mode new sessions run in; `nullopt` for the harness default.
    std::optional<std::string> access;
};

// The sessions, their library and harnesses, and the active project.
class Runtime {
  public:
    explicit Runtime(RuntimeParts parts);

    [[nodiscard]] const std::shared_ptr<session::Library>& library() const noexcept { return library_; }
    [[nodiscard]] const Harnesses& harnesses() const noexcept { return harnesses_; }
    [[nodiscard]] Harnesses& harnesses_mut() noexcept { return harnesses_; }
    [[nodiscard]] const Sessions& sessions() const noexcept { return sessions_; }
    [[nodiscard]] Sessions& sessions_mut() noexcept { return sessions_; }

    // Public key of this node.
    [[nodiscard]] std::string_view node() const noexcept { return node_; }
    // Directory new sessions run in.
    [[nodiscard]] const std::filesystem::path& cwd() const noexcept { return cwd_; }
    // The active project, which new sessions join. Projects group sessions; they are not folders.
    [[nodiscard]] const std::optional<std::string>& project() const noexcept { return project_; }
    // Makes `project` the active one, and `cwd`, when given, the folder new sessions run in.
    void set_active(std::string project, std::optional<std::filesystem::path> cwd);
    // Makes `cwd` the folder new sessions run in; the active project stays.
    void set_cwd(std::filesystem::path cwd) { cwd_ = std::move(cwd); }
    // No project is active: new sessions join none.
    void clear_project() { project_.reset(); }

    // Approval mode new sessions run in; `nullopt` for the harness default.
    [[nodiscard]] const std::optional<std::string>& access() const noexcept { return access_; }
    void set_access(std::optional<std::string> access) { access_ = std::move(access); }
    // Whether the active session restarts on the approval mode once its turn ends.
    [[nodiscard]] bool restart_for_access() const noexcept { return restart_for_access_; }
    void set_restart_for_access(bool restart) { restart_for_access_ = restart; }

    // Creates a new session's journal and writer.
    [[nodiscard]] std::expected<std::pair<SessionId, std::shared_ptr<bus::SessionWriter>>, std::string> create_session();
    // Opens a session from its journal unless it is open already.
    [[nodiscard]] std::expected<void, std::string> load_session(std::string_view id);
    // Closes a session's writer and releases its journal, deleting the journal too when `delete_`.
    std::pair<std::optional<Session>, std::optional<std::string>> close_session(std::string_view id, bool delete_);
    // Deletes the journals of the empty sessions `recents` found, unless they were opened
    // since; returns a note per one that couldn't be deleted.
    [[nodiscard]] std::vector<std::string> delete_empty(std::vector<std::string> empty) const;

    // ---- lifecycle -----------------------------------------------------------

    // Applies `command` and returns the work and events it leads to.
    Transition dispatch(Command command);
    // Begins a feed frame without applying its patches yet.
    [[nodiscard]] FeedBatch begin_feed(SessionId id, Feed feed) const;
    // Applies the next event, or finishes the frame's once-only turn reaction. React to the
    // returned transition before calling again: its DOM is valid only until the next patch.
    // `nullopt` means the frame is fully drained.
    std::optional<Transition> advance_feed(FeedBatch& batch);

  private:
    [[nodiscard]] contracts::SessionConfig base_config(std::string_view session_id, std::string_view harness,
                                                       const core::Dom* dom) const;
    void prepare(SessionId session, std::string harness, bool resume, contracts::SessionConfig config, Launch launch,
                 Transition& out);
    void start(std::optional<contracts::Prompt> prompt, Transition& out);
    void resume(contracts::Prompt prompt, Transition& out);
    void continue_in_new_session(contracts::Prompt prompt, Transition& out) const;
    void send(contracts::Prompt prompt, Transition& out);
    void spawn(Spawn spawn, Transition& out);
    void interrupt(Transition& out);
    void open(SessionId id, Transition& out);
    void close(const SessionId& id, bool delete_, Transition& out);
    void changed(const SessionId& id, Change change, Transition& out) const;
    void complete(Completion completion, Transition& out);
    void started(SessionId id, HarnessPtr harness, std::optional<contracts::Prompt> prompt, Transition& out);
    void change_access(std::string id, Transition& out);
    void restart(Transition& out);

    std::shared_ptr<session::Library> library_;
    Harnesses harnesses_;
    Sessions sessions_;
    std::string node_;
    std::filesystem::path cwd_;
    std::optional<std::string> project_;
    std::optional<std::string> access_;
    // The approval mode changed while a turn ran: the active session restarts on it once the
    // turn ends.
    bool restart_for_access_ = false;
    // Starts, resumes and spawns in flight.
    Operations operations_;
};

} // namespace loa::app_runtime
