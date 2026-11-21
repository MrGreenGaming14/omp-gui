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
        Box::pin(async move {
            #[cfg(not(windows))]
            let shown = notification.show_async().await;
            // `show_async` does not exist on Windows: run the blocking
            // `show` on its own thread so the UI never waits on it.
            #[cfg(windows)]
            let shown = {
                let (sender, receiver) = futures::channel::oneshot::channel();
                std::thread::spawn(move || {
                    let _ = sender.send(notification.show());
                });
                receiver.await.map_err(|e| NotifyError(e.to_string()))?
            };
            shown.map(drop).map_err(|e| NotifyError(e.to_string()))
        })
    }
}
