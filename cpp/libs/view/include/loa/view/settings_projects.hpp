#pragma once

// The project screen: shared instructions, pinned files, the digest model, the
// ideas notes and the project's sessions with their digests and context toggle.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <optional>
#include <string>
#include <variant>
#include <vector>

namespace loa::view::settings::projects {

// A file every chat of the project should know about, with why it is pinned.
struct PinnedFile {
    std::string path;
    std::optional<std::string> note;

    friend bool operator==(const PinnedFile&, const PinnedFile&) = default;
};

// Settings shared by every session of a project; `digest_model` `std::nullopt` uses the summarizer's default.
// `folder`, where the project's new chats start, isn't edited with the rest and is kept as stored.
struct ProjectSettings {
    std::string instructions;
    std::vector<PinnedFile> pinned;
    std::optional<std::string> digest_model;
    std::optional<std::string> folder;
    // Whether the ideas notes go into the context of the project's new chats; off by default.
    bool ideas_in_context = false;

    friend bool operator==(const ProjectSettings&, const ProjectSettings&) = default;
};

// Whether a session's digest is written and current.
enum class DigestState : std::uint8_t { Missing, Fresh, Stale };

// What the screen shows of a session's digest.
struct Digest {
    std::string summary;
    std::vector<std::string> decisions;

    friend bool operator==(const Digest&, const Digest&) = default;
};

// One session on the project screen.
struct SessionRow {
    std::string id;
    std::optional<std::string> title;
    std::optional<std::string> harness;
    // Unix milliseconds of the last activity.
    std::int64_t last_active = 0;
    std::size_t turns = 0;
    bool excluded = false;
    DigestState digest_state = DigestState::Missing;
    std::optional<Digest> digest;
    std::optional<std::string> digest_model;

    friend bool operator==(const SessionRow&, const SessionRow&) = default;
};

// Everything the project screen shows; sessions newest activity first.
struct ProjectOverview {
    std::optional<std::string> cwd;
    ProjectSettings settings;
    // The user's own ideas notes for the project, empty when there are none, or why they can't be read.
    std::expected<std::string, std::string> ideas;
    std::vector<SessionRow> sessions;

    friend bool operator==(const ProjectOverview&, const ProjectOverview&) = default;
};

struct OmpHistoryRow {
    std::string id;
    std::string title;
    // Unix milliseconds of the last write.
    std::int64_t modified = 0;

    friend bool operator==(const OmpHistoryRow&, const OmpHistoryRow&) = default;
};

namespace omp_history {
struct Loading {
    friend bool operator==(const Loading&, const Loading&) = default;
};
struct Ready {
    std::vector<OmpHistoryRow> rows;
    friend bool operator==(const Ready&, const Ready&) = default;
};
struct Failed {
    std::string message;
    friend bool operator==(const Failed&, const Failed&) = default;
};
} // namespace omp_history

using OmpHistory = std::variant<omp_history::Loading, omp_history::Ready, omp_history::Failed>;

// What the app does for the screen.
namespace intent {
// Store these settings, and the ideas notes when they were edited, and report the result back to the screen.
// `std::nullopt` leaves the stored notes alone; edited notes are refused when the file changed outside since it
// was loaded.
struct SaveSettings {
    std::string project;
    ProjectSettings settings;
    std::optional<std::string> ideas;
    friend bool operator==(const SaveSettings&, const SaveSettings&) = default;
};
// Load the ideas notes again after they changed outside, dropping their unsaved edit.
struct ReloadIdeas {
    friend bool operator==(const ReloadIdeas&, const ReloadIdeas&) = default;
};
// Record a session's context toggle; answer with a fresh overview.
struct SetExcluded {
    std::string session;
    bool excluded = false;
    friend bool operator==(const SetExcluded&, const SetExcluded&) = default;
};
struct OpenSession {
    std::string session;
    friend bool operator==(const OpenSession&, const OpenSession&) = default;
};
// Import and open a listed OMP terminal conversation by its native id.
struct OpenOmpHistory {
    std::string id;
    friend bool operator==(const OpenOmpHistory&, const OpenOmpHistory&) = default;
};
// Delete a listed OMP terminal conversation from omp's storage by its native id; Loa's imported copies stay.
struct DeleteOmpHistory {
    std::string id;
    friend bool operator==(const DeleteOmpHistory&, const DeleteOmpHistory&) = default;
};
// Start a chat in this project, in its working directory when known.
struct NewChat {
    std::string project;
    std::optional<std::string> cwd;
    friend bool operator==(const NewChat&, const NewChat&) = default;
};
// Show a folder dialog for the project's folder, starting at the current one; the app stores the choice and
// sends a fresh overview.
struct ChangeFolder {
    std::string project;
    std::optional<std::string> current;
    friend bool operator==(const ChangeFolder&, const ChangeFolder&) = default;
};
// Delete the project, confirmed; its sessions are kept.
struct Delete {
    std::string project;
    friend bool operator==(const Delete&, const Delete&) = default;
};
} // namespace intent

using Intent = std::variant<intent::SaveSettings, intent::ReloadIdeas, intent::SetExcluded, intent::OpenSession,
                            intent::OpenOmpHistory, intent::DeleteOmpHistory, intent::NewChat, intent::ChangeFolder,
                            intent::Delete>;

// What the New project screen asks the app to do.
namespace create_intent {
// Show a folder dialog starting at this folder; the app answers with the folder picked.
struct ChooseFolder {
    std::string folder;
    friend bool operator==(const ChooseFolder&, const ChooseFolder&) = default;
};
// Create the project; the app answers a failure on the screen.
struct Create {
    std::string name;
    ProjectSettings settings;
    friend bool operator==(const Create&, const Create&) = default;
};
struct Cancel {
    friend bool operator==(const Cancel&, const Cancel&) = default;
};
} // namespace create_intent

using CreateIntent = std::variant<create_intent::ChooseFolder, create_intent::Create, create_intent::Cancel>;

} // namespace loa::view::settings::projects
