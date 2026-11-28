#pragma once

#include <functional>
#include <string_view>
#include <utility>

namespace loa::mcp {

// Decides whether a tool may be listed and called: the per project toggles, and for a peer, its grant.
class ToolFilter {
  public:
    virtual ~ToolFilter() = default;
    [[nodiscard]] virtual bool allows(std::string_view server, std::string_view tool) const = 0;
};

// Lets every tool through.
class AllowAll final : public ToolFilter {
  public:
    [[nodiscard]] bool allows(std::string_view, std::string_view) const override { return true; }
};

// A filter from a function (the Rust blanket impl for closures).
class FnFilter final : public ToolFilter {
  public:
    explicit FnFilter(std::function<bool(std::string_view, std::string_view)> allows) : allows_(std::move(allows)) {}
    [[nodiscard]] bool allows(std::string_view server, std::string_view tool) const override { return allows_(server, tool); }

  private:
    std::function<bool(std::string_view, std::string_view)> allows_;
};

} // namespace loa::mcp
