#pragma once

// Workflows, their editable rules, and intents from the workflows screen.

#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

namespace loa::view::settings::workflows {

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

namespace rules {
struct Managed {
    std::string name;
    std::vector<std::string> allowed;
    std::vector<std::string> disallowed;
    std::string notes;
    ManagedFields fields;
    friend bool operator==(const Managed&, const Managed&) = default;
};
// A marker-delimited body that cannot be represented by the structured editor.
struct External {
    std::string text;
    friend bool operator==(const External&, const External&) = default;
};
struct HandWritten {
    std::string text;
    friend bool operator==(const HandWritten&, const HandWritten&) = default;
};
struct None {
    friend bool operator==(const None&, const None&) = default;
};
} // namespace rules

// Defaults to `rules::None`.
using Rules = std::variant<rules::None, rules::Managed, rules::External, rules::HandWritten>;

struct Workflow {
    std::string role;
    std::vector<std::string> models;
    Rules rules;

    friend bool operator==(const Workflow&, const Workflow&) = default;
};

struct Draft {
    Workflow workflow;

    friend bool operator==(const Draft&, const Draft&) = default;
};

struct AgentInfo {
    std::string name;
    bool enabled = false;

    friend bool operator==(const AgentInfo&, const AgentInfo&) = default;
};

struct FieldError {
    std::string_view field;
    std::string message;

    friend bool operator==(const FieldError&, const FieldError&) = default;
};

namespace error {
struct ChangedOnDisk {
    friend bool operator==(const ChangedOnDisk&, const ChangedOnDisk&) = default;
};
struct Message {
    std::string message;
    friend bool operator==(const Message&, const Message&) = default;
};
} // namespace error

using Error = std::variant<error::ChangedOnDisk, error::Message>;

namespace intent {
struct Open {
    std::string role;
    friend bool operator==(const Open&, const Open&) = default;
};
struct New {
    friend bool operator==(const New&, const New&) = default;
};
struct Save {
    Draft draft;
    std::optional<std::string> previous_role;
    friend bool operator==(const Save&, const Save&) = default;
};
struct Delete {
    std::string role;
    friend bool operator==(const Delete&, const Delete&) = default;
};
struct Reload {
    friend bool operator==(const Reload&, const Reload&) = default;
};
struct Close {
    friend bool operator==(const Close&, const Close&) = default;
};
} // namespace intent

using Intent = std::variant<intent::Open, intent::New, intent::Save, intent::Delete, intent::Reload, intent::Close>;

} // namespace loa::view::settings::workflows
