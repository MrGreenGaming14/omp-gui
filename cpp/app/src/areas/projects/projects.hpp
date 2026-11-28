#pragma once

// Projects (P1, P2) (port of `loa-app/src/areas/projects/{mod,history}.rs`): projects are groups
// of sessions, not folders. A session's project is its `<session project>`; the sidebar lists
// every stored project and every project a session names. A project may have a folder, where
// its new chats start and whose `.omgui` folder links to the directories its sessions run in.
// This area creates projects (the New project screen), moves sessions between them, changes a
// project's folder, deletes projects (keeping their sessions), selects a project (it becomes the
// active project, with its folder when it has one, and its screen lists its chats), and runs the
// project screen (shared instructions, the ideas notes, pinned files, sessions with their
// digests and exclude toggle), digest rebuilds at turn end and compaction, the project context
// new chats receive, the `project_search` / `project_read` host tools, and the project's OMP
// terminal history (listing, importing and deleting omp's own conversations in its folder).

#include <cstdint>
#include <expected>
#include <filesystem>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <set>
#include <span>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/app/area.hpp"
#include "loa/app/routes.hpp"
#include "loa/harness/omp/native.hpp"
#include "loa/harness/omp/session.hpp"
#include "loa/projects/rebuild.hpp"
#include "loa/projects/service.hpp"
#include "loa/projects/sessions.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/settings/projects.hpp"
#include "loa/ui/settings/projects/create.hpp"
#include "loa/ui/settings/projects/state.hpp"

namespace loa::app::areas::projects {

namespace screen = loa::ui::settings::projects;
namespace create = loa::ui::settings::projects::create;
using loa::harness::omp::native::ProjectSession;

// Palette command id prefixes that open a project and toggle its sidebar pin, followed by its name.
inline constexpr std::string_view OPEN_COMMAND = "open:";
inline constexpr std::string_view PIN_COMMAND = "pin:";

// What a folder dialog picks for.
namespace pick {
// The New project form's folder.
struct Form {};
// The folder of this existing project.
struct Project {
    std::string project;
};
} // namespace pick
using Pick = std::variant<pick::Form, pick::Project>;

// How saving the project screen went.
namespace saving {
// Everything sent was stored; the ideas notes, when sent, with their file as written.
struct Stored {
    std::optional<loa::settings::Ideas> ideas;
};
// The settings were stored, but the ideas notes changed outside since they were loaded and were left alone.
struct IdeasChanged {};
struct Failed {
    std::string error;
};
} // namespace saving
using Saving = std::variant<saving::Stored, saving::IdeasChanged, saving::Failed>;

namespace msg {
// Every project, most recently active first.
struct Listed {
    std::expected<std::vector<loa::projects::ProjectSummary>, std::string> result;
};
// The project screen's data for this project, answering overview request `request`.
struct Overview {
    std::uint64_t request = 0;
    std::string project;
    std::expected<loa::settings::ProjectOverview, std::string> result;
};
struct Screen {
    screen::Message message;
};
// A save of the project screen with these settings.
struct Saved {
    std::string project;
    loa::settings::ProjectSettings settings;
    Saving saving;
};
// A session's exclude toggle was recorded, in this project.
struct Excluded {
    std::string project;
    std::expected<bool, std::string> result;
};
// The context new chats in this project receive; `nullopt` when it has nothing to share yet.
struct Context {
    std::string project;
    std::expected<std::optional<std::string>, std::string> result;
};
// The rebuild driver finished with a session's digest.
struct Rebuild {
    loa::projects::RebuildOutcome outcome;
};
// A project was deleted, with how many sessions it had.
struct Deleted {
    std::string project;
    std::expected<std::size_t, std::string> result;
};
struct Form {
    create::Message message;
};
// A project was created, with the folder it stored.
struct Created {
    std::string name;
    std::expected<std::optional<std::filesystem::path>, std::string> result;
};
// A folder dialog closed, with the folder picked if any.
struct FolderChosen {
    Pick pick;
    std::expected<std::optional<std::filesystem::path>, std::string> result;
};
// A project's folder was stored, possibly isolated inside the temp folder.
struct FolderSet {
    std::string project;
    std::expected<std::filesystem::path, std::string> result;
};
struct Moved {
    std::expected<void, std::string> result;
};
struct HistoryLoaded {
    std::uint64_t scan = 0;
    std::expected<std::vector<ProjectSession>, std::string> result;
};
struct HistoryOpened {
    std::uint64_t scan = 0;
    std::expected<std::string, std::string> result;
};
// A history row's conversation was deleted from omp's storage.
struct HistoryDeleted {
    std::expected<void, std::string> result;
};
} // namespace msg

// `to::projects::Open`: the user picked this project in the sidebar or the palette.
// `to::projects::Activate`: make it the active one without showing its screen.
// `to::projects::NewChatIn`: a new session in this project, in its folder.
// `to::projects::AskDelete`: show the project's screen asking to confirm its deletion.
// `to::projects::New`: open the New project screen.
// `to::projects::Move`: move a session to a project, or out of every project.
using Message = std::variant<to::projects::Open, msg::Listed, msg::Overview, msg::Screen, msg::Saved, msg::Excluded,
                             msg::Context, msg::Rebuild, to::projects::Activate, to::projects::NewChatIn,
                             to::projects::AskDelete, msg::Deleted, to::projects::New, msg::Form, msg::Created,
                             msg::FolderChosen, msg::FolderSet, to::projects::Move, msg::Moved, msg::HistoryLoaded,
                             msg::HistoryOpened, msg::HistoryDeleted>;

// The OMP terminal history of the shown project's folder.
struct History {
    std::uint64_t scan = 0;
    std::optional<std::filesystem::path> folder;
    std::vector<ProjectSession> rows;
    // The project whose history row is being opened or deleted; one row at a time.
    std::optional<std::string> busy;
    std::set<std::string, std::less<>> changing;

