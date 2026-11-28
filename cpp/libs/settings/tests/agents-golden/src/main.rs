//! Writes the agent and workflow goldens under `cpp/tests/golden/settings` from the
//! hand-written inputs there (`agent.md`, `agent-crlf.md`, `workflows/RULES.md`,
//! `workflows/config.yml`). Run: `cargo run` in this directory.

use std::path::{Path, PathBuf};

use loa_settings::models::agents::{self, AgentDefinition, ModelSpec};
use loa_settings::models::error::{Result, SettingsValidator};
use loa_settings::models::selector::Selector;
use loa_settings::models::workflows::{ManagedFields, RoutingNote, RuleBlock, Rules, Workflow, WorkflowFiles};

struct Accept;
#[async_trait::async_trait]
impl SettingsValidator for Accept {
    async fn check_config(&self, _: &str, _: &[(String, serde_json::Value)]) -> Result<()> {
        Ok(())
    }
    async fn check_models(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[tokio::main]
async fn main() {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../tests/golden/settings");
    let read = |p: &Path| std::fs::read_to_string(out.join(p)).unwrap();

    let mut def = agents::parse(&read(Path::new("agent.md"))).unwrap();
    def.set_tools(Some(s(&["read", "bash"]))).unwrap();
    def.set_thinking_level(Some("high")).unwrap();
    def.set_spawns(None).unwrap();
    def.set_model(Some(ModelSpec::One("@slow".into()))).unwrap();
    def.set_description("Plans: carefully").unwrap();
    def.set_body("\nNew body\n");
    std::fs::write(out.join("agent.edited.md"), def.to_text()).unwrap();

    let mut crlf = agents::parse(&read(Path::new("agent-crlf.md"))).unwrap();
    crlf.set_thinking_level(Some("minimal")).unwrap();
    crlf.set_spawns(Some(s(&["plan"]))).unwrap();
    std::fs::write(out.join("agent-crlf.edited.md"), crlf.to_text()).unwrap();

    let mut new = AgentDefinition::new("helper", "Does things: well").unwrap();
    new.set_tools(Some(s(&["read"]))).unwrap();
    new.set_model(Some(ModelSpec::Chain(s(&["@fast", "vendor/model"])))).unwrap();
    new.set_body("Help.\n");
    std::fs::write(out.join("agent.new.md"), new.to_text()).unwrap();

    let tmp = std::env::temp_dir().join(format!("loa-workflows-golden-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("RULES.md"), read(Path::new("workflows/RULES.md"))).unwrap();
    std::fs::write(tmp.join("config.yml"), read(Path::new("workflows/config.yml"))).unwrap();
    let files = WorkflowFiles::in_agent_dir(&tmp);

    let (_, snapshot) = files.load().unwrap();
    let research = Workflow {
        role: "RESEARCH".into(),
        models: vec![],
        rules: Rules::Managed {
            name: "Research".into(),
            allowed: s(&["scout"]),
            disallowed: s(&["bash"]),
            notes: "Cite sources.".into(),
            fields: ManagedFields {
                description: "Use for research.\n\nMore detail.".into(),
                routing: vec![RoutingNote { agent: "scout".into(), note: "Gather references.".into() }],
                steps: s(&["Read sources.", "Summarize findings."]),
                blocks: vec![RuleBlock { heading: "Guidance".into(), text: "First.\n\n#### Detail\nKeep context.\n".into() }],
            },
        },
    };
    files.save(&research, None, &snapshot, &Accept).await.unwrap();
    std::fs::copy(tmp.join("RULES.md"), out.join("workflows/RULES.created.md")).unwrap();
    std::fs::copy(tmp.join("config.yml"), out.join("workflows/config.created.yml")).unwrap();

    let (list, snapshot) = files.load().unwrap();
    let mut deep = list.into_iter().find(|w| w.role == "DEEP").unwrap();
    let Rules::HandWritten { text } = &deep.rules else { panic!("DEEP is not hand-written") };
    deep.rules = Rules::HandWritten { text: text.replace("dig deep", "dig deeper") };
    deep.role = "STUDY".into();
    files.save(&deep, Some("DEEP"), &snapshot, &Accept).await.unwrap();
    let (list, snapshot) = files.load().unwrap();
    let mut ext = list.into_iter().find(|w| w.role == "EXT").unwrap();
    ext.rules = Rules::External { text: "Edited body for `EXT`.\n".into() };
    ext.models = vec![Selector::model("vendor/other")];
    files.save(&ext, Some("EXT"), &snapshot, &Accept).await.unwrap();
    let (_, snapshot) = files.load().unwrap();
    files.delete("PLAN", &snapshot, &Accept).await.unwrap();
    std::fs::copy(tmp.join("RULES.md"), out.join("workflows/RULES.edited.md")).unwrap();
    std::fs::copy(tmp.join("config.yml"), out.join("workflows/config.edited.yml")).unwrap();
    std::fs::remove_dir_all(&tmp).unwrap();
}
