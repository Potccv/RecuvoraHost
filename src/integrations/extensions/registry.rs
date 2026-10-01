//! Extension registration, service lifetime and shared dispatch ownership.
use super::validation::validate_metadata;
use super::{
    ContractDeclaration, ExtensionClient, ExtensionError, ExtensionKind, ExtensionMetadata,
};
use super::{ExtensionDefinition, ExtensionsConfig, NodeSettings};
use crate::protocol;
use crate::runtime::operation::{CallScope, DispatchError};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Semaphore;

pub(super) struct Entry {
    pub(super) definition: ExtensionDefinition,
    pub(super) client: ExtensionClient,
    pub(super) metadata: ExtensionMetadata,
    pub(super) ordinary: Arc<Semaphore>,
    pub(super) monitoring_view: Arc<Semaphore>,
    pub(super) approval: Arc<Semaphore>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExtensionStatus {
    pub id: String,
    pub kind: ExtensionKind,
    pub available: bool,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct ExtensionRegistry {
    entries: BTreeMap<String, Arc<Entry>>,
    definitions: Vec<ExtensionDefinition>,
    statuses: Vec<ExtensionStatus>,
    contract_owners: BTreeMap<(String, u32), String>,
    pub(super) settings: NodeSettings,
    pub(super) calls: CallScope,
}

pub(super) fn dispatch_error(error: DispatchError) -> ExtensionError {
    match error {
        DispatchError::Supervisor(message) => ExtensionError::Unknown {
            // No fabricated wire identifier: the supervisor may have exited
            // before obtaining the remote call ID. Original caller context is
            // retained by the domain request and must have its result checked there.
            call_id: String::new(),
            message: format!(
                "supervisor exited after dispatch; remote call ID unavailable: {message}"
            ),
        },
        error => ExtensionError::Unavailable(error.to_string()),
    }
}

impl ExtensionRegistry {
    pub async fn connect(config: ExtensionsConfig) -> Result<Self, ExtensionError> {
        Self::connect_with_settings(config, NodeSettings::default()).await
    }

    pub async fn connect_with_settings(
        config: ExtensionsConfig,
        settings: NodeSettings,
    ) -> Result<Self, ExtensionError> {
        config.validate()?;
        settings.validate()?;
        let original_definitions = config.extensions.clone();
        let mut statuses = Vec::new();
        let mut entries = BTreeMap::new();
        let mut contracts = BTreeMap::<(String, u32), ContractDeclaration>::new();
        let mut contract_owners = BTreeMap::<(String, u32), String>::new();
        // Consumers own new contracts. Nodes can only implement a contract whose
        // trusted plugin has already registered an identical declaration.
        let mut definitions = config
            .extensions
            .into_iter()
            .filter(|d| d.enabled)
            .collect::<Vec<_>>();
        definitions.sort_by_key(|d| {
            if d.kind == ExtensionKind::Plugin {
                0
            } else {
                1
            }
        });
        for definition in definitions {
            let client = ExtensionClient {
                id: definition.id.clone(),
                kind: definition.kind,
                endpoint: definition.endpoint.clone(),
            };
            let registration: Result<ExtensionMetadata, ExtensionError> = async {
                let metadata = client.probe_with_settings(&settings.protocol).await?;
                validate_metadata(&definition, &metadata)?;
                let mut staged = contracts.clone();
                let mut staged_owners = contract_owners.clone();
                for contract in &metadata.contracts {
                    let key = (contract.id.clone(), contract.version);
                    if definition.kind == ExtensionKind::Plugin {
                        if !definition
                            .namespaces
                            .iter()
                            .any(|n| contract.id == *n || contract.id.starts_with(&format!("{n}.")))
                        {
                            return Err(ExtensionError::Rejected(
                                "contract outside configured plugin namespace".into(),
                            ));
                        }
                        if staged.insert(key, contract.clone()).is_some() {
                            return Err(ExtensionError::Rejected(
                                "duplicate contract registration".into(),
                            ));
                        }
                        staged_owners.insert(
                            (contract.id.clone(), contract.version),
                            definition.id.clone(),
                        );
                    } else if !matches!(
                        contract.id.as_str(),
                        "recuvora.harness" | "recuvora.repair"
                    ) && contracts.get(&key) != Some(contract)
                    {
                        return Err(ExtensionError::Rejected(format!(
                            "node {} has missing or incompatible consumer for {}",
                            definition.id, contract.id
                        )));
                    } else if matches!(contract.id.as_str(), "recuvora.harness" | "recuvora.repair")
                        && contract.version != 1
                    {
                        return Err(ExtensionError::Rejected(
                            "unsupported built-in contract version".into(),
                        ));
                    }
                }
                contracts = staged;
                contract_owners = staged_owners;
                Ok(metadata)
            }
            .await;
            let metadata = match registration {
                Ok(metadata) => {
                    statuses.push(ExtensionStatus {
                        id: definition.id.clone(),
                        kind: definition.kind,
                        available: true,
                        error: None,
                    });
                    metadata
                }
                Err(error) => {
                    statuses.push(ExtensionStatus {
                        id: definition.id.clone(),
                        kind: definition.kind,
                        available: false,
                        error: Some(error.to_string()),
                    });
                    continue;
                }
            };
            entries.insert(
                definition.id.clone(),
                Arc::new(Entry {
                    definition,
                    client,
                    metadata,
                    ordinary: Arc::new(Semaphore::new(settings.ordinary_concurrency)),
                    monitoring_view: Arc::new(Semaphore::new(settings.monitoring_view_concurrency)),
                    approval: Arc::new(Semaphore::new(settings.approval_concurrency)),
                }),
            );
        }
        for definition in &original_definitions {
            if !definition.enabled {
                statuses.push(ExtensionStatus {
                    id: definition.id.clone(),
                    kind: definition.kind,
                    available: false,
                    error: Some("disabled by configuration".into()),
                });
            }
        }
        Ok(Self {
            entries,
            definitions: original_definitions,
            statuses,
            contract_owners,
            settings,
            calls: CallScope::default(),
        })
    }
    pub fn definitions(&self) -> Vec<ExtensionDefinition> {
        self.definitions.clone()
    }
    pub fn statuses(&self) -> &[ExtensionStatus] {
        &self.statuses
    }
    pub fn settings(&self) -> &NodeSettings {
        &self.settings
    }
    pub fn metadata(&self, id: &str) -> Option<&ExtensionMetadata> {
        self.entries.get(id).map(|e| &e.metadata)
    }

    pub fn contract_owner(&self, contract: &str, version: u32) -> Option<&str> {
        self.contract_owners
            .get(&(contract.to_owned(), version))
            .map(String::as_str)
    }

    pub async fn shutdown(&self) -> Result<(), ExtensionError> {
        self.calls
            .shutdown()
            .await
            .map_err(|error| ExtensionError::Unavailable(error.to_string()))
    }

    pub fn begin_shutdown(&self) -> Result<(), ExtensionError> {
        self.calls
            .close()
            .map_err(|error| ExtensionError::Unavailable(error.to_string()))
    }

    pub(super) fn entry(&self, id: &str) -> Result<&Entry, ExtensionError> {
        self.entries
            .get(id)
            .map(AsRef::as_ref)
            .ok_or_else(|| ExtensionError::Unavailable(format!("extension {id} not registered")))
    }
}

pub(super) fn method_for<'a>(
    entry: &'a Entry,
    contract: &str,
    version: u32,
    method: &str,
) -> Result<&'a protocol::MethodDeclaration, ExtensionError> {
    if !entry
        .definition
        .allow_calls
        .iter()
        .any(|a| a.contract == contract && a.version == version && a.method == method)
    {
        return Err(ExtensionError::Rejected(
            "method is not in the trusted configuration allowlist".into(),
        ));
    }
    entry
        .metadata
        .contracts
        .iter()
        .find(|c| c.id == contract && c.version == version)
        .and_then(|c| c.methods.iter().find(|m| m.name == method))
        .ok_or_else(|| ExtensionError::Rejected("method was not registered".into()))
}
