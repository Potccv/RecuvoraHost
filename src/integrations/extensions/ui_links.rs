//! Optional plugin page discovery and snapshots; deployment bindings are immutable.
use super::registry::{dispatch_error, method_for};
use super::{ExtensionCall, ExtensionError, ExtensionKind, ExtensionRegistry, page_urls};
use crate::protocol;
use crate::runtime::operation::Cancellation;
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::Semaphore;

pub const UI_LINKS_CAPABILITY: &str = "recuvora.ui_links.v1";
pub const UI_LINKS_METHOD: &str = "describe_ui_links";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UiLinksConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub entrypoints: BTreeMap<String, String>,
}

impl UiLinksConfig {
    pub(super) fn validate(&self, kind: ExtensionKind) -> Result<(), ExtensionError> {
        if self.entrypoints.len() > 16
            || (kind != ExtensionKind::Plugin && (self.enabled || !self.entrypoints.is_empty()))
        {
            return Err(ExtensionError::Configuration(
                "page bindings require a plugin and at most 16 entrypoints".into(),
            ));
        }
        for (id, base) in &self.entrypoints {
            if !protocol::valid_id(id) {
                return Err(ExtensionError::Configuration(
                    "invalid page entrypoint identity".into(),
                ));
            }
            page_urls::base_url(base)
                .map_err(|message| ExtensionError::Configuration(message.into()))?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Description {
    schema_version: u32,
    revision: String,
    pages: Vec<Page>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Page {
    id: String,
    title: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_text"
    )]
    description: Option<String>,
    entrypoint: String,
    relative_url: String,
    open_mode: String,
}

fn optional_text<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Serialize)]
pub struct UiLink {
    pub kind: &'static str,
    pub plugin_id: String,
    pub page_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub url: String,
    pub open_mode: &'static str,
    pub descriptor_revision: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct UnboundPage {
    pub page_id: String,
    pub entrypoint: String,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct UiLinksSnapshot {
    pub plugin_id: String,
    pub status: &'static str,
    pub descriptor_revision: Option<String>,
    pub error: Option<&'static str>,
    pub links: Vec<UiLink>,
    pub unavailable_pages: Vec<UnboundPage>,
}

impl UiLinksSnapshot {
    fn empty(id: &str, status: &'static str) -> Self {
        Self {
            plugin_id: id.into(),
            status,
            descriptor_revision: None,
            error: None,
            links: Vec::new(),
            unavailable_pages: Vec::new(),
        }
    }
}

pub(super) struct UiLinksRuntime {
    capacity: Arc<Semaphore>,
    state: Mutex<PageState>,
}
struct PageState {
    closed: bool,
    last: Option<Description>,
    snapshot: UiLinksSnapshot,
}

impl UiLinksRuntime {
    pub(super) fn new(id: &str) -> Self {
        Self {
            capacity: Arc::new(Semaphore::new(1)),
            state: Mutex::new(PageState {
                closed: false,
                last: None,
                snapshot: UiLinksSnapshot::empty(id, "unavailable"),
            }),
        }
    }
    fn lock(&self) -> Result<MutexGuard<'_, PageState>, ExtensionError> {
        self.state
            .lock()
            .map_err(|_| ExtensionError::Unavailable("page snapshot lock poisoned".into()))
    }
    pub(super) fn close(&self) -> Result<(), ExtensionError> {
        let mut state = self.lock()?;
        state.closed = true;
        state.snapshot.status = "unavailable";
        state.snapshot.links.clear();
        state.snapshot.unavailable_pages.clear();
        state.snapshot.error = Some("host is shutting down");
        Ok(())
    }
}

fn text_valid(text: &str, limit: usize, required: bool) -> bool {
    text.len() <= limit
        && (!required || !text.trim().is_empty())
        && !text.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}')
}

