#pragma once

// `loa-qt --smoke <screen>`: builds one screen offscreen, shows it, spins the event loop once,
// runs the screen's checks and exits. UI modules register their screens here.

#include <functional>
#include <map>
#include <string>
#include <string_view>
#include <vector>

class QWidget;

namespace loa::ui::app {

class Smoke {
  public:
    // Builds the screen; returns null to fail.
    using Build = std::function<QWidget*()>;
    // Checks the shown screen; returns an error message, or empty when it passed.
    using Check = std::function<std::string(QWidget&)>;

    // The process-wide registry, with the builtin `kit` screen.
    [[nodiscard]] static Smoke& registry();

    void add(std::string name, Build build, Check check = {});
    [[nodiscard]] std::vector<std::string> names() const;

    // 0 when `screen` builds, shows and passes its checks; 1 on failure; 2 when unknown.
    // Needs a running `QApplication`.
    [[nodiscard]] int run(std::string_view screen) const;

  private:
    struct Entry {
        Build build;
        Check check;
    };
    std::map<std::string, Entry, std::less<>> screens_;
};

// The `kit` screen: every icon, tone, text role, button kind, surface and shared widget.
[[nodiscard]] QWidget* kit_gallery();
// Checks the gallery: every icon resource renders, and every widget has an accessible name.
[[nodiscard]] std::string check_kit_gallery(QWidget& gallery);

} // namespace loa::ui::app
