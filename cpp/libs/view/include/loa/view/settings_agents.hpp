#pragma once

// The agents screen: the omp agent definitions found on disk and the form that
// builds one, mirrored from the agent definition domain. The app checks a form's
// fields; the screen shows the `FieldError`s it reports next to their fields.

#include <cstdint>
#include <filesystem>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

namespace loa::view::settings::agents {

// Where an agent file was found: the user agents folder or the project's `.omp/agents`.
enum class AgentSource : std::uint8_t { User, Project };

// The `model` key: one model or role alias (`std::string`), or a fallback chain.
struct ModelSpec {
    std::variant<std::string, std::vector<std::string>> value;

    // Every model entry in order.
    [[nodiscard]] std::span<const std::string> entries() const noexcept;

    friend bool operator==(const ModelSpec&, const ModelSpec&) = default;
};

// The parts of a definition the form edits. `std::nullopt` leaves the key out, so omp's default applies.
struct AgentFields {
    std::string name;
    std::string description;
    std::optional<ModelSpec> model;
    std::optional<std::string> thinking_level;
    std::optional<std::vector<std::string>> tools;
    std::optional<std::vector<std::string>> spawns;
    // The agent's system prompt.
    std::string body;

    friend bool operator==(const AgentFields&, const AgentFields&) = default;
};

// An agent file on disk.
struct AgentEntry {
    std::filesystem::path path;
    AgentSource source = AgentSource::User;
    bool enabled = false;
    AgentFields fields;

    friend bool operator==(const AgentEntry&, const AgentEntry&) = default;
};

// What the form offers: models and role aliases, thinking levels and tools.
struct Choices {
    // `provider/model` ids and role aliases such as `@slow`.
    std::vector<std::string> models;
    std::vector<std::string> thinking_levels;
    std::vector<std::string> tools;
    // Where the tool list comes from when it isn't omp's own.
    std::optional<std::string> tools_note;

    friend bool operator==(const Choices&, const Choices&) = default;
};

// A problem with one field (`"form"` for the form as a whole).
struct FieldError {
    std::string_view field;
    std::string message;

    friend bool operator==(const FieldError&, const FieldError&) = default;
};

// The form as the app writes it: the edited fields, the listed file they started from (whose keys
// the form has no field for are kept) and the file being edited.
struct Draft {
    AgentFields fields;
    // The file the form was filled from (edited or copied); `std::nullopt` for a blank agent.
    std::optional<std::filesystem::path> base;
    // The file the form edits; `std::nullopt` for a new agent or a copy.
    std::optional<std::filesystem::path> editing;

    friend bool operator==(const Draft&, const Draft&) = default;
};

// What the app does for the screen.
namespace intent {
// Check the draft and answer with its errors; sent by edits once Save showed errors.
struct Check {
    Draft draft;
    friend bool operator==(const Check&, const Check&) = default;
};
// Check the draft and, when it has no errors, write it: back to the file it edits or to a new
// file in the user agents folder. Answer with the errors either way.
struct Save {
    Draft draft;
    friend bool operator==(const Save&, const Save&) = default;
};
// The user confirmed replacing the agent file at `path` with the draft.
struct Replace {
    Draft draft;
    std::filesystem::path path;
    friend bool operator==(const Replace&, const Replace&) = default;
};
// Enable or disable the agent file at the path (the opposite of its state now).
struct Toggle {
    std::filesystem::path path;
    friend bool operator==(const Toggle&, const Toggle&) = default;
};
// The user confirmed deleting the agent file at the path.
struct Delete {
    std::filesystem::path path;
    friend bool operator==(const Delete&, const Delete&) = default;
};
struct Close {
    friend bool operator==(const Close&, const Close&) = default;
};
} // namespace intent

using Intent = std::variant<intent::Check, intent::Save, intent::Replace, intent::Toggle, intent::Delete, intent::Close>;

} // namespace loa::view::settings::agents
