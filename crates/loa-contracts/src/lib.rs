//! Ports the app and its services talk through: harnesses, surfaces, tools, summaries.

pub mod availability;
pub mod harness;
pub mod surface;
pub mod tools;

use async_trait::async_trait;

/// Turns `input` into a short text following `instructions` (digests, changelogs, titles).
#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, instructions: &str, input: &str) -> Result<String, String>;
}
