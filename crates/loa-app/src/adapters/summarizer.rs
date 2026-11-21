//! Digest summaries by omp's one-shot model calls.

use std::sync::Arc;

use loa_projects::{Summarizer, Summarizers};

/// `omp -p` on the given model, `@smol` by default.
pub struct OmpSummarizers(pub loa_harness::omp::OneShot);

impl Summarizers for OmpSummarizers {
    fn on(&self, model: Option<&str>) -> (Arc<dyn Summarizer>, Option<String>) {
        let model = self.0.pick(model).to_owned();
        let name = model.trim_start_matches('@').to_owned();
        (Arc::new(self.0.clone().with_model(model)), Some(name))
    }
}
