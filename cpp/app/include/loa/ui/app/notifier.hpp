#pragma once

// The desktop `Notifier` (port of `loa-app/src/adapters/notifier.rs`): libnotify on Linux.

#include <string>

#include "loa/view/session.hpp"

namespace loa::ui::app {

class DesktopNotifier final : public loa::view::session::port::Notifier {
  public:
    // `app_name` is the notification's application name.
    explicit DesktopNotifier(std::string app_name);
    [[nodiscard]] loa::view::session::port::NotifyFuture notify(const loa::view::session::port::Notice& notice) const override;

  private:
    std::string app_name_;
};

} // namespace loa::ui::app
