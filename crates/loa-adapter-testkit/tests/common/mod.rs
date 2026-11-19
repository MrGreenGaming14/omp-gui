//! Adapters with one deliberate bug each, built on `ScriptedAdapter`, so the
//! tests can show the testkit catches what a broken adapter would do.

#![allow(dead_code)]

use async_trait::async_trait;
use loa_adapter::{Adapter, Capabilities, HarnessSession, Result, SessionConfig};
use loa_adapter_testkit::ScriptedAdapter;
use loa_core::{CommitError, Dom, Draft, Patch, PatchSink};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

pub fn golden(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(name)
}

/// Rewrites every draft before committing it: simulates an adapter that
/// writes the wrong patches.
struct MappedSink {
    inner: Arc<dyn PatchSink>,
    map: fn(Draft) -> Draft,
}

impl PatchSink for MappedSink {
    fn commit_at(&self, draft: Draft, ts: i64) -> Result<Patch, CommitError> {
        self.inner.commit_at((self.map)(draft), ts)
    }
}

/// `ScriptedAdapter` with overridden capabilities, rewritten drafts or a
/// rewritten export.
pub struct Faulty {
    pub inner: ScriptedAdapter,
    pub caps: Option<Capabilities>,
    pub map: Option<fn(Draft) -> Draft>,
    pub export_edit: Option<fn(String) -> String>,
}

impl Faulty {
    pub fn new(inner: ScriptedAdapter) -> Self {
        Self { inner, caps: None, map: None, export_edit: None }
    }
}

#[async_trait]
impl Adapter for Faulty {
    fn harness(&self) -> &'static str {
        self.inner.harness()
    }

    fn capabilities(&self) -> Capabilities {
        self.caps.clone().unwrap_or_else(|| self.inner.capabilities())
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let sink: Arc<dyn PatchSink> = match self.map {
            Some(map) => Arc::new(MappedSink { inner: sink, map }),
            None => sink,
        };
        self.inner.start(config, sink).await
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        self.inner.resume(config, dom, sink).await
    }

    fn import(&self, native: &Path, sink: &dyn PatchSink) -> Result<()> {
        self.inner.import(native, sink)
    }

    fn export(&self, dom: &Dom, out: &Path) -> Result<()> {
        self.inner.export(dom, out)?;
        if let Some(edit) = self.export_edit {
            let text = std::fs::read_to_string(out)?;
            std::fs::write(out, edit(text))?;
        }
        Ok(())
    }
}
