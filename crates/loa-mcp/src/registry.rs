//! Adding, editing and removing servers, persisted through a port.

use std::sync::Mutex;

use async_trait::async_trait;
use loa_util::LockExt;
use rmcp::model::Tool;

use crate::import::{self, Found, ImportReport};
use crate::spec::ServerSpec;
use crate::{Error, Manager, Result};

pub type StoreError = Box<dyn std::error::Error + Send + Sync>;

/// Where the server list lives; the central config implements it.
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

/// The list in memory, for throwaway profiles and tests.
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<Vec<ServerSpec>>);

impl MemoryStore {
    pub fn new(servers: Vec<ServerSpec>) -> Self {
        Self(Mutex::new(servers))
    }

    pub fn servers(&self) -> Vec<ServerSpec> {
        self.0.locked().clone()
    }
}

#[async_trait]
impl ServerStore for MemoryStore {
    async fn load(&self) -> Result<Vec<ServerSpec>, StoreError> {
        Ok(self.servers())
    }

    async fn save(&self, servers: &[ServerSpec]) -> Result<(), StoreError> {
        *self.0.locked() = servers.to_vec();
        Ok(())
    }
}

/// The saved server list kept in step with the running [`Manager`].
pub struct Registry<S> {
    store: S,
    manager: Manager,
    /// Everything stored, including entries the manager can't run, so saving never drops them.
    specs: tokio::sync::Mutex<Vec<ServerSpec>>,
}

fn position(specs: &[ServerSpec], name: &str) -> Result<usize> {
    specs.iter().position(|other| other.name == name).ok_or_else(|| Error::UnknownServer(name.to_owned()))
}

fn free(specs: &[ServerSpec], name: &str) -> Result<()> {
    if specs.iter().any(|other| other.name == name) { Err(Error::NameTaken(name.to_owned())) } else { Ok(()) }
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

    /// Applies `change` to a copy of the list and saves it; the list only
    /// changes when both succeed, so a failed save rolls back.
    pub async fn commit<T>(&self, change: impl FnOnce(&mut Vec<ServerSpec>) -> Result<T>) -> Result<T> {
        let mut specs = self.specs.lock().await;
        let mut next = specs.clone();
        let out = change(&mut next)?;
        self.store.save(&next).await.map_err(Error::Store)?;
        *specs = next;
        Ok(out)
    }

    /// Saves and starts a new server once it answers; returns its tools.
    pub async fn add(&self, spec: ServerSpec) -> Result<Vec<Tool>> {
        spec.validate()?;
        free(&self.specs.lock().await, &spec.name)?;
        let tools = self.manager.probe(&spec).await?;
        self.commit(|specs| free(specs, &spec.name).map(|()| specs.push(spec.clone()))).await?;
        self.manager.add(spec)?;
        Ok(tools)
    }

    /// Replaces the server `name` with `spec` (which may rename it), after the same check as [`Registry::add`].
    pub async fn edit(&self, name: &str, spec: ServerSpec) -> Result<Vec<Tool>> {
        spec.validate()?;
        let tools = self.manager.probe(&spec).await?;
        self.commit(|specs| {
            let index = position(specs, name)?;
            if spec.name != name {
                free(specs, &spec.name)?;
            }
            specs[index] = spec.clone();
            Ok(())
        })
        .await?;
        self.manager.replace(name, spec).await?;
        Ok(tools)
    }

    /// Stops the server and deletes it from the store.
    pub async fn remove(&self, name: &str) -> Result<()> {
        self.commit(|specs| position(specs, name).map(|index| drop(specs.remove(index)))).await?;
        match self.manager.remove(name).await {
            Ok(_) | Err(Error::UnknownServer(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Turns a server on or off and saves the choice.
    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        self.commit(|specs| position(specs, name).map(|index| specs[index].enabled = enabled)).await?;
        if enabled { self.manager.start(name) } else { self.manager.stop(name).await }
    }

    /// Imports entries an [`import::scan`] found and the user picked, saving and starting the new ones.
    pub async fn import_found(&self, found: Vec<Found>) -> Result<ImportReport> {
        let report = self.commit(|specs| {
            let report = import::plan_found(specs, found);
            specs.extend(report.imported().cloned());
            Ok(report)
        });
        let report = report.await?;
        report.imported().try_for_each(|spec| self.manager.add(spec.clone()))?;
        Ok(report)
    }
}
