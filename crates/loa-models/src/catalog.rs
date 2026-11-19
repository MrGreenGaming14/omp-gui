//! Available models grouped by provider, with the roles configured in omp.

use std::collections::BTreeMap;

use crate::config::{OmpConfig, Selector};
use crate::rpc::{ModelInfo, OmpRpc};
use crate::{Error, Result};

/// A selectable model and its configured role names.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    pub model: ModelInfo,
    pub roles: Vec<String>,
}

/// Models offered by one provider.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderModels {
    pub provider: String,
    pub models: Vec<ModelRow>,
}

/// Groups omp's available catalog using the concrete provider and model ids.
/// Role aliases and fuzzy selectors are left to omp to resolve, rather than
/// guessed here; only exact assignments are marked in the picker.
pub async fn available_models(rpc: &dyn OmpRpc, config: &OmpConfig) -> Result<Vec<ProviderModels>> {
    let roles = config.model_roles()?;
    let mut groups = BTreeMap::<String, Vec<ModelRow>>::new();
    for model in rpc.get_available_models().await? {
        let selector = model.selector();
        let assigned = roles.iter().filter(|role| role.candidates.iter().any(|candidate| {
            matches!(candidate, Selector::Model { model: id, .. } if id == &selector)
        })).map(|role| role.role.clone()).collect();
        groups.entry(model.provider.clone()).or_default().push(ModelRow { model, roles: assigned });
    }
    Ok(groups.into_iter().map(|(provider, mut models)| {
        models.sort_by(|a, b| a.model.name.cmp(&b.model.name).then_with(|| a.model.id.cmp(&b.model.id)));
        ProviderModels { provider, models }
    }).collect())
}

/// Switches the running omp session to an available concrete model.
pub async fn select_session_model(rpc: &dyn OmpRpc, provider: &str, model_id: &str) -> Result<ModelInfo> {
    Ok(rpc.set_model(provider, model_id).await?)
}

/// Assigns a concrete model to a role in omp's config and saves it atomically.
/// This does not change the current session's model; omp reloads the role for
/// subsequent selections and subagent launches.
pub fn assign_role(config: &mut OmpConfig, role: &str, model: &ModelInfo) -> Result<()> {
    if model.provider.is_empty() || model.id.is_empty() {
        return Err(Error::invalid("model", "provider and model id must be non-empty"));
    }
    config.set_model_role(role, &[Selector::model(model.selector())])?;
    config.save()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tokio::sync::mpsc::UnboundedSender;
    use crate::rpc::{LoginProvider, LoginUiRequest, RpcError, UiResponse};
    use std::sync::Mutex;

    struct Fake { models: Vec<ModelInfo>, selected: Mutex<Vec<(String,String)>> }
    #[async_trait]
    impl OmpRpc for Fake {
        async fn get_login_providers(&self) -> std::result::Result<Vec<LoginProvider>, RpcError> { Ok(vec![]) }
        async fn login(&self, _: &str, _: UnboundedSender<LoginUiRequest>) -> std::result::Result<String, RpcError> { Err(RpcError::Transport("not used".into())) }
        async fn respond_ui(&self, _: UiResponse) -> std::result::Result<(), RpcError> { Ok(()) }
        async fn get_available_models(&self) -> std::result::Result<Vec<ModelInfo>, RpcError> { Ok(self.models.clone()) }
        async fn set_model(&self, provider: &str, model_id: &str) -> std::result::Result<ModelInfo, RpcError> {
            self.selected.lock().unwrap().push((provider.into(), model_id.into()));
            self.models.iter().find(|m| m.provider == provider && m.id == model_id).cloned().ok_or(RpcError::Transport("unknown model".into()))
        }
    }
    fn model(provider: &str, id: &str) -> ModelInfo {
        serde_json::from_value(serde_json::json!({"provider":provider,"id":id,"name":id})).unwrap()
    }
    #[tokio::test]
    async fn picker_keeps_same_id_under_distinct_providers_and_switches_exact_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yml");
        let mut config = OmpConfig::load(&path).unwrap();
        let fake = Fake { models: vec![model("openai", "shared"), model("gateway", "shared")], selected: Mutex::new(vec![]) };
        assign_role(&mut config, "smol", &fake.models[1]).unwrap();
        let groups = available_models(&fake, &config).await.unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].models[0].roles, ["smol"]);
        assert!(groups[1].models[0].roles.is_empty());
        select_session_model(&fake, "openai", "shared").await.unwrap();
        assert_eq!(*fake.selected.lock().unwrap(), [("openai".into(), "shared".into())]);
        assert!(std::fs::read_to_string(path).unwrap().contains("gateway/shared"));
    }
}
