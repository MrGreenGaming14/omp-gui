#include "loa/ui/app/notifier.hpp"

#include <asio/awaitable.hpp>

#if defined(__linux__)
#include <libnotify/notify.h>
#endif

namespace loa::ui::app {

namespace port = loa::view::session::port;

namespace {

// Shows one notification; runs on the executor that awaits it, never the UI thread.
port::NotifyFuture show(std::string app_name, std::string heading, std::string summary) {
#if defined(__linux__)
    if (!notify_is_initted() && !notify_init(app_name.c_str()))
        co_return std::unexpected(port::NotifyError{"libnotify could not be initialized"});
    NotifyNotification* n = notify_notification_new(heading.c_str(), summary.c_str(), nullptr);
    GError* error = nullptr;
    const bool shown = notify_notification_show(n, &error);
    g_object_unref(G_OBJECT(n));
    if (!shown) {
        std::string reason = error ? error->message : "notification not shown";
        if (error) g_error_free(error);
        co_return std::unexpected(port::NotifyError{std::move(reason)});
    }
    co_return std::expected<void, port::NotifyError>{};
#else
    (void)app_name, (void)heading, (void)summary;
    co_return std::unexpected(port::NotifyError{"desktop notifications are not supported on this platform"});
#endif
}

} // namespace

DesktopNotifier::DesktopNotifier(std::string app_name) : app_name_(std::move(app_name)) {}

port::NotifyFuture DesktopNotifier::notify(const port::Notice& notice) const { return show(app_name_, notice.heading, notice.summary); }

} // namespace loa::ui::app
