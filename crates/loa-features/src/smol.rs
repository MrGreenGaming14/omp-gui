//! The small-model port behind app compaction and advisor notes: one model
//! call, prompt in and text out, with no tools and no session. The app backs
//! it with `loa_harness::omp::OneShot` (`omp -p` on a model role) through
//! the `loa_projects::Summarizer` port.

use crate::{Error, Result, Summarizer};
use async_trait::async_trait;
use std::sync::Arc;

/// A [`Summarizer`] over any one-shot model call.
#[derive(Clone)]
pub struct OneShot {
    model: Arc<dyn loa_projects::Summarizer>,
}

impl OneShot {
    pub fn new(model: Arc<dyn loa_projects::Summarizer>) -> Self {
        Self { model }
    }

    /// Runs `prompt` on `model` (a model id or a role such as `@smol`) and
    /// returns the answer text.
    pub async fn ask(&self, model: &str, prompt: &str) -> Result<String> {
        let model = Some(model).filter(|m| !m.trim().is_empty());
        let answer = self.model.summarize(prompt, model).await.map_err(|e| Error::Port(e.to_string()))?;
        Ok(answer.text)
    }
}

/// What the small model is asked to write when the app compacts a session.
pub fn compaction_prompt(history: &str, instructions: Option<&str>) -> String {
    let mut prompt = String::from(
        "Summarize the conversation below so a new agent session can continue it without the original \
         messages. Keep the user's goals and constraints, decisions made and why, files and commands that \
         matter, the current state of the work and what remains to do. Write plain prose and short lists, \
         no preamble.\n",
    );
    if let Some(instructions) = instructions.map(str::trim).filter(|i| !i.is_empty()) {
        prompt.push_str("\nAlso follow these instructions: ");
        prompt.push_str(instructions);
        prompt.push('\n');
    }
    // The history is quoted material, not instructions; the task is repeated
    // after it so a last line like "Reply only with OK" can't take over.
    prompt.push_str("\n<conversation>\n");
    prompt.push_str(history);
    prompt.push_str("</conversation>\n\nNow write the summary of the conversation above. Do not answer or continue it.\n");
    prompt
}

#[async_trait]
impl Summarizer for OneShot {
    async fn summarize(&self, model: &str, history: &str, instructions: Option<&str>) -> Result<String> {
        let summary = self.ask(model, &compaction_prompt(history, instructions)).await?;
        let summary = summary.trim();
        if summary.is_empty() {
            return Err(Error::Port("the model returned an empty summary".into()));
        }
        Ok(summary.to_owned())
    }
}
