#pragma once

// omp agent definition files: Markdown with a YAML frontmatter block and the agent's
// system prompt as body (port of `models/agents.rs`).
//
// User agents live in `<agent_dir>/agents/`, project agents in `<project>/.omp/agents/`.
// `<name>.md.disabled` is a disabled agent. The frontmatter stays a `YamlDocument`, so
// keys the app does not know, key order, comments and quoting survive an edit, and an
// unedited parse followed by `AgentDefinition::to_text` is byte identical.
//
// The app's own checks (`validate`) are the gate: omp has no command that validates a
// single agent file without running it.

#include <cstdint>
#include <filesystem>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/settings/models/error.hpp"
#include "loa/settings/models/yaml.hpp"

namespace loa::settings::models::agents {

// The `model` key: one model or role alias, or a fallback chain.
struct ModelSpec {
    enum class Kind : std::uint8_t { One, Chain };
    Kind kind = Kind::One;
    std::vector<std::string> items;

    [[nodiscard]] static ModelSpec one(std::string model) { return {Kind::One, {std::move(model)}}; }
    [[nodiscard]] static ModelSpec chain(std::vector<std::string> models) { return {Kind::Chain, std::move(models)}; }
    // Every model entry in order.
    [[nodiscard]] std::span<const std::string> entries() const noexcept { return items; }

    friend bool operator==(const ModelSpec&, const ModelSpec&) = default;
};

class AgentDefinition;
// Parses an agent file.
[[nodiscard]] Result<AgentDefinition> parse(std::string_view text);

// A parsed agent file.
class AgentDefinition {
  public:
    // A blank agent with only `name` and `description` set.
    [[nodiscard]] static Result<AgentDefinition> create(std::string_view name, std::string_view description);

    [[nodiscard]] std::string_view name() const;
    [[nodiscard]] std::string_view description() const;
    [[nodiscard]] std::optional<ModelSpec> model() const;
    [[nodiscard]] std::optional<std::string_view> thinking_level() const;
    // None when the key is absent (omp's default tools).
    [[nodiscard]] std::optional<std::vector<std::string>> tools() const { return list_key("tools"); }
    [[nodiscard]] std::optional<std::vector<std::string>> spawns() const { return list_key("spawns"); }
    [[nodiscard]] const std::string& body() const noexcept { return body_; }

    [[nodiscard]] Result<> set_name(std::string_view name);
    [[nodiscard]] Result<> set_description(std::string_view description);
    [[nodiscard]] Result<> set_model(std::optional<ModelSpec> model);
    [[nodiscard]] Result<> set_thinking_level(std::optional<std::string_view> level);
    [[nodiscard]] Result<> set_tools(std::optional<std::vector<std::string>> tools) { return set_list("tools", std::move(tools)); }
    [[nodiscard]] Result<> set_spawns(std::optional<std::vector<std::string>> spawns) { return set_list("spawns", std::move(spawns)); }
    void set_body(std::string body) { body_ = std::move(body); }

    // The file text. Identical to the parsed input when nothing was edited.
    [[nodiscard]] std::string to_text() const;

  private:
    friend Result<AgentDefinition> parse(std::string_view text);
    AgentDefinition(std::string open, std::string close, bool crlf, yaml::YamlDocument front, std::string body)
        : open_(std::move(open)), close_(std::move(close)), crlf_(crlf), front_(std::move(front)), body_(std::move(body)) {}

    [[nodiscard]] std::optional<std::string_view> str_key(std::string_view key) const;
    [[nodiscard]] std::optional<std::vector<std::string>> list_key(std::string_view key) const;
    Result<> set_list(std::string_view key, std::optional<std::vector<std::string>> list);

    std::string open_;  // `---` line including its line ending
    std::string close_; // closing `---` line including its line ending
    bool crlf_ = false;
    yaml::YamlDocument front_; // frontmatter with LF line endings
    std::string body_;
};

// Where an agent file was found.
enum class AgentSource : std::uint8_t { User, Project };

// An agent file on disk.
struct AgentFile {
    AgentDefinition definition;
    std::filesystem::path path;
    AgentSource source = AgentSource::User;
    bool enabled = true;
};

struct AgentList {
    std::vector<AgentFile> files;
    // Files or directories that failed to read or parse, sorted by path.
    std::vector<std::pair<std::filesystem::path, Error>> broken;
};

// Lists agents in `<agent_dir>/agents` and, when given, `<project>/.omp/agents`, sorted by
// name. Files that fail to read or parse are returned separately.
[[nodiscard]] AgentList list(const std::filesystem::path& agent_dir, const std::optional<std::filesystem::path>& project);

// A problem with one field of an agent.
struct FieldError {
    std::string field;
    std::string message;
    friend bool operator==(const FieldError&, const FieldError&) = default;
};

// Checks an agent. Tool and agent names are checked only against non empty lists.
[[nodiscard]] std::vector<FieldError> validate(const AgentDefinition& def, std::span<const std::string> known_tools,
                                               std::span<const std::string> known_agents);

// Whether `write` may replace an existing agent file.
enum class Overwrite : std::uint8_t { No, Yes };

// Writes `<dir>/<name>.md` atomically, creating `dir`. Refuses unsafe names and, with
// `Overwrite::No`, an existing file or `.md.disabled` twin.
[[nodiscard]] Result<std::filesystem::path> write(const std::filesystem::path& dir, const AgentDefinition& def, Overwrite overwrite);

// Enables or disables an agent by renaming `.md` <-> `.md.disabled`. Returns the new path;
// a file already in the requested state is left alone.
[[nodiscard]] Result<std::filesystem::path> set_enabled(const std::filesystem::path& path, bool enabled);

// Deletes an agent file (Rust `delete`).
[[nodiscard]] Result<> remove(const std::filesystem::path& path);

} // namespace loa::settings::models::agents
