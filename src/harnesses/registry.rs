//! Named provider selection, supervised calls and result-to-request correlation.
use super::config::{canonicalize_workspace_roots, validate_identifier, validate_registry_config};
use super::output_validation::{
    normalize_project, normalize_project_list_result, valid_project_id,
    validate_client_project_grouping,
};
use super::validation::{
    validate_project_create_request, validate_project_list_request, validate_run_request,
};
use super::{
    ConversationUncertainty, HarnessAdapterFactory, HarnessDefinition, HarnessError,
    HarnessProject, HarnessProjectCreateRequest, HarnessProjectListRequest, HarnessProvider,
    HarnessRegistryConfig, HarnessRunRequest, HarnessRunResult, ProjectCreationUncertainty,
};
use crate::runtime::operation::{CallScope, DispatchError};
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct HarnessRegistryBuilder {
    factories: BTreeMap<String, Arc<dyn HarnessAdapterFactory>>,
}

impl HarnessRegistryBuilder {
    pub fn new() -> Self {
        Self {
            factories: BTreeMap::new(),
        }
    }

    pub fn register(
        &mut self,
        factory: Arc<dyn HarnessAdapterFactory>,
    ) -> Result<(), HarnessError> {
        let id = factory.adapter_id().to_owned();
        validate_identifier("adapter", &id)?;
        if self.factories.contains_key(&id) {
            return Err(HarnessError::DuplicateAdapter(id));
        }
        self.factories.insert(id, factory);
        Ok(())
    }

    pub fn build(self, mut config: HarnessRegistryConfig) -> Result<HarnessRegistry, HarnessError> {
        validate_registry_config(&config)?;
        let mut entries = BTreeMap::new();
        for definition in &mut config.harnesses {
            let factory = self
                .factories
                .get(&definition.adapter)
                .ok_or_else(|| HarnessError::UnsupportedAdapter(definition.adapter.clone()))?;
            canonicalize_workspace_roots(definition)?;
            let provider = factory.build(definition.clone())?;
            entries.insert(
                definition.id.clone(),
                RegistryEntry {
                    definition: definition.clone(),
                    provider,
                },
            );
        }
        Ok(HarnessRegistry {
            default_harness: config.default_harness,
            entries: Arc::new(entries),
            calls: CallScope::default(),
        })
    }
}

impl Default for HarnessRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

struct RegistryEntry {
    definition: HarnessDefinition,
    provider: Arc<dyn HarnessProvider>,
}

/// A bounded set of explicitly named Harness instances. Routing never falls
/// back to a different instance after a requested provider fails.
#[derive(Clone)]
pub struct HarnessRegistry {
    default_harness: Option<String>,
    entries: Arc<BTreeMap<String, RegistryEntry>>,
    calls: CallScope,
}

impl HarnessRegistry {
    pub fn definitions(&self) -> Vec<HarnessDefinition> {
        self.entries
            .values()
            .map(|entry| entry.definition.clone())
            .collect()
    }

    pub fn default_harness(&self) -> Option<&str> {
        self.default_harness.as_deref()
    }

    pub async fn list_projects(
        &self,
        harness_id: Option<&str>,
        request: HarnessProjectListRequest,
    ) -> Result<Vec<HarnessProject>, HarnessError> {
        let registry = self.clone();
        let harness_id = harness_id.map(str::to_owned);
        self.calls
            .run(request.cancellation.clone(), async move {
                registry
                    .list_projects_inner(harness_id.as_deref(), request)
                    .await
            })
            .await
            .map_err(dispatch_error)?
    }

    async fn list_projects_inner(
        &self,
        harness_id: Option<&str>,
        request: HarnessProjectListRequest,
    ) -> Result<Vec<HarnessProject>, HarnessError> {
        let entry = self.entry(harness_id)?;
        let request = validate_project_list_request(&entry.definition, request)?;
        let mut projects = entry.provider.list_projects(request).await?;
        normalize_project_list_result(&entry.definition, &mut projects)?;
        Ok(projects)
    }

    pub async fn create_project(
        &self,
        harness_id: Option<&str>,
        request: HarnessProjectCreateRequest,
    ) -> Result<HarnessProject, HarnessError> {
        let registry = self.clone();
        let uncertainty = ProjectCreationUncertainty {
            harness: harness_id
                .or(self.default_harness())
                .unwrap_or("selected")
                .into(),
            name: request.name.clone(),
            root_directory: request.root_directory.clone(),
            idempotency_key: request.idempotency_key.clone(),
            project_id: None,
            message:
                "service supervisor terminated after dispatch; inspect the original project request"
                    .into(),
        };
        let harness_id = harness_id.map(str::to_owned);
        self.calls
            .run(request.cancellation.clone(), async move {
                registry
                    .create_project_inner(harness_id.as_deref(), request)
                    .await
            })
            .await
            .map_err(|error| match error {
                DispatchError::Supervisor(_) => {
                    HarnessError::ProjectCreationOutcomeUnknown(Box::new(uncertainty))
                }
                error => dispatch_error(error),
            })?
    }

