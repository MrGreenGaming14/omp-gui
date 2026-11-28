#pragma once

// The `sandbox_task` runs the app lists, built from `SandboxEvent`s (Rust `loa_sandbox::runs`).

#include <cstddef>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/sandbox/isolation.hpp"
#include "loa/sandbox/tool.hpp"

namespace loa::sandbox {

namespace run_state {
struct Running {
    friend bool operator==(const Running&, const Running&) = default;
};
struct Finished {
    std::optional<std::string> final_message;
    // The isolated run's patch; none for runs on the workspace itself.
    std::optional<std::string> patch;
    // The files the patch touches.
    std::vector<PatchFile> files;
    // Set once the patch was applied to the workspace.
    bool applied = false;
    friend bool operator==(const Finished&, const Finished&) = default;
};
struct Failed {
    std::string error;
    friend bool operator==(const Failed&, const Failed&) = default;
};
} // namespace run_state

using RunState = std::variant<run_state::Running, run_state::Finished, run_state::Failed>;

struct RunEntry {
    SandboxRun run;
    RunState state;

    // A patch that can still be applied.
    [[nodiscard]] std::optional<std::string_view> pending_patch() const;
    friend bool operator==(const RunEntry&, const RunEntry&) = default;
};

// Runs in the order they started.
class RunLog {
  public:
    void record(SandboxEvent event);
    [[nodiscard]] std::span<const RunEntry> entries() const noexcept { return entries_; }
    [[nodiscard]] const RunEntry* get(std::uint64_t run) const;
    [[nodiscard]] std::size_t running() const;
    // Records that the run's patch was applied (with `apply_patch`) to its workspace.
    void mark_applied(std::uint64_t run);

  private:
    RunEntry* find(std::uint64_t run);
    std::vector<RunEntry> entries_;
};

} // namespace loa::sandbox