    [[nodiscard]] bool matches_folder(const std::optional<std::filesystem::path>& f) const { return folder == f; }
    bool begin_change(std::string_view project);
    void end_change(std::string_view project);
};

class State final : public TypedArea<State>, public ProjectsPort {
  public:
    static constexpr std::string_view ID = area_id::PROJECTS;
    using Message = projects::Message;

    // The projects of this node, digests built by the app's summarizer.
    [[nodiscard]] static std::pair<std::unique_ptr<State>, Task> make(Ctx& ctx);
    // The projects of this node with digests from `summarizer`.
    [[nodiscard]] static std::pair<std::unique_ptr<State>, Task> with(
        Ctx& ctx, std::shared_ptr<const loa::projects::Summarizers> summarizer, loa::projects::ServiceConfig config);
    ~State() override;

    static std::span<const AreaCommand<State>> COMMANDS();

    Task update_typed(Message message, Ctx& ctx);

    // The project whose page this area shows, if any.
    [[nodiscard]] std::optional<std::string> shown() const override;
    // The projects the sidebar lists.
    [[nodiscard]] const std::vector<loa::projects::ProjectSummary>& projects() const noexcept { return projects_; }
    // The cached context new chats in `project` receive.
    [[nodiscard]] std::optional<std::string_view> context(std::string_view project) const;
    // The project screen, once a project was opened.
    [[nodiscard]] const screen::State* screen() const noexcept { return screen_ ? &*screen_ : nullptr; }
    // The New project form while it is open.
    [[nodiscard]] const create::State* form() const noexcept { return creating_ ? &*creating_ : nullptr; }
    // The id of the latest overview request.
    [[nodiscard]] std::uint64_t overview_request() const noexcept { return overview_request_; }