    async fn create_project_inner(
        &self,
        harness_id: Option<&str>,
        request: HarnessProjectCreateRequest,
    ) -> Result<HarnessProject, HarnessError> {
        let entry = self.entry(harness_id)?;
        let request = validate_project_create_request(&entry.definition, request)?;
        let remote = request.remote_workspace.is_some();
        let expected_name = request.name.clone();
        let expected_root = request.root_directory.clone();
        let idempotency_key = request.idempotency_key.clone();
        let mut project = entry.provider.create_project(request).await?;
        let observed_project_id = valid_project_id(&project.id).then(|| project.id.clone());
        if let Err(error) = normalize_project(&entry.definition, &mut project) {
            return Err(HarnessError::ProjectCreationOutcomeUnknown(Box::new(
                ProjectCreationUncertainty {
                    harness: entry.definition.id.clone(),
                    name: expected_name,
                    root_directory: expected_root,
                    idempotency_key,
                    project_id: observed_project_id,
                    message: error.to_string(),
                },
            )));
        }
        let returned_root = match project.roots.as_slice() {
            [root] => root.clone(),
            _ => {
                return Err(HarnessError::ProjectCreationOutcomeUnknown(Box::new(
                    ProjectCreationUncertainty {
                        harness: entry.definition.id.clone(),
                        name: expected_name,
                        root_directory: expected_root,
                        idempotency_key,
                        project_id: Some(project.id.clone()),
                        message: "created project did not return exactly one root".to_owned(),
                    },
                )));
            }
        };
        if project.name != expected_name || (!remote && returned_root != expected_root) {
            return Err(HarnessError::ProjectCreationOutcomeUnknown(Box::new(
                ProjectCreationUncertainty {
                    harness: entry.definition.id.clone(),
                    name: expected_name,
                    root_directory: expected_root,
                    idempotency_key,
                    project_id: Some(project.id.clone()),
                    message: "created project does not match the requested name and root"
                        .to_owned(),
                },
            )));
        }
        Ok(project)
    }

    pub async fn run(
        &self,
        harness_id: Option<&str>,
        request: HarnessRunRequest,
    ) -> Result<HarnessRunResult, HarnessError> {
        let registry = self.clone();
        let uncertainty = ConversationUncertainty {
            harness: harness_id
                .or(self.default_harness())
                .unwrap_or("selected")
                .into(),
            thread_id: None,
            project_directory: request.project_directory.clone(),
            visibility: request.visibility,
            native_project_id: request.placement.project_id().map(str::to_owned),
            message:
                "service supervisor terminated after dispatch; inspect the original conversation"
                    .into(),
        };
        let harness_id = harness_id.map(str::to_owned);
        self.calls
            .run(request.cancellation.clone(), async move {
                registry.run_inner(harness_id.as_deref(), request).await
            })
            .await
            .map_err(|error| match error {
                DispatchError::Supervisor(_) => {
                    HarnessError::ConversationOutcomeUnknown(Box::new(uncertainty))
                }
                error => dispatch_error(error),
            })?
    }

    async fn run_inner(
        &self,
        harness_id: Option<&str>,
        request: HarnessRunRequest,
    ) -> Result<HarnessRunResult, HarnessError> {
        let entry = self.entry(harness_id)?;
        let validated = validate_run_request(&entry.definition, request)?;
        let expected_directory = validated.project_directory.clone();
        let expected_visibility = validated.visibility;
        let expected_project_id = validated.placement.project_id().map(str::to_owned);
        let result = entry.provider.run(validated.into_request()).await?;
        if result.harness_id != entry.definition.id
            || result.adapter != entry.definition.adapter
            || result.address != entry.definition.address
            || result.project_directory != expected_directory
            || result.visibility != expected_visibility
            || result.native_project_id != expected_project_id
        {
            return Err(HarnessError::ConversationOutcomeUnknown(Box::new(
                ConversationUncertainty {
                    harness: entry.definition.id.clone(),
                    thread_id: valid_project_id(&result.thread_id)
                        .then(|| result.thread_id.clone()),
                    project_directory: expected_directory,
                    visibility: expected_visibility,
                    native_project_id: expected_project_id,
                    message: "provider result does not match the selected Harness instance"
                        .to_owned(),
                },
            )));
        }
        if let Err(error) = validate_client_project_grouping(
            &entry.definition,
            expected_visibility,
            &result.client_project_grouping,
        ) {
            return Err(HarnessError::ConversationOutcomeUnknown(Box::new(
                ConversationUncertainty {
                    harness: entry.definition.id.clone(),
                    thread_id: valid_project_id(&result.thread_id)
                        .then(|| result.thread_id.clone()),
                    project_directory: expected_directory,
                    visibility: expected_visibility,
                    native_project_id: expected_project_id,
                    message: error.to_string(),
                },
            )));
        }
        Ok(result)
    }

    pub async fn shutdown(&self) -> Result<(), HarnessError> {
        self.calls.shutdown().await.map_err(dispatch_error)
    }

    pub fn begin_shutdown(&self) -> Result<(), HarnessError> {
        self.calls.close().map_err(dispatch_error)
    }

    fn entry(&self, harness_id: Option<&str>) -> Result<&RegistryEntry, HarnessError> {
        let id = match harness_id {
            Some(id) => id,
            None => self
                .default_harness
                .as_deref()
                .ok_or(HarnessError::NoDefaultHarness)?,
        };
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| HarnessError::UnknownHarness(id.to_owned()))?;
        if !entry.definition.enabled {
            return Err(HarnessError::HarnessDisabled(id.to_owned()));
        }
        Ok(entry)
    }
}

fn dispatch_error(error: DispatchError) -> HarnessError {
    HarnessError::Unavailable {
        harness: "registry".into(),
        message: error.to_string(),
    }
}
