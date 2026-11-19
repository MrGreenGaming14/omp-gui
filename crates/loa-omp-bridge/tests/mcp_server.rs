//! The MCP server shell, driven by a real rmcp client against an in-memory executor.

use std::sync::{Arc, Mutex};

use loa_omp_bridge::{Error, OutputBlock, ToolExecutor, ToolOutput, ToolSpec, serve};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ContentBlock};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

struct MemoryExecutor {
    tools: Vec<ToolSpec>,
    calls: Mutex<Vec<(String, Map<String, Value>)>>,
}

#[async_trait::async_trait]
impl ToolExecutor for MemoryExecutor {
    fn tools(&self) -> &[ToolSpec] {
        &self.tools
    }

    async fn call(
        &self,
        name: &str,
        arguments: Map<String, Value>,
        _cancel: CancellationToken,
    ) -> loa_omp_bridge::Result<ToolOutput> {
        self.calls.lock().unwrap().push((name.to_owned(), arguments.clone()));
        match name {
            "read" => Ok(ToolOutput {
                content: vec![
                    OutputBlock::Text(format!("[{}#AB12]\n1:hello", arguments["path"].as_str().unwrap())),
                    OutputBlock::Image { data: "aGk=".into(), mime_type: "image/png".into() },
                ],
                is_error: false,
            }),
            "edit" => Ok(ToolOutput {
                content: vec![OutputBlock::Text("stale tag AB12".into())],
                is_error: true,
            }),
            _ => Err(Error::NotRun { tool: name.to_owned(), reason: "turn error".into() }),
        }
    }
}

fn spec(name: &str, schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: format!("{name} tool"),
        input_schema: schema.as_object().unwrap().clone(),
    }
}

#[tokio::test]
async fn clients_see_executor_schemas_and_results() {
    let read_schema = json!({
        "type": "object",
        "properties": {"path": {"type": "string", "description": "File or URL"}},
        "required": ["path"],
    });
    let executor = Arc::new(MemoryExecutor {
        tools: vec![
            spec("read", read_schema.clone()),
            spec("edit", json!({"type": "object", "properties": {"input": {"type": "string"}}})),
            spec("lsp", json!({"type": "object"})),
        ],
        calls: Mutex::new(Vec::new()),
    });
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let (reader, writer) = tokio::io::split(server_io);
    let server = tokio::spawn(serve(Arc::clone(&executor), reader, writer));
    let client = ().serve(client_io).await.unwrap();

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(names, ["read", "edit", "lsp"]);
    assert_eq!(Value::Object((*tools[0].input_schema).clone()), read_schema);

    let mut arguments = Map::new();
    arguments.insert("path".into(), json!("src/main.rs"));
    let read = client
        .call_tool(CallToolRequestParams::new("read").with_arguments(arguments.clone()))
        .await
        .unwrap();
    assert_eq!(read.is_error, Some(false));
    match (&read.content[0], &read.content[1]) {
        (ContentBlock::Text(text), ContentBlock::Image(image)) => {
            assert_eq!(text.text, "[src/main.rs#AB12]\n1:hello");
            assert_eq!(image.mime_type, "image/png");
        }
        other => panic!("unexpected content {other:?}"),
    }
    assert_eq!(executor.calls.lock().unwrap()[0], ("read".to_owned(), arguments));

    let edit = client
        .call_tool(CallToolRequestParams::new("edit").with_arguments(Map::new()))
        .await
        .unwrap();
    assert_eq!(edit.is_error, Some(true), "a tool's own failure reaches the client as isError");

    let failed = client.call_tool(CallToolRequestParams::new("lsp")).await.unwrap();
    assert_eq!(failed.is_error, Some(true));
    let ContentBlock::Text(text) = &failed.content[0] else { panic!("no error text") };
    assert!(text.text.contains("turn error"), "{}", text.text);

    assert!(client.call_tool(CallToolRequestParams::new("bash")).await.is_err());
    assert_eq!(executor.calls.lock().unwrap().len(), 3, "unknown tools never reach the executor");

    client.cancel().await.unwrap();
    server.await.unwrap().unwrap();
}