    [[nodiscard]] QWidget* view_screen(const Ctx& ctx, const Emit& send) const override;
    void contribute(contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const override;
    [[nodiscard]] std::optional<HostToolSet> host_tools(const contracts::SessionConfig& config, const Target& target,
                                                        const Ctx& ctx) const override;
    Task on_session_event(const SessionEvent& event, Ctx& ctx) override;
    [[nodiscard]] Subscriptions subscription() const override;

    // Runs `projects.<arg>` (palette command).
    Task command(std::string_view arg, Ctx& ctx);
    // The palette rows: each project's Open, then its Pin.
    [[nodiscard]] std::vector<std::pair<std::string, std::string>> rows(const Ctx& ctx) const;

  private:
    // The project and directory the next new session starts in, set by the project screen's
    // "New chat" and a project row's `+`.
    struct NewChat {
        std::string project;
        std::optional<std::filesystem::path> cwd;
    };

    State(std::unique_ptr<loa::runtime::Executor> executor, loa::projects::ProjectService service,
          loa::projects::LiveSessions sessions, std::shared_ptr<loa::projects::Outcomes> outcomes,
          std::pair<std::vector<contracts::HostToolSpec>, std::shared_ptr<const contracts::HostTools>> tools);

    [[nodiscard]] Task list() const;
    [[nodiscard]] Task load_overview(std::string project);
    [[nodiscard]] Task refresh_context(std::string project) const;
    [[nodiscard]] Task refresh_contexts() const;
    [[nodiscard]] Task reload_screen();
    [[nodiscard]] std::optional<std::filesystem::path> folder(std::string_view project) const;
    void sync_chip(Ctx& ctx) const;
    Task open(std::string project, Ctx& ctx);
    Task open_in(std::string project, std::optional<std::filesystem::path> folder, Ctx& ctx);
    Task show(std::string project, Ctx& ctx);
    [[nodiscard]] static Task choose_folder(const Ctx& ctx, std::filesystem::path start, Pick pick);
    Task screen_event(screen::Intent event, Ctx& ctx);
    Task form_event(screen::CreateIntent event, Ctx& ctx);
    [[nodiscard]] std::vector<std::string> names() const;

    // history.rs
    void clear_history();
    Task scan_history(std::optional<std::filesystem::path> folder, const Ctx& ctx);
    void history_loaded(std::uint64_t scan, std::expected<std::vector<ProjectSession>, std::string> result);
    std::optional<std::tuple<std::string, std::filesystem::path, ProjectSession>> claim_row(std::string_view native_id,
                                                                                         Ctx& ctx);
    void release_row();
    Task refresh_history(const Ctx& ctx);
    Task open_history(std::string_view native_id, Ctx& ctx);
    Task history_opened(std::uint64_t scan, std::expected<std::string, std::string> result, Ctx& ctx);
    Task delete_history(std::string_view native_id, Ctx& ctx);
    Task history_deleted(std::expected<void, std::string> result, Ctx& ctx);

    // Runs the rebuild driver; declared first so it stops after everything that feeds it.
    std::unique_ptr<loa::runtime::Executor> executor_;
    loa::projects::ProjectService service_;
    loa::projects::LiveSessions sessions_;
    History history_;
    // `project_search` and `project_read`, served to every session.
    std::pair<std::vector<contracts::HostToolSpec>, std::shared_ptr<const contracts::HostTools>> tools_;
    // The rebuild driver's outcomes, which the subscription feeds.
    std::shared_ptr<loa::projects::Outcomes> outcomes_;
    std::vector<loa::projects::ProjectSummary> projects_;
    // The project screen, drawn while the app shows this area's screen.
    std::optional<screen::State> screen_;
    std::uint64_t overview_request_ = 0;
    // The New project form, drawn instead of the project screen while open.
    std::optional<create::State> creating_;
    // Context for new chats, by project.
    std::map<std::string, std::optional<std::string>, std::less<>> contexts_;
    // The shown project's ideas notes file as seen by the screen's notes, which saving them
    // checks it against. Kept while the notes have an unsaved edit; `nullopt` until loaded, and
    // to take the next overview's notes over an edit.
    std::optional<loa::runtime::fs::Snapshot> ideas_file_;
    // Taken by the next new session's `contribute`.
    mutable std::mutex new_chat_mutex_;
    mutable std::optional<NewChat> new_chat_;
};

} // namespace loa::app::areas::projects
