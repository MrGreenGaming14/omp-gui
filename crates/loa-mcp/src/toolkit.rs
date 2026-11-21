//! A [`ToolSet`] served over MCP, and the conversions between rmcp tool
//! results and [`ToolOutput`]. (Both types are foreign here, so the
//! conversions are functions rather than `From` impls.)

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use loa_contracts::tools::{CallCtx, OutputBlock, ToolOutput, ToolSet};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, JsonObject, ListToolsResult,
    PaginatedRequestParams, ResourceContents, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{Error, Result};

/// An image block from base64 `data`; `None` when it doesn't decode.
pub fn image(data: &str, mime: &str) -> Option<OutputBlock> {
    Some(OutputBlock::Image { mime: mime.to_owned(), data: STANDARD.decode(data).ok()?.into() })
}

/// What a model can read of an rmcp result; structured content stands in when there is no other content.
pub fn to_output(result: CallToolResult) -> ToolOutput {
    let mut blocks: Vec<OutputBlock> = result
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => OutputBlock::Text(text.text.clone()),
            ContentBlock::Image(i) => image(&i.data, &i.mime_type).unwrap_or_else(|| OutputBlock::Text(format!("[undecodable {} image]", i.mime_type))),
            ContentBlock::Resource(embedded) => OutputBlock::Text(match &embedded.resource {
                ResourceContents::TextResourceContents { text, .. } => text.clone(),
                ResourceContents::BlobResourceContents { uri, .. } => format!("[binary resource {uri}]"),
                _ => "[resource]".to_owned(),
            }),
            ContentBlock::ResourceLink(link) => OutputBlock::Text(format!("[resource {}]", link.uri)),
            ContentBlock::Audio(audio) => OutputBlock::Text(format!("[{} audio]", audio.mime_type)),
            _ => OutputBlock::Text("[unsupported content]".to_owned()),
        })
        .collect();
    if let (true, Some(value)) = (blocks.is_empty(), &result.structured_content) {
        blocks.push(OutputBlock::Text(value.to_string()));
    }
    ToolOutput { blocks, is_error: result.is_error == Some(true) }
}

/// The rmcp result for `output`, images base64 encoded.
pub fn to_result(output: ToolOutput) -> CallToolResult {
    let content = output.blocks.into_iter().map(|block| match block {
        OutputBlock::Text(text) => ContentBlock::text(text),
        OutputBlock::Image { mime, data } => ContentBlock::image(STANDARD.encode(data), mime),
    });
    if output.is_error { CallToolResult::error(content.collect()) } else { CallToolResult::success(content.collect()) }
}

/// Text of `output` with images as placeholders.
pub fn text(output: &ToolOutput) -> String {
    let lines = output.blocks.iter().map(|block| match block {
        OutputBlock::Text(text) => text.clone(),
        OutputBlock::Image { mime, data } => format!("[{mime} image, {} bytes]", data.len()),
    });
    lines.collect::<Vec<_>>().join("\n")
}

/// An MCP server offering the tools of a [`ToolSet`].
#[derive(Clone)]
pub struct ToolSetMcp {
    tools: Arc<ToolSet>,
    info: Implementation,
    instructions: Option<String>,
}

impl ToolSetMcp {
    pub fn new(tools: ToolSet, name: &str) -> Self {
        Self { tools: Arc::new(tools), info: Implementation::new(name, env!("CARGO_PKG_VERSION")), instructions: None }
    }

    pub fn with_instructions(mut self, text: &str) -> Self {
        self.instructions = Some(text.to_owned());
        self
    }

    /// Serves on `reader`/`writer` until the client disconnects.
    pub async fn serve<R, W>(self, reader: R, writer: W) -> Result<()>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let failed = |e: String| Error::Bridge(format!("MCP transport error: {e}"));
        let running = ServiceExt::serve(self, (reader, writer)).await.map_err(|e| failed(e.to_string()))?;
        running.waiting().await.map(drop).map_err(|e| failed(e.to_string()))
    }

    /// Serves on this process's stdin and stdout.
    pub async fn serve_stdio(self) -> Result<()> {
        let (stdin, stdout) = rmcp::transport::stdio();
        self.serve(stdin, stdout).await
    }
}

impl ServerHandler for ToolSetMcp {
    fn get_info(&self) -> ServerConfig {
        let config = ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(self.info.clone());
        match &self.instructions {
            Some(text) => config.with_instructions(text.clone()),
            None => config,
        }
    }

    async fn list_tools(&self, _: Option<PaginatedRequestParams>, _: RequestContext<RoleServer>) -> Result<ListToolsResult, ErrorData> {
        let tools = self.tools.specs().into_iter().map(|spec| {
            let schema: JsonObject = serde_json::from_str(&spec.input_schema).unwrap_or_default();
            let hints = self.tools.get(&spec.name).map(|tool| tool.annotations()).unwrap_or_default();
            let mut tool = Tool::new(spec.name, spec.description, schema);
            // No hints means MCP's defaults (which assume destructive), not an explicit "safe".
            if hints != loa_contracts::tools::ToolAnnotations::default() {
                tool = tool.with_annotations(ToolAnnotations::new().read_only(hints.read_only).destructive(hints.destructive));
            }
            match hints.title {
                Some(title) => tool.with_title(title),
                None => tool,
            }
        });
        Ok(ListToolsResult::with_all_items(tools.collect()))
    }

    async fn call_tool(&self, request: CallToolRequestParams, context: RequestContext<RoleServer>) -> Result<CallToolResponse, ErrorData> {
        if self.tools.get(&request.name).is_none() {
            return Err(ErrorData::invalid_params(format!("unknown tool: {}", request.name), None));
        }
        let args = serde_json::Value::Object(request.arguments.unwrap_or_default());
        // Over HTTP the transport's auth layer may name the calling session in the request extensions.
        let parts = context.extensions.get::<http::request::Parts>();
        let ctx = parts.and_then(|parts| parts.extensions.get::<CallCtx>()).cloned().unwrap_or_default();
        // A cancelled request drops the call.
        let output = tokio::select! {
            output = self.tools.call(&ctx, &request.name, args) => output.unwrap_or_else(ToolOutput::error),
            () = context.ct.cancelled() => ToolOutput::error("tool call cancelled"),
        };
        Ok(to_result(output).into())
    }
}