fn description(value: Value) -> Result<Description, &'static str> {
    if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > 32 * 1024) {
        return Err("page description exceeds 32 KiB");
    }
    let result: Description =
        serde_json::from_value(value).map_err(|_| "invalid page description fields")?;
    if result.schema_version != 1
        || !protocol::valid_id(&result.revision)
        || result.pages.len() > 16
    {
        return Err("invalid page description version, revision or size");
    }
    let mut ids = BTreeSet::new();
    for page in &result.pages {
        if !protocol::valid_id(&page.id)
            || !ids.insert(&page.id)
            || !protocol::valid_id(&page.entrypoint)
            || page.open_mode != "external"
            || !text_valid(&page.title, 128, true)
            || page
                .description
                .as_ref()
                .is_some_and(|text| !text_valid(text, 1024, false))
        {
            return Err("invalid or duplicate page");
        }
        page_urls::relative_url(&page.relative_url)?;
    }
    Ok(result)
}

impl ExtensionRegistry {
    fn ui_links_method(&self, id: &str) -> Result<Option<(String, u32)>, ExtensionError> {
        let entry = self.entry(id)?;
        if !entry
            .metadata
            .capabilities
            .iter()
            .any(|c| c == UI_LINKS_CAPABILITY)
        {
            return Ok(None);
        }
        let methods = entry
            .metadata
            .contracts
            .iter()
            .flat_map(|contract| {
                contract
                    .methods
                    .iter()
                    .filter(|method| method.name == UI_LINKS_METHOD)
                    .map(move |method| (contract, method))
            })
            .collect::<Vec<_>>();
        if methods.len() != 1 {
            return Err(ExtensionError::Rejected(
                "page capability requires one descriptor method".into(),
            ));
        }
        let (contract, method) = methods[0];
        if entry.definition.kind != ExtensionKind::Plugin
            || contract.version != 1
            || !method.read_only
            || contract.id == "recuvora"
            || contract.id.starts_with("recuvora.")
            || self.contract_owner(&contract.id, contract.version) != Some(id)
        {
            return Err(ExtensionError::Rejected(
                "invalid page descriptor registration".into(),
            ));
        }
        method_for(entry, &contract.id, contract.version, UI_LINKS_METHOD)?;
        protocol::validate_value(&method.input_schema, &json!({"schema_version":1}))?;
        Ok(Some((contract.id.clone(), contract.version)))
    }

    pub fn ui_links(&self, id: &str) -> Result<UiLinksSnapshot, ExtensionError> {
        let definition = self
            .definitions
            .iter()
            .find(|d| d.id == id && d.kind == ExtensionKind::Plugin)
            .ok_or_else(|| ExtensionError::Rejected("unknown plugin".into()))?;
        if !definition.enabled || !definition.ui_links.enabled {
            return Ok(UiLinksSnapshot::empty(id, "disabled"));
        }
        let Ok(entry) = self.entry(id) else {
            return Ok(UiLinksSnapshot::empty(id, "unavailable"));
        };
        match self.ui_links_method(id) {
            Ok(None) => Ok(UiLinksSnapshot::empty(id, "not_declared")),
            Err(_) => Ok(UiLinksSnapshot::empty(id, "invalid_registration")),
            Ok(Some(_)) => Ok(entry.ui_links.lock()?.snapshot.clone()),
        }
    }

    pub fn ui_links_catalog(&self) -> Result<Vec<UiLinksSnapshot>, ExtensionError> {
        self.definitions
            .iter()
            .filter(|d| d.kind == ExtensionKind::Plugin)
            .map(|d| self.ui_links(&d.id))
            .collect()
    }

