//! Desktop notifications for the decision desk, through `notify-rust` (D-Bus on Linux).

use loa_view::session::port::{Notice, Notifier, NotifyError, NotifyFuture};

#[derive(Clone, Debug)]
pub struct DesktopNotifier(pub String);

impl DesktopNotifier {
    pub fn new(app_name: impl Into<String>) -> Self { Self(app_name.into()) }
}

impl Notifier for DesktopNotifier {
    fn notify(&self, n: &Notice) -> NotifyFuture {
        let mut notification = notify_rust::Notification::new();
        notification.appname(&self.0).summary(&n.heading).body(&n.summary);
        Box::pin(async move { notification.show_async().await.map(drop).map_err(|e| NotifyError(e.to_string())) })
    }
}
