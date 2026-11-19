//! Adding, editing and removing servers, persisted through a port.

use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use rmcp::model::Tool;

use crate::import::{self, ConfigFile, Found, ImportReport};
use crate::spec::ServerSpec;
use crate::{Error, Manager, Result};

/// Error a [`ServerStore`] implementation reports.
pub type StoreError = Box<dyn std::error::Error + Send + Sync>;

/// Where the server list lives. The central config implements it; this
/// crate never touches the config file itself.
#[async_trait]
pub trait ServerStore: Send + Sync {
    async fn load(&self) -> Result<Vec<ServerSpec>, StoreError>;
    /// Replaces the stored list.
    async fn save(&self, servers: &[ServerSpec]) -> Result<(), StoreError>;
}

#[async_trait]
impl<T: ServerStore + ?Sized> ServerStore for std::sync::Arc<T> {
    async fn load(&self) -> Result<Vec<ServerSpec>, StoreError> {
        (**self).load().await
    }

    async fn save(&self, servers: &[ServerSpec]) -> Result<(), StoreError> {
        (**self).save(servers).await
    }
}

/// A store that keeps the list in memory, for throwaway profiles and tests.
#[derive(Debug, Default)]
pub struct MemoryStore {
    servers: Mutex<Vec<ServerSpec>>,
}

impl MemoryStore {
    pub fn new(servers: Vec<ServerSpec>) -> Self {
        Self { servers: Mutex::new(servers) }
    }

    pub fn servers(&self) -> Vec<ServerSpec> {
        self.servers.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[async_trait]
impl ServerStore for MemoryStore {
    async fn load(&self) -> Result<Vec<ServerSpec>, StoreError> {
        Ok(self.servers())
    }

    async fn save(&self, servers: &[ServerSpec]) -> Result<(), StoreError> {
        *self.servers.lock().unwrap_or_else(PoisonError::into_inner) = servers.to_vec();
        Ok(())
    }
}

/// The saved server list kept in step with the running [`Manager`].
pub struct Registry<S> {
    store: S,
    manager: Manager,
    /// Everything stored, including entries the manager can't run (an
    /// invalid hand edit), so saving never drops them.
    specs: tokio::sync::Mutex<Vec<ServerSpec>>,
}

impl<S: ServerStore> Registry<S> {
    /// Loads the stored servers and starts the enabled ones.
    pub async fn open(store: S, manager: Manager) -> Result<Self> {
        let specs = store.load().await.map_err(Error::Store)?;
        for spec in &specs {
            if let Err(e) = manager.add(spec.clone()) {
                tracing::warn!(server = spec.name, "not starting a stored server: {e}");
            }
        }
        Ok(Self { store, manager, specs: tokio::sync::Mutex::new(specs) })
    }

    pub fn manager(&self) -> &Manager {
        &self.manager
    }

    pub async fn specs(&self) -> Vec<ServerSpec> {
        self.specs.lock().await.clone()
    }

    /// Connects to the new server and lists its tools; only a server that
    /// answers is saved and started. Returns the tools it offers.
    pub async fn add(&self, spec: ServerSpec) -> Result<Vec<Tool>> {
        spec.validate()?;
        let mut specs = self.specs.lock().await;
        if specs.iter().any(|other| other.name == spec.name) {
            return Err(Error::NameTaken(spec.name));
        }
        let tools = self.manager.probe(&spec).await?;
        specs.push(spec.clone());
        if let Err(e) = self.store.save(&specs).await {
            specs.pop();
            return Err(Error::Store(e));
        }
        self.manager.add(spec)?;
        Ok(tools)
    }

    /// Replaces the server `name` with `spec` (which may rename it), after
    /// the same check as [`Registry::add`].
    pub async fn edit(&self, name: &str, spec: ServerSpec) -> Result<Vec<Tool>> {
        spec.validate()?;
        let mut specs = self.specs.lock().await;
        let index = specs.iter().position(|other| other.name == name).ok_or_else(|| Error::UnknownServer(name.to_owned()))?;
        if spec.name != name && specs.iter().any(|other| other.name == spec.name) {
            return Err(Error::NameTaken(spec.name));
        }
        let tools = self.manager.probe(&spec).await?;
        let old = std::mem::replace(&mut specs[index], spec.clone());
        if let Err(e) = self.store.save(&specs).await {
            specs[index] = old;
            return Err(Error::Store(e));
        }
        match self.manager.replace(name, spec.clone()).await {
            Err(Error::UnknownServer(_)) => self.manager.add(spec)?,
            other => other?,
        }
        Ok(tools)
    }

    /// Stops the server and deletes it from the store.
    pub async fn remove(&self, name: &str) -> Result<()> {
        let mut specs = self.specs.lock().await;
        let index = specs.iter().position(|other| other.name == name).ok_or_else(|| Error::UnknownServer(name.to_owned()))?;
        let removed = specs.remove(index);
        if let Err(e) = self.store.save(&specs).await {
            specs.insert(index, removed);
            return Err(Error::Store(e));
        }
        match self.manager.remove(name).await {
            Ok(_) | Err(Error::UnknownServer(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Turns a server on or off and saves the choice.
    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        let mut specs = self.specs.lock().await;
        let spec = specs.iter_mut().find(|other| other.name == name).ok_or_else(|| Error::UnknownServer(name.to_owned()))?;
        spec.enabled = enabled;
        if let Err(e) = self.store.save(&specs).await {
            if let Some(spec) = specs.iter_mut().find(|other| other.name == name) {
                spec.enabled = !enabled;
            }
            return Err(Error::Store(e));
        }
        if enabled { self.manager.start(name) } else { self.manager.stop(name).await }
    }

    /// Imports the servers found in `files`, saves and starts the new ones,
    /// and reports what happened to every entry.
    pub async fn import(&self, files: &[ConfigFile]) -> Result<ImportReport> {
        let mut specs = self.specs.lock().await;
        let report = import::plan(&specs, files);
        self.register_imported(&mut specs, report).await
    }

    /// Imports entries a [`import::scan`] found and the user picked.
    pub async fn import_found(&self, found: Vec<Found>) -> Result<ImportReport> {
        let mut specs = self.specs.lock().await;
        let report = import::plan_found(&specs, found);
        self.register_imported(&mut specs, report).await
    }

    async fn register_imported(&self, specs: &mut Vec<ServerSpec>, report: ImportReport) -> Result<ImportReport> {
        let before = specs.len();
        specs.extend(report.imported().cloned());
        if specs.len() == before {
            return Ok(report);
        }
        if let Err(e) = self.store.save(specs).await {
            specs.truncate(before);
            return Err(Error::Store(e));
        }
        for spec in report.imported() {
            self.manager.add(spec.clone())?;
        }
        Ok(report)
    }
}
