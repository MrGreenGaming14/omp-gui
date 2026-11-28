//! Golden files for the C++ port, written by `cargo test -p loa-contracts --test
//! golden -- --ignored` into `$LOA_GOLDEN_OUT`: the tool input schemas `DynTool::spec`
//! emits for argument structs covering every shape the app's tools use.

#![allow(dead_code)]

use async_trait::async_trait;
use loa_contracts::tools::{CallCtx, DynTool, Tool, ToolOutput};
use std::fs;
use std::path::PathBuf;

/// The `Args` of the `shout` tool in `loa_contracts::tools` tests.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct Args {
    word: String,
}

/// How a text query matches.
#[derive(Copy, Clone, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TextMode {
    /// The whole query as one substring.
    #[default]
    Phrase,
    /// Every whitespace separated word, anywhere.
    AllWords,
}

#[derive(Copy, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum ModifierKey {
    Shift,
    Ctrl,
}

fn one() -> u32 { 1 }

/// Search arguments.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    /// Text to find, case insensitive.
    query: Option<String>,
    /// phrase: the whole query as written. all_words: every word, anywhere.
    #[serde(default)]
    mode: TextMode,
    /// Node id from a search hit.
    #[schemars(range(min = 1))]
    node: Option<u64>,
    /// next_offset of the previous page.
    #[serde(default)]
    offset: usize,
    #[schemars(range(min = 1, max = 32000))]
    max_bytes: Option<usize>,
    /// Pixels from the left edge
    x: f32,
    /// 2 for a double click
    #[serde(default = "one")]
    clicks: u32,
    #[serde(default)]
    modifiers: Vec<ModifierKey>,
    #[serde(default)]
    submit: bool,
    network: Option<bool>,
    cpus: Option<u32>,
    mode2: Option<TextMode>,
}

/// Structured data the agent tools ask a document surface for.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DataQuery {
    /// Format, MIME type, page count and page sizes.
    Info,
    /// Page text.
    Text {
        #[serde(default)]
        page: Option<usize>,
    },
    /// Cells of `range` in `sheet`.
    Cells {
        #[serde(default)]
        sheet: Option<String>,
        range: String,
    },
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DataArgs {
    /// The surface, e.g. surface://docs/1
    uri: String,
    query: DataQuery,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

macro_rules! tool {
    ($name:ident, $args:ty, $wire:literal) => {
        struct $name;
        #[async_trait]
        impl Tool for $name {
            type Args = $args;
            const NAME: &'static str = $wire;
            fn description(&self) -> &str { "" }
            async fn run(&self, _: &CallCtx, _: $args) -> Result<ToolOutput, String> { Ok(ToolOutput::default()) }
        }
    };
}

tool!(Shout, Args, "args");
tool!(Search, SearchArgs, "search_args");
tool!(Data, DataArgs, "data_args");
tool!(Nothing, NoArgs, "no_args");

#[test]
#[ignore = "writes golden files; run with LOA_GOLDEN_OUT set"]
fn write_golden_tool_schemas() {
    let dir = PathBuf::from(std::env::var("LOA_GOLDEN_OUT").expect("LOA_GOLDEN_OUT")).join("contracts");
    fs::create_dir_all(&dir).unwrap();
    let tools: [&dyn DynTool; 4] = [&Shout, &Search, &Data, &Nothing];
    for tool in tools {
        let spec = tool.spec();
        fs::write(dir.join(format!("{}.schema.json", spec.name)), spec.input_schema).unwrap();
    }
}
