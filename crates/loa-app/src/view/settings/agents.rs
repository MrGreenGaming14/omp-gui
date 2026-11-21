//! omp agent definitions as [`loa_view::settings::agents`] views, and the form's fields back
//! onto a definition.

use loa_settings::models::agents::{AgentDefinition, AgentFile, AgentSource, FieldError, ModelSpec};
use loa_settings::models::error::Result;
use loa_view::settings::agents as view;

fn model_to_view(model: ModelSpec) -> view::ModelSpec {
    match model {
        ModelSpec::One(m) => view::ModelSpec::One(m),
        ModelSpec::Chain(c) => view::ModelSpec::Chain(c),
    }
}

fn model_from_view(model: view::ModelSpec) -> ModelSpec {
    match model {
        view::ModelSpec::One(m) => ModelSpec::One(m),
        view::ModelSpec::Chain(c) => ModelSpec::Chain(c),
    }
}

/// The parts of `def` the form edits.
pub fn fields_to_view(def: &AgentDefinition) -> view::AgentFields {
    view::AgentFields {
        name: def.name().to_owned(),
        description: def.description().to_owned(),
        model: def.model().map(model_to_view),
        thinking_level: def.thinking_level().map(str::to_owned),
        tools: def.tools(),
        spawns: def.spawns(),
        body: def.body().to_owned(),
    }
}

/// `base` with the form's `fields`. Only the keys that differ are rewritten, so an unedited
/// definition keeps its text.
pub fn fields_from_view(base: &AgentDefinition, fields: view::AgentFields) -> Result<AgentDefinition> {
    let before = fields_to_view(base);
    let mut def = base.clone();
    if fields.name != before.name {
        def.set_name(&fields.name)?;
    }
    if fields.description != before.description {
        def.set_description(&fields.description)?;
    }
    if fields.model != before.model {
        def.set_model(fields.model.map(model_from_view))?;
    }
    if fields.thinking_level != before.thinking_level {
        def.set_thinking_level(fields.thinking_level.as_deref())?;
    }
    if fields.tools != before.tools {
        def.set_tools(fields.tools)?;
    }
    if fields.spawns != before.spawns {
        def.set_spawns(fields.spawns)?;
    }
    if fields.body != before.body {
        def.set_body(fields.body);
    }
    Ok(def)
}

pub fn entry_to_view(file: &AgentFile) -> view::AgentEntry {
    let source = match file.source {
        AgentSource::User => view::AgentSource::User,
        AgentSource::Project => view::AgentSource::Project,
    };
    view::AgentEntry { path: file.path.clone(), source, enabled: file.enabled, fields: fields_to_view(&file.definition) }
}

pub fn field_error_to_view(error: FieldError) -> view::FieldError {
    view::FieldError { field: error.field, message: error.message }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_settings::models::agents::parse;

    const PLAN: &str = "---\nname: plan\n# why: planning only\ndescription: \"Software architect: plans\"\nmodel:\n  - \"@slow\"\n  - anthropic/claude-opus-5-5\nthinkingLevel: high\nspawns:\n  - explore\ntools: [read, grep]\noutput:\n  properties:\n    plan:\n      type: string\n---\nPlan first.\n";

    #[test]
    fn agent_fields_round_trip() {
        let def = parse(PLAN).unwrap();
        let fields = fields_to_view(&def);
        assert_eq!(fields.model, Some(view::ModelSpec::Chain(vec!["@slow".into(), "anthropic/claude-opus-5-5".into()])));
        assert_eq!(fields.tools, Some(vec!["read".into(), "grep".into()]));
        // Unedited fields leave the file byte identical.
        assert_eq!(fields_from_view(&def, fields.clone()).unwrap().to_text(), PLAN);

        let edited = view::AgentFields {
            name: "plan-copy".into(),
            description: "Plans".into(),
            model: Some(view::ModelSpec::One("@default".into())),
            thinking_level: None,
            tools: Some(vec!["read".into(), "bash".into()]),
            spawns: None,
            body: "New body\n".into(),
        };
        let back = fields_from_view(&def, edited.clone()).unwrap();
        assert_eq!(fields_to_view(&back), edited);
        let text = back.to_text();
        assert!(text.contains("# why: planning only") && text.contains("output:\n  properties:\n"), "unknown keys and comments survive: {text}");

        let blank = AgentDefinition::new("", "").unwrap();
        assert_eq!(fields_to_view(&fields_from_view(&blank, view::AgentFields::default()).unwrap()), view::AgentFields::default());
    }
}
