#pragma once

// What a harness does natively; the app provides the rest on top.

#include <algorithm>
#include <cstdint>
#include <span>
#include <vector>

#include "loa/core/schema.hpp"

namespace loa::contracts {

// An omp feature the app guarantees on every harness.
enum class Feature {
    Fork,
    // Branch and rewind inside the harness's own session tree.
    Rewind,
    Compaction,
    ModelRoles,
    Subagents,
    // The user can message a running subagent directly and read its live
    // conversation (`HarnessSession::message_subagent`).
    SubagentMessages,
    PlanMode,
    // Hashline edits, LSP, DAP, AST edits.
    OmpTools,
    AdvisorRules,
};

// When the harness lets the model change.
enum class ModelSwitch {
    Never,
    PerTurn,
    // Any time, including mid turn.
    PerCall,
};

// How project context reaches the harness.
enum class ContextDelivery {
    SystemPrompt,
    ContextFile,
    McpResource,
};

class CapsBuilder;

// Declared by `Adapter::capabilities`; the default only takes prompts.
struct Capabilities {
    // Element schema version the adapter emits.
    std::uint32_t schema_version = 0;
    std::vector<Feature> features;
    ModelSwitch model_switch = ModelSwitch::Never;
    std::vector<loa::core::DecisionKind> decisions;
    // Best first.
    std::vector<ContextDelivery> context_delivery;
    bool host_tools = false;
    bool mcp_servers = false;
    // A prompt sent mid turn is queued instead of `Busy`.
    bool queue_mid_turn = false;
    bool effort = false;
    bool import = false;
    bool export_ = false;

    [[nodiscard]] static CapsBuilder builder();
    [[nodiscard]] bool has(Feature feature) const { return std::ranges::find(features, feature) != features.end(); }

    friend bool operator==(const Capabilities&, const Capabilities&) = default;
};

// Chained construction of `Capabilities`.
class CapsBuilder {
  public:
    CapsBuilder& host_tools() { caps_.host_tools = true; return *this; }
    CapsBuilder& mcp_servers() { caps_.mcp_servers = true; return *this; }
    CapsBuilder& queue_mid_turn() { caps_.queue_mid_turn = true; return *this; }
    CapsBuilder& effort() { caps_.effort = true; return *this; }
    CapsBuilder& import() { caps_.import = true; return *this; }
    CapsBuilder& export_() { caps_.export_ = true; return *this; }
    CapsBuilder& schema(std::uint32_t v) { caps_.schema_version = v; return *this; }
    CapsBuilder& model_switch(ModelSwitch v) { caps_.model_switch = v; return *this; }
    CapsBuilder& features(std::span<const Feature> v) { caps_.features.insert(caps_.features.end(), v.begin(), v.end()); return *this; }
    CapsBuilder& decisions(std::span<const loa::core::DecisionKind> v) { caps_.decisions.insert(caps_.decisions.end(), v.begin(), v.end()); return *this; }
    CapsBuilder& context(std::span<const ContextDelivery> v) { caps_.context_delivery.insert(caps_.context_delivery.end(), v.begin(), v.end()); return *this; }

    [[nodiscard]] Capabilities build() const& { return caps_; }
    [[nodiscard]] Capabilities build() && { return std::move(caps_); }

  private:
    Capabilities caps_;
};

inline CapsBuilder Capabilities::builder() { return {}; }

} // namespace loa::contracts
