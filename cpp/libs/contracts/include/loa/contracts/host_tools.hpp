#pragma once

// Routes one session's host tool calls to the service that registered each tool.

#include <expected>
#include <memory>
#include <span>
#include <string>
#include <unordered_map>
#include <vector>

#include "loa/contracts/harness.hpp"

namespace loa::contracts {

// Two handlers registered the same tool name.
struct DuplicateHostTool {
    std::string name;
    // "host tool `<name>` is already registered".
    [[nodiscard]] std::string message() const { return "host tool `" + name + "` is already registered"; }
    friend bool operator==(const DuplicateHostTool&, const DuplicateHostTool&) = default;
};

// Dispatches host tool calls to the handler that registered each tool.
class HostToolRouter final : public HostTools {
  public:
    std::expected<void, DuplicateHostTool> add(HostToolSpec spec, std::shared_ptr<const HostTools> handler);
    std::expected<void, DuplicateHostTool> add_all(std::vector<HostToolSpec> specs, const std::shared_ptr<const HostTools>& handler);

    // The registered tools, in the order they were added.
    [[nodiscard]] std::span<const HostToolSpec> specs() const noexcept { return specs_; }
    [[nodiscard]] bool empty() const noexcept { return specs_.empty(); }

    // Appends the tools to `config` and makes the router its handler; no-op when empty.
    void install(SessionConfig& config) &&;

    [[nodiscard]] Awaitable<std::expected<std::string, std::string>> call(
        std::string session_id, std::string tool, std::string args) const override;
    [[nodiscard]] Awaitable<std::expected<std::vector<OutputBlock>, std::string>> call_content(
        std::string session_id, std::string tool, std::string args) const override;

  private:
    std::vector<HostToolSpec> specs_;
    std::unordered_map<std::string, std::shared_ptr<const HostTools>> handlers_;
};

} // namespace loa::contracts
