#pragma once

// Workflows stored as omp model roles and sections of the agent RULES.md (port of
// `models/workflows.rs`).
//
// A workflow is a `modelRoles` entry of `config.yml` plus, optionally, its section of
// `RULES.md`: either a marker-delimited section the app renders (`Rules::Kind::Managed`),
// a marker-delimited body it keeps verbatim (`External`) or a hand-written `## <Name> Mode`
// section (`HandWritten`). Every edit leaves the bytes outside the edited section alone.

#include <cstdint>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/settings/models/error.hpp"
#include "loa/settings/models/files.hpp"
#include "loa/settings/models/selector.hpp"

namespace loa::settings::models::workflows {

struct RoutingNote {
    std::string agent;
    std::string note;
    friend bool operator==(const RoutingNote&, const RoutingNote&) = default;
};

struct RuleBlock {
    std::string heading;
    std::string text;
    friend bool operator==(const RuleBlock&, const RuleBlock&) = default;
};

struct ManagedFields {
    std::string description;
    std::vector<RoutingNote> routing;
    std::vector<std::string> steps;
    std::vector<RuleBlock> blocks;
    friend bool operator==(const ManagedFields&, const ManagedFields&) = default;
};

// A workflow's RULES.md section (Rust enum `Rules`). Fields not used by `kind` stay empty.
struct Rules {
    enum class Kind : std::uint8_t {
        Managed,     // a marker-delimited section; `name` is its display name
        External,    // a marker-delimited body the structured editor cannot represent: `text`
        HandWritten, // `text`
        None,
    };
    Kind kind = Kind::None;
    std::string name;
    std::vector<std::string> allowed;
    std::vector<std::string> disallowed;
    std::string notes;
    ManagedFields fields;
    std::string text;

    [[nodiscard]] static Rules managed(std::string name, std::vector<std::string> allowed, std::vector<std::string> disallowed,
                                       std::string notes, ManagedFields fields = {}) {
        return {Kind::Managed, std::move(name), std::move(allowed), std::move(disallowed), std::move(notes), std::move(fields), {}};
    }
    [[nodiscard]] static Rules external(std::string text) { return {Kind::External, {}, {}, {}, {}, {}, std::move(text)}; }
    [[nodiscard]] static Rules hand_written(std::string text) { return {Kind::HandWritten, {}, {}, {}, {}, {}, std::move(text)}; }
    [[nodiscard]] static Rules none() { return {}; }

    friend bool operator==(const Rules&, const Rules&) = default;
};

struct Workflow {
    std::string role;
    std::vector<selector::Selector> models;
    Rules rules;
    friend bool operator==(const Workflow&, const Workflow&) = default;
};

// Immutable load-time contents of both files, used to reject stale edits.
class WorkflowSnapshot {
  private:
    friend struct WorkflowFiles;
    WorkflowSnapshot(runtime::fs::Snapshot config, runtime::fs::Snapshot rules) : config_(std::move(config)), rules_(std::move(rules)) {}
    runtime::fs::Snapshot config_;
    runtime::fs::Snapshot rules_;
};

// Derives a role from a display name (`Deep Research` -> `DEEP_RESEARCH`).
[[nodiscard]] Result<std::string> role_for(std::string_view name);

struct WorkflowFiles {
    std::filesystem::path config;
    std::filesystem::path rules;

    [[nodiscard]] static WorkflowFiles from(const files::OmpFiles& files) { return in_agent_dir(files.agent_dir); }
    [[nodiscard]] static WorkflowFiles in_agent_dir(const std::filesystem::path& dir) { return {dir / "config.yml", dir / "RULES.md"}; }

    // Loads all config roles and an optimistic-concurrency snapshot of both files.
    [[nodiscard]] Result<std::pair<std::vector<Workflow>, WorkflowSnapshot>> load() const;

    // Creates, updates or renames a workflow. Existing hand-written sections remain
    // hand-written. `previous_role` is none for a new workflow and the old role for an
    // update or rename. Call `load` again after a successful edit to obtain a fresh
    // snapshot before the next edit. Every argument must outlive the coroutine.
    [[nodiscard]] contracts::Awaitable<Result<>> save(const Workflow& workflow, std::optional<std::string_view> previous_role,
                                                      const WorkflowSnapshot& snapshot, const SettingsValidator& validator) const;

    // Removes `role` from `config.yml` and its section from RULES.md (Rust `delete`).
    // Every argument must outlive the coroutine.
    [[nodiscard]] contracts::Awaitable<Result<>> remove(std::string_view role, const WorkflowSnapshot& snapshot,
                                                        const SettingsValidator& validator) const;

    friend bool operator==(const WorkflowFiles&, const WorkflowFiles&) = default;
};

} // namespace loa::settings::models::workflows
