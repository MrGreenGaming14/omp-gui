#pragma once

// The `loa-qt` application: the `QApplication`, the runtime executor and its GUI bridge, and
// the theme. `run()` dispatches `--smoke <screen>` or opens the main window. Port of the iced
// entry point (`loa-app/src/main.rs`); services wiring lands with the app-core issue.

#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "loa/ui/kit/theme.hpp"

class QApplication;

namespace loa::runtime {
class Executor;
}

namespace loa::ui::app {

class AsioBridge;

// What the command line asks for.
struct Options {
    std::optional<std::string> smoke; // `--smoke <screen>`
    bool help = false;
    std::string error; // non-empty when the arguments are invalid
};

[[nodiscard]] Options parse_args(std::span<const std::string_view> args);

class Application {
  public:
    // `argc`/`argv` must outlive the application (Qt keeps references).
    Application(int& argc, char** argv);
    ~Application();
    Application(const Application&) = delete;
    Application& operator=(const Application&) = delete;

    // The process exit code.
    [[nodiscard]] int run();

    // Resolves `a` in `preferred` lightness, makes it the active set and restyles the app.
    // Returns what failed (the theme still applies with fallbacks).
    std::vector<kit::theme::ThemeError> apply_theme(const kit::theme::Appearance& a, kit::theme::Variant preferred);

    [[nodiscard]] QApplication& qt() const { return *qt_; }
    [[nodiscard]] kit::theme::ThemeRegistry& themes() { return themes_; }
    [[nodiscard]] loa::runtime::Executor& executor() const { return *executor_; }
    [[nodiscard]] AsioBridge& bridge() const { return *bridge_; }

  private:
    std::vector<std::string_view> args_;
    std::unique_ptr<QApplication> qt_;
    std::unique_ptr<loa::runtime::Executor> executor_;
    std::unique_ptr<AsioBridge> bridge_;
    kit::theme::ThemeRegistry themes_;
};

} // namespace loa::ui::app
