//! Projections between stored workflows and their editable views.

use loa_settings::models::{error::Result, selector::Selector, workflows::{Rules, Workflow}};
use loa_view::settings::workflows as view;

pub fn to_view(workflow: &Workflow) -> view::Workflow {
    view::Workflow {
        role: workflow.role.clone(),
        models: workflow.models.iter().map(ToString::to_string).collect(),
        rules: match &workflow.rules {
            Rules::Managed { name, allowed, disallowed, notes } => view::Rules::Managed { name: name.clone(), allowed: allowed.clone(), disallowed: disallowed.clone(), notes: notes.clone() },
            Rules::HandWritten { text } => view::Rules::HandWritten { text: text.clone() },
            Rules::None => view::Rules::None,
        },
    }
}

pub fn from_view(workflow: view::Workflow) -> Result<Workflow> {
    Ok(Workflow {
        role: workflow.role,
        models: workflow.models.iter().map(|model| Selector::parse(model)).collect::<Result<_>>()?,
        rules: match workflow.rules {
            view::Rules::Managed { name, allowed, disallowed, notes } => Rules::Managed { name, allowed, disallowed, notes },
            view::Rules::HandWritten { text } => Rules::HandWritten { text },
            view::Rules::None => Rules::None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_round_trips_each_rules_kind() {
        for rules in [Rules::None, Rules::HandWritten { text: "## Mode\nText\n".into() }, Rules::Managed { name: "Review".into(), allowed: vec!["reviewer".into()], disallowed: vec!["writer".into()], notes: "Notes".into() }] {
            let workflow = Workflow { role: "REVIEW".into(), models: vec![Selector::parse("provider/model:high").unwrap(), Selector::parse("@default").unwrap()], rules };
            assert_eq!(from_view(to_view(&workflow)).unwrap(), workflow);
        }
    }
}
