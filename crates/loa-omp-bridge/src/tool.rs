use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::Result;

/// A tool the bridge offers, with the JSON Schema of its arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema object for the tool's arguments, as the executor reports it.
    pub input_schema: Map<String, Value>,
}

/// One block of a tool's result.
#[derive(Debug, Clone, PartialEq)]
pub enum OutputBlock {
    Text(String),
    /// Base64 encoded image data.
    Image { data: String, mime_type: String },
}

/// The result of one tool execution.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub content: Vec<OutputBlock>,
    /// The tool itself reported failure (bad arguments, stale hashline tag, ...).
    pub is_error: bool,
}

/// Port the MCP server runs tools through.
///
/// [`crate::OmpExecutor`] implements it with a real omp process. Another
/// implementation only has to report its tools and execute calls.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync + 'static {
    /// Tools offered to MCP clients. Fixed for the executor's lifetime.
    fn tools(&self) -> &[ToolSpec];

    /// Executes `name` with `arguments`. When `cancel` fires the executor stops
    /// the call as soon as it can and returns [`crate::Error::Cancelled`].
    async fn call(
        &self,
        name: &str,
        arguments: Map<String, Value>,
        cancel: CancellationToken,
    ) -> Result<ToolOutput>;
}
