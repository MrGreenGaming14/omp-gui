/// Decides whether a tool may be listed and called: the per project toggles,
/// and for a peer, its grant. The app supplies it per call site.
pub trait ToolFilter: Send + Sync {
    fn allows(&self, server: &str, tool: &str) -> bool;
}

impl<F: Fn(&str, &str) -> bool + Send + Sync> ToolFilter for F {
    fn allows(&self, server: &str, tool: &str) -> bool {
        self(server, tool)
    }
}

/// Lets every tool through.
#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAll;

impl ToolFilter for AllowAll {
    fn allows(&self, _server: &str, _tool: &str) -> bool {
        true
    }
}
