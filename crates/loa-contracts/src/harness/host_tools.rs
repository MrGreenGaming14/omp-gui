//! Routes one session's host tool calls to the service that registered each tool.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::{HostToolSpec, HostTools, SessionConfig};
use crate::tools::OutputBlock;

/// Dispatches host tool calls to the handler that registered each tool.
#[derive(Clone, Default)]
pub struct HostToolRouter {
    specs: Vec<HostToolSpec>,
    handlers: HashMap<String, Arc<dyn HostTools>>,
}

impl std::fmt::Debug for HostToolRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.specs.iter().map(|s| &s.name)).finish()
    }
}

/// Two handlers registered the same tool name.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("host tool `{0}` is already registered")]
pub struct DuplicateHostTool(pub String);

impl HostToolRouter {
    pub fn new() -> Self { Self::default() }

    pub fn add(&mut self, spec: HostToolSpec, handler: Arc<dyn HostTools>) -> Result<(), DuplicateHostTool> {
        if self.handlers.contains_key(&spec.name) {
            return Err(DuplicateHostTool(spec.name));
        }
        self.handlers.insert(spec.name.clone(), handler);
        self.specs.push(spec);
        Ok(())
    }

    pub fn add_all(
        &mut self,
        specs: impl IntoIterator<Item = HostToolSpec>,
        handler: Arc<dyn HostTools>,
    ) -> Result<(), DuplicateHostTool> {
        specs.into_iter().try_for_each(|spec| self.add(spec, handler.clone()))
    }

    /// The registered tools, in the order they were added.
    pub fn specs(&self) -> &[HostToolSpec] { &self.specs }

    pub fn is_empty(&self) -> bool { self.specs.is_empty() }

    /// Appends the tools to `config` and makes the router its handler; no-op when empty.
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

    async fn call_content(&self, session_id: &str, tool: &str, args: &str) -> Result<Vec<OutputBlock>, String> {
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

    fn spec(name: &str) -> HostToolSpec {
        HostToolSpec { name: name.into(), description: String::new(), input_schema: "{}".into() }
    }

    #[tokio::test]
    async fn routes_calls_and_installs_into_the_config() {
        let mut router = HostToolRouter::new();
        router.add_all([spec("a"), spec("b")], Arc::new(Echo("one"))).unwrap();
        assert_eq!(router.add(spec("a"), Arc::new(Echo("two"))), Err(DuplicateHostTool("a".into())));
        assert_eq!(router.call("s1", "b", "{}").await.unwrap(), "one:s1:b:{}");
        assert_eq!(router.call_content("s1", "a", "1").await.unwrap(), [OutputBlock::Text("one:s1:a:1".into())]);
        assert_eq!(router.call("s1", "nope", "{}").await.unwrap_err(), "unknown host tool `nope`");

        let mut config = SessionConfig { host_tools: vec![spec("existing")], ..Default::default() };
        HostToolRouter::new().install(&mut config);
        assert!(config.host_tool_handler.is_none());
        router.install(&mut config);
        let names: Vec<_> = config.host_tools.iter().map(|s| s.name.as_str()).collect();
        assert_eq!((names, config.host_tool_handler.is_some()), (vec!["existing", "a", "b"], true));
    }
}
