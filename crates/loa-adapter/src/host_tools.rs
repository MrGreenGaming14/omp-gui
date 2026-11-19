//! Rich host tool results and the router that lets several app services
//! serve host tools to one session.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;

use crate::{HostToolSpec, HostTools, SessionConfig};

/// One block of a host tool's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostToolContent {
    Text(String),
    /// Raw image bytes (not base64) with their MIME type, e.g. `image/png`.
    Image { mime: String, data: Bytes },
}

/// Dispatches host tool calls to the handler that registered each tool.
///
/// [`SessionConfig`] carries one handler; the app adds every service's tools
/// here and installs the router.
#[derive(Clone, Default)]
pub struct HostToolRouter {
    specs: Vec<HostToolSpec>,
    handlers: HashMap<String, Arc<dyn HostTools>>,
}

impl std::fmt::Debug for HostToolRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostToolRouter")
            .field("tools", &self.specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>())
            .finish()
    }
}

/// Two handlers registered the same tool name.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("host tool `{0}` is already registered")]
pub struct DuplicateHostTool(pub String);

impl HostToolRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Routes calls of `spec.name` to `handler`.
    pub fn add(&mut self, spec: HostToolSpec, handler: Arc<dyn HostTools>) -> Result<(), DuplicateHostTool> {
        if self.handlers.contains_key(&spec.name) {
            return Err(DuplicateHostTool(spec.name));
        }
        self.handlers.insert(spec.name.clone(), handler);
        self.specs.push(spec);
        Ok(())
    }

    /// Routes every tool of `specs` to `handler`.
    pub fn add_all(
        &mut self,
        specs: impl IntoIterator<Item = HostToolSpec>,
        handler: Arc<dyn HostTools>,
    ) -> Result<(), DuplicateHostTool> {
        for spec in specs {
            self.add(spec, handler.clone())?;
        }
        Ok(())
    }

    /// The registered tools, in the order they were added.
    pub fn specs(&self) -> &[HostToolSpec] {
        &self.specs
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Appends the tools to `config.host_tools` and makes the router the
    /// session's handler. Does nothing without tools.
    pub fn install(self, config: &mut SessionConfig) {
        if self.is_empty() {
            return;
        }
        config.host_tools.extend(self.specs.iter().cloned());
        config.host_tool_handler = Some(Arc::new(self));
    }

    fn handler(&self, tool: &str) -> Result<&Arc<dyn HostTools>, String> {
        self.handlers.get(tool).ok_or_else(|| format!("unknown host tool `{tool}`"))
    }
}

#[async_trait]
impl HostTools for HostToolRouter {
    async fn call(&self, session_id: &str, tool: &str, args: &str) -> Result<String, String> {
        self.handler(tool)?.call(session_id, tool, args).await
    }

    async fn call_content(&self, session_id: &str, tool: &str, args: &str) -> Result<Vec<HostToolContent>, String> {
        self.handler(tool)?.call_content(session_id, tool, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo(&'static str);

    #[async_trait]
    impl HostTools for Echo {
        async fn call(&self, session_id: &str, tool: &str, args: &str) -> Result<String, String> {
            Ok(format!("{}:{session_id}:{tool}:{args}", self.0))
        }
    }

    struct Picture;

    #[async_trait]
    impl HostTools for Picture {
        async fn call(&self, _: &str, _: &str, _: &str) -> Result<String, String> {
            Err("images only".into())
        }

        async fn call_content(&self, _: &str, _: &str, _: &str) -> Result<Vec<HostToolContent>, String> {
            Ok(vec![HostToolContent::Image { mime: "image/png".into(), data: Bytes::from_static(b"png") }])
        }
    }

    fn spec(name: &str) -> HostToolSpec {
        HostToolSpec { name: name.into(), description: String::new(), input_schema: "{}".into() }
    }

    #[tokio::test]
    async fn calls_reach_the_handler_that_registered_the_tool() {
        let mut router = HostToolRouter::new();
        router.add_all([spec("a"), spec("b")], Arc::new(Echo("one"))).unwrap();
        router.add(spec("shot"), Arc::new(Picture)).unwrap();
        assert_eq!(router.add(spec("a"), Arc::new(Echo("two"))), Err(DuplicateHostTool("a".into())));

        assert_eq!(router.call("s1", "b", "{}").await.unwrap(), "one:s1:b:{}");
        assert_eq!(
            router.call_content("s1", "a", "{\"x\":1}").await.unwrap(),
            [HostToolContent::Text("one:s1:a:{\"x\":1}".into())]
        );
        assert!(matches!(&router.call_content("s1", "shot", "{}").await.unwrap()[0], HostToolContent::Image { mime, .. } if mime == "image/png"));
        assert_eq!(router.call("s1", "nope", "{}").await.unwrap_err(), "unknown host tool `nope`");
    }

    #[test]
    fn install_appends_tools_and_sets_the_handler() {
        let mut config = SessionConfig { host_tools: vec![spec("existing")], ..Default::default() };
        HostToolRouter::new().install(&mut config);
        assert!(config.host_tool_handler.is_none());

        let mut router = HostToolRouter::new();
        router.add(spec("a"), Arc::new(Echo("one"))).unwrap();
        router.install(&mut config);
        let names: Vec<_> = config.host_tools.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["existing", "a"]);
        assert!(config.host_tool_handler.is_some());
    }
}
