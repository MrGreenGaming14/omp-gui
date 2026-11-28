#pragma once

// Per-project settings and what the project screen shows about a project's sessions.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/core/value.hpp"
#include "loa/runtime/fs.hpp"

namespace loa::settings {

// A file every chat of the project should know about, with why it is pinned.
struct PinnedFile {
    std::string path;
    std::optional<std::string> note;
    friend bool operator==(const PinnedFile&, const PinnedFile&) = default;
};

// Settings shared by every session of a project; `digest_model` none uses the summarizer's default.
struct ProjectSettings {
    std::string instructions;
    std::vector<PinnedFile> pinned;
    std::optional<std::string> digest_model;
    // Working directory of the project, set when it is created or its folder changed; none when it has none.
    std::optional<std::filesystem::path> folder;
    // Whether the project's ideas notes go into the context of its new chats; off by default.
    bool ideas_in_context = false;
    friend bool operator==(const ProjectSettings&, const ProjectSettings&) = default;
};

// One touched file and a one line note.
struct FileNote {
    std::string path;
    std::string note;

    // Parses the schema's `"path: note"` form.
    [[nodiscard]] static FileNote parse(std::string_view text);
    [[nodiscard]] std::string encode() const;
    friend bool operator==(const FileNote&, const FileNote&) = default;
};

// What a digest says about its session.
struct DigestContent {
    std::string summary;
    std::vector<std::string> decisions;
    std::vector<FileNote> files;
    // Unfinished todos and unanswered questions.
    std::vector<std::string> open_items;
    // The turns the digest came from.
    std::vector<loa::core::NodeId> pointers;
    friend bool operator==(const DigestContent&, const DigestContent&) = default;
};

enum class DigestState : std::uint8_t { Missing, Fresh, Stale };

// One session on the project screen.
struct SessionRow {
    std::string id;
    std::optional<std::string> title;
    std::optional<std::string> harness;
    std::int64_t created = 0;
    std::int64_t last_active = 0;
    std::size_t turns = 0;
    bool excluded = false;
    DigestState digest_state = DigestState::Missing;
    std::optional<DigestContent> digest;
    std::optional<std::string> digest_model;
    friend bool operator==(const SessionRow&, const SessionRow&) = default;
};

// A project's ideas notes as read: their text, empty when there are none, and the file as
// seen then, so saving them refuses to replace an edit made outside since.
struct Ideas {
    std::string text;
    loa::runtime::fs::Snapshot file;
};

// Everything the project screen shows; sessions newest activity first.
struct ProjectOverview {
    std::string name;
    std::optional<std::string> cwd;
    ProjectSettings settings;
    // The project's ideas notes, or why they can't be read.
    std::expected<Ideas, std::string> ideas;
    std::vector<SessionRow> sessions;
};

} // namespace loa::settings
