//! Typed agent tools: a [`Tool`] is registered in a [`ToolSet`], served as host tools or over MCP.

use crate::harness::{HostToolSpec, HostTools};
use async_trait::async_trait;
use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use std::collections::BTreeMap;
use std::sync::Arc;

/// One block of a tool result; image `data` is raw bytes, not base64.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputBlock {
    Text(String),
    Image { mime: String, data: Bytes },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolOutput {
    pub blocks: Vec<OutputBlock>,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self { blocks: vec![OutputBlock::Text(text.into())], is_error: false }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self { is_error: true, ..Self::text(text) }
    }

    /// The text blocks joined by newlines.
    pub fn joined_text(&self) -> String {
        let texts = self.blocks.iter().filter_map(|b| if let OutputBlock::Text(t) = b { Some(t.as_str()) } else { None });
        texts.collect::<Vec<_>>().join("\n")
    }
}

/// MCP-style hints about what a tool does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolAnnotations {
    pub title: Option<String>,
    pub read_only: bool,
    pub destructive: bool,
}

/// Who is calling.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallCtx {
    pub session_id: String,
}

/// A tool with typed arguments; its JSON Schema comes from `Args`.
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    type Args: DeserializeOwned + schemars::JsonSchema + Send;
    const NAME: &'static str;
    fn description(&self) -> &str;
    fn annotations(&self) -> ToolAnnotations { ToolAnnotations::default() }
    async fn run(&self, ctx: &CallCtx, args: Self::Args) -> Result<ToolOutput, String>;
}

/// Object-safe form of [`Tool`], taking JSON arguments.
#[async_trait]
pub trait DynTool: Send + Sync {
    fn spec(&self) -> HostToolSpec;
    fn annotations(&self) -> ToolAnnotations;
    async fn call(&self, ctx: &CallCtx, args: Json) -> Result<ToolOutput, String>;
}

#[async_trait]
impl<T: Tool> DynTool for T {
    fn spec(&self) -> HostToolSpec {
        let schema = serde_json::to_string(&schemars::schema_for!(T::Args)).unwrap_or_else(|_| "{}".into());
        HostToolSpec { name: T::NAME.into(), description: self.description().into(), input_schema: schema }
    }

    fn annotations(&self) -> ToolAnnotations { Tool::annotations(self) }

    async fn call(&self, ctx: &CallCtx, args: Json) -> Result<ToolOutput, String> {
        let args = serde_json::from_value(args).map_err(|e| format!("invalid arguments for `{}`: {e}", T::NAME))?;
        self.run(ctx, args).await
    }
}

/// Tools by name.
#[derive(Clone, Default)]
pub struct ToolSet {
    tools: BTreeMap<String, Arc<dyn DynTool>>,
}

impl ToolSet {
    pub fn new() -> Self { Self::default() }

    /// Adds `tool`, replacing one with the same name.
    pub fn register<T: Tool>(&mut self, tool: T) -> &mut Self {
        self.add(Arc::new(tool))
    }

    /// Adds a tool whose name and schema are only known at runtime.
    pub fn add(&mut self, tool: Arc<dyn DynTool>) -> &mut Self {
        self.tools.insert(tool.spec().name, tool);
        self
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn DynTool>> { self.tools.get(name) }

    pub fn specs(&self) -> Vec<HostToolSpec> { self.tools.values().map(|t| t.spec()).collect() }

    pub async fn call(&self, ctx: &CallCtx, name: &str, args: Json) -> Result<ToolOutput, String> {
        self.get(name).ok_or_else(|| format!("unknown tool `{name}`"))?.call(ctx, args).await
    }
}

#[async_trait]
impl HostTools for ToolSet {
    async fn call(&self, session_id: &str, tool: &str, args: &str) -> Result<String, String> {
        self.call_content(session_id, tool, args).await.map(|blocks| ToolOutput { blocks, is_error: false }.joined_text())
    }

    async fn call_content(&self, session_id: &str, tool: &str, args: &str) -> Result<Vec<OutputBlock>, String> {
        let args = if args.trim().is_empty() { Json::Null } else { serde_json::from_str(args).map_err(|e| e.to_string())? };
        let out = ToolSet::call(self, &CallCtx { session_id: session_id.into() }, tool, args).await?;
        if out.is_error { Err(out.joined_text()) } else { Ok(out.blocks) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct Args {
        word: String,
    }

    struct Shout;

    #[async_trait]
    impl Tool for Shout {
        type Args = Args;
        const NAME: &'static str = "shout";
        fn description(&self) -> &str { "uppercases" }
        async fn run(&self, ctx: &CallCtx, args: Args) -> Result<ToolOutput, String> {
            match args.word.as_str() {
                "" => Ok(ToolOutput::error("empty")),
                w => Ok(ToolOutput::text(format!("{}:{}", ctx.session_id, w.to_uppercase()))),
            }
        }
    }

    #[tokio::test]
    async fn tool_set_serves_specs_and_host_tool_calls() {
        let mut set = ToolSet::new();
        set.register(Shout);
        let spec = &set.specs()[0];
        assert_eq!((spec.name.as_str(), spec.description.as_str()), ("shout", "uppercases"));
        assert!(spec.input_schema.contains("\"word\""), "{}", spec.input_schema);
        for (tool, args, want) in [
            ("shout", r#"{"word":"hi"}"#, Ok("s:HI")),
            ("shout", r#"{"word":""}"#, Err("empty")),
            ("shout", "{}", Err("invalid arguments for `shout`: missing field `word`")),
            ("nope", "{}", Err("unknown tool `nope`")),
        ] {
            assert_eq!(HostTools::call(&set, "s", tool, args).await.as_deref(), want.map_err(String::from).as_deref());
        }
    }
}