    pub async fn refresh_ui_links(
        &self,
        id: &str,
        cancellation: Cancellation,
    ) -> Result<UiLinksSnapshot, ExtensionError> {
        let snapshot = self.ui_links(id)?;
        if matches!(
            snapshot.status,
            "disabled" | "not_declared" | "invalid_registration"
        ) {
            return Ok(snapshot);
        }
        let entry = self.entry(id)?;
        let permit = entry
            .ui_links
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| ExtensionError::Rejected("page description capacity exhausted".into()))?;
        let registry = self.clone();
        let id = id.to_owned();
        self.calls
            .run(cancellation.clone(), async move {
                // Capacity covers transport cleanup, validation and publication.
                let _permit = permit;
                registry.refresh_ui_links_inner(&id, cancellation).await
            })
            .await
            .map_err(dispatch_error)?
    }

    async fn refresh_ui_links_inner(
        &self,
        id: &str,
        cancellation: Cancellation,
    ) -> Result<UiLinksSnapshot, ExtensionError> {
        let entry = self.entry(id)?;
        if entry.ui_links.lock()?.closed {
            return Err(ExtensionError::Unavailable("host is shutting down".into()));
        }
        let (contract, version) = self
            .ui_links_method(id)?
            .ok_or_else(|| ExtensionError::Rejected("page capability missing".into()))?;
        let method = method_for(entry, &contract, version, UI_LINKS_METHOD)?;
        let result = entry
            .client
            .call_with_settings(
                ExtensionCall {
                    contract: contract.clone(),
                    version,
                    method: UI_LINKS_METHOD.into(),
                    params: json!({"schema_version":1}),
                    timeout: Duration::from_secs(5),
                },
                entry.metadata.clone(),
                cancellation.clone(),
                None,
                None,
                &self.settings.protocol,
            )
            .await;
        let validated = match result {
            Ok(value) => protocol::validate_value(&method.output_schema, &value)
                .map_err(|_| ("invalid_response", "page output violates its schema"))
                .and_then(|_| description(value).map_err(|message| ("invalid_response", message))),
            // Provider text can contain credentials or arbitrary URLs.
            Err(_) => Err(("unavailable", "page description request failed")),
        };
        let mut state = entry.ui_links.lock()?;
        if state.closed || cancellation.is_cancelled() {
            state.snapshot.links.clear();
            state.snapshot.unavailable_pages.clear();
            state.snapshot.status = "unavailable";
            state.snapshot.error = Some("page description was cancelled");
            return Ok(state.snapshot.clone());
        }
        let outcome = validated.and_then(|description| {
            if state
                .last
                .as_ref()
                .is_some_and(|last| last.revision == description.revision && last != &description)
            {
                return Err(("invalid_response", "page revision content changed"));
            }
            let mut snapshot = UiLinksSnapshot::empty(id, "ready");
            snapshot.descriptor_revision = Some(description.revision.clone());
            for page in &description.pages {
                if let Some(base) = entry.definition.ui_links.entrypoints.get(&page.entrypoint) {
                    let url = page_urls::resolve(base, &page.relative_url)
                        .map_err(|message| ("invalid_response", message))?;
                    snapshot.links.push(UiLink {
                        kind: "external_link",
                        plugin_id: id.into(),
                        page_id: page.id.clone(),
                        title: page.title.clone(),
                        description: page.description.clone(),
                        url,
                        open_mode: "external",
                        descriptor_revision: description.revision.clone(),
                    });
                } else {
                    snapshot.unavailable_pages.push(UnboundPage {
                        page_id: page.id.clone(),
                        entrypoint: page.entrypoint.clone(),
                        reason: "binding_missing",
                    });
                }
            }
            Ok((description, snapshot))
        });
        match outcome {
            Ok((description, snapshot)) => {
                state.last = Some(description);
                state.snapshot = snapshot;
            }
            Err((status, message)) => {
                state.snapshot.status = status;
                state.snapshot.error = Some(message);
                state.snapshot.links.clear();
                state.snapshot.unavailable_pages.clear();
            }
        }
        Ok(state.snapshot.clone())
    }

    pub(super) async fn initialize_ui_links(&self) {
        let pending = self
            .definitions
            .iter()
            .filter(|definition| {
                definition.kind == ExtensionKind::Plugin
                    && definition.enabled
                    && definition.ui_links.enabled
            })
            .map(|definition| self.refresh_ui_links(&definition.id, Cancellation::new()));
        // Bound startup fan-out while retaining each call in this registry's scope.
        stream::iter(pending)
            .buffer_unordered(4)
            .for_each(|_| async {})
            .await;
    }
}
