#pragma once

// Appearance: the saved theme choice plus token overrides, as the UI reads and
// edits it. Theme data only; color tokens live in `loa::ui::theme`.

#include <cstdint>
#include <memory>
#include <string>
#include <string_view>

#include <toml++/impl/forward_declarations.hpp>

namespace loa::view::theme {

// A theme by registry name or file path; `system`, `dark` and `light` are registry names.
struct ThemeRef {
    static constexpr std::string_view SYSTEM = "system";

    std::string value{SYSTEM};

    [[nodiscard]] const std::string& as_str() const noexcept { return value; }

    friend bool operator==(const ThemeRef&, const ThemeRef&) = default;
};

// A TOML table of dotted token keys, compared by value.
class Overrides {
  public:
    Overrides();
    explicit Overrides(toml::table table);
    Overrides(const Overrides& other);
    Overrides(Overrides&&) noexcept;
    Overrides& operator=(const Overrides& other);
    Overrides& operator=(Overrides&&) noexcept;
    ~Overrides();

    [[nodiscard]] const toml::table& table() const noexcept { return *table_; }
    [[nodiscard]] toml::table& table() noexcept { return *table_; }

    friend bool operator==(const Overrides& a, const Overrides& b);

  private:
    std::unique_ptr<toml::table> table_;
};

// The UI theme plus token overrides.
struct Appearance {
    ThemeRef theme;
    Overrides overrides;

    friend bool operator==(const Appearance&, const Appearance&) = default;
};

// An icon the UI draws; the kit maps each to its SVG.
enum class Icon : std::uint8_t {
    ArrowUp,
    Bot,
    Brain,
    Check,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    CircleDot,
    Cpu,
    ExternalLink,
    FileText,
    Files,
    Folder,
    Gauge,
    GitCompare,
    GitFork,
    Globe,
    Image,
    LoaderCircle,
    Maximize,
    Minimize,
    Monitor,
    PanelRight,
    PanelRightClose,
    Pin,
    Plus,
    Search,
    Settings,
    Shield,
    SlidersHorizontal,
    Sparkles,
    Square,
    Terminal,
    TriangleAlert,
    Wrench,
    X,
};

} // namespace loa::view::theme
