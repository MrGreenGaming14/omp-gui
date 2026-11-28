#pragma once

// The `surface_*` agent tools: one `Tool` each, on a `ToolSet` per scope. Port
// of `loa-surfaces/src/hub/tools.rs`.

#include <atomic>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/tools.hpp"

namespace loa::surfaces::hub {

class SurfaceHub;

inline constexpr std::string_view LIST = "surface_list";
inline constexpr std::string_view SCREENSHOT = "surface_screenshot";
inline constexpr std::string_view CLICK = "surface_click";
inline constexpr std::string_view TYPE = "surface_type";
inline constexpr std::string_view SCROLL = "surface_scroll";
inline constexpr std::string_view OPEN_BROWSER = "surface_open_browser";
inline constexpr std::string_view NAVIGATE = "surface_navigate";
inline constexpr std::string_view PAGE_TEXT = "surface_page_text";
inline constexpr std::string_view ELEMENTS = "surface_elements";
inline constexpr std::string_view DOCUMENT_DATA = "surface_document_data";

// The tools served for `scope` (a built-in server name), or every tool for none.
[[nodiscard]] loa::contracts::ToolSet tool_set(const std::shared_ptr<SurfaceHub>& hub, std::optional<std::string_view> scope);

// The surface tools over a `SurfaceHub`, as host tools.
class SurfaceTools final : public loa::contracts::HostTools, public std::enable_shared_from_this<SurfaceTools> {
  public:
    [[nodiscard]] static std::shared_ptr<SurfaceTools> make(std::shared_ptr<SurfaceHub> hub);

    [[nodiscard]] const std::shared_ptr<SurfaceHub>& hub() const noexcept { return hub_; }

    // The tools as host tools, with the handler that runs them.
    [[nodiscard]] std::pair<std::vector<loa::contracts::HostToolSpec>, std::shared_ptr<const loa::contracts::HostTools>> host_tools() const;

    // Runs tool `name`; with a `scope` only that built-in server's tools and surfaces are reachable.
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::ToolOutput> run(std::string name, loa::contracts::Json args,
                                                                          std::optional<std::string> scope) const;

    // Text-only result: screenshots are saved as PNG files and their path returned.
    [[nodiscard]] loa::contracts::Awaitable<std::expected<std::string, std::string>> call(std::string session_id, std::string tool,
                                                                                         std::string args) const override;
    [[nodiscard]] loa::contracts::Awaitable<std::expected<std::vector<loa::contracts::OutputBlock>, std::string>> call_content(
        std::string session_id, std::string tool, std::string args) const override;

  private:
    SurfaceTools(std::shared_ptr<SurfaceHub> hub, loa::contracts::ToolSet set);
    // Saves a screenshot for a text-only result.
    [[nodiscard]] std::expected<std::filesystem::path, std::string> save_screenshot(std::span<const std::byte> png) const;

    std::shared_ptr<SurfaceHub> hub_;
    loa::contracts::ToolSet set_;
    mutable std::atomic<std::uint64_t> shots_{0};
};

} // namespace loa::surfaces::hub
