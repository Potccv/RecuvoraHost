//! Authenticated HTTP mapping of the existing trusted services.
mod handlers;
mod history;
mod http;
mod journal;
mod monitoring;
mod plugin_pages;
mod project_logs;
mod recovery;
mod static_files;
mod views;
pub use project_logs::{LogResultMapping, LogSourceConfig};

use crate::boot::host::{
    HostConfig, HostRuntime, MonitoringHostConfig, load_harness_config, load_repair_config,
};
use crate::harnesses::{
    ConversationPlacement, ConversationVisibility, HarnessCancellation, HarnessDefinition,
    HarnessProjectListRequest, HarnessRegistry, HarnessRunRequest,
};
use crate::integrations::extensions::{ExtensionRegistry, ExtensionsConfig};
use crate::integrations::recovery::{RecoveryError, RecoveryService};
use crate::monitoring::{MonitorHandle, MonitorsConfig};
use crate::persistence::approval::{ApprovalDecision, ApprovalError};
use crate::repair::{RepairConfig, RepairSession, WorkflowError};
use crate::simulation::{Engine, EngineConfig, EngineHandle, Simulation, TaskSpec};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path as RoutePath, Query, Request, State},
    http::{HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use journal::Journal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::File,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PERMISSIONS: &[&str] = &[
    "harness.run",
    "harness.projects",
    "repair.run",
    "approval.decide",
    "approval.apply",
    "approval.check_result",
    "simulation.run",
    "logs.read",
    "operation.cancel",
    "extension.read",
    "monitor.read",
    "incident.read",
    "incident.acknowledge",
    "recovery.read",
    "recovery.decide",
    "recovery.resume",
    "recovery.check_result",
    "knowledge.read",
];

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub schema_version: u32,
    pub listen: SocketAddr,
    pub token_file: PathBuf,
    pub operator: String,
    pub permissions: Vec<String>,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    pub data_dir: PathBuf,
    pub harness_config: Option<PathBuf>,
    pub extensions_config: Option<PathBuf>,
    pub repair_config: Option<PathBuf>,
    pub monitors_config: Option<PathBuf>,
    #[serde(default)]
    pub recovery_config: Option<PathBuf>,
    #[serde(default)]
    pub log_sources: Vec<LogSourceConfig>,
    /// Optional externally built official UI directory, snapshotted at startup.
    #[serde(default)]
    pub ui_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}
impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }
    fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", message)
    }
    fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ApiError {}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error":{"code":self.code,"message":self.message},"auto_retry":false})),
        )
            .into_response()
    }
}
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self::unavailable(e.to_string())
    }
}
impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self::invalid(e.to_string())
    }
}
impl From<WorkflowError> for ApiError {
    fn from(e: WorkflowError) -> Self {
        match &e {
            WorkflowError::Approval(
                ApprovalError::Conflict
                | ApprovalError::InvalidState(_)
                | ApprovalError::Expired
                | ApprovalError::TargetBusy,
            ) => Self::conflict(e.to_string()),
            WorkflowError::Approval(ApprovalError::NotFound) => {
                Self::new(StatusCode::NOT_FOUND, "not_found", e.to_string())
            }
            WorkflowError::OutcomeUnknown { .. } => {
                Self::new(StatusCode::CONFLICT, "unknown", e.to_string())
            }
            _ => Self::invalid(e.to_string()),
        }
    }
}

impl From<RecoveryError> for ApiError {
    fn from(error: RecoveryError) -> Self {
        match &error {
            RecoveryError::Busy
            | RecoveryError::Conflict
            | RecoveryError::Approval(
                ApprovalError::Conflict
                | ApprovalError::InvalidState(_)
                | ApprovalError::Expired
                | ApprovalError::TargetBusy
                | ApprovalError::ReviewNotDue
                | ApprovalError::ReviewTimedOut,
            ) => Self::conflict(error.to_string()),
            RecoveryError::Approval(ApprovalError::NotFound) => {
                Self::new(StatusCode::NOT_FOUND, "not_found", error.to_string())
            }
            RecoveryError::Invalid(_)
            | RecoveryError::Json(_)
            | RecoveryError::Approval(ApprovalError::Invalid(_) | ApprovalError::OutOfScope) => {
                Self::invalid(error.to_string())
            }
            _ => Self::unavailable(error.to_string()),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Operation {
    id: String,
    kind: String,
    status: String,
    #[serde(rename = "updatedAt")]
    updated_at: u64,
    result: Option<Value>,
    error: Option<String>,
    auto_retry: bool,
    context: Value,
}

pub struct Console {
    config: ServerConfig,
    assets: static_files::Assets,
    host: tokio::sync::Mutex<HostRuntime>,
    accepting: std::sync::atomic::AtomicBool,
    secret: Vec<u8>,
    registry: Option<Arc<HarnessRegistry>>,
    extensions: Option<Arc<ExtensionRegistry>>,
    monitoring: Option<Arc<MonitorHandle>>,
    repair: Option<Arc<RepairSession>>,
    repair_config: Option<RepairConfig>,
    recovery: Option<Arc<RecoveryService>>,
    simulation: EngineHandle,
    journal: Mutex<Journal>,
    calls: Mutex<BTreeMap<String, HarnessCancellation>>,
    log_cursors: Mutex<BTreeMap<String, project_logs::LogCursor>>,
}

pub(super) fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
}
fn lock<T>(m: &Mutex<T>) -> Result<MutexGuard<'_, T>, ApiError> {
    m.lock()
        .map_err(|_| ApiError::unavailable("service lock poisoned"))
}
fn external(path: &Path) -> Result<PathBuf, ApiError> {
    if !path.is_absolute() {
        return Err(ApiError::invalid("absolute external path required"));
    }
    crate::boot::host::external_path(path)
        .and_then(|path| path.canonicalize())
        .map_err(|error| ApiError::invalid(error.to_string()))
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ApiError> {
    let path = external(path)?;
    let mut bytes = Vec::new();
    File::open(path)?.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(ApiError::invalid("configuration exceeds 64 KiB"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

impl Console {
    pub async fn open(config: ServerConfig) -> Result<(Arc<Self>, Engine), ApiError> {
        Self::open_protected(config, &[]).await
    }
    async fn open_protected(
        config: ServerConfig,
        additional_protected: &[PathBuf],
    ) -> Result<(Arc<Self>, Engine), ApiError> {
        project_logs::validate_sources(&config.log_sources)?;
        if config.schema_version != 1
            || !config.listen.ip().is_loopback()
            || !valid_id(&config.operator)
            || config
                .permissions
                .iter()
                .any(|p| !PERMISSIONS.contains(&p.as_str()))
            || config.allowed_origins.len() > 16
            || config
                .allowed_origins
                .iter()
                .any(|o| o.len() > 256 || o.contains('*') || o.ends_with('/'))
        {
            return Err(ApiError::invalid(
                "invalid schema, operator, permissions, origin or non-loopback listener",
            ));
        }
        let data = external(&config.data_dir)?;
        crate::boot::host::external_path(&data.join("operations.jsonl"))?;
        crate::boot::host::validate_simulation_paths(&data.join("simulation"))?;
        let assets = static_files::Assets::load(config.ui_dir.as_deref())?;
        let recovery_config = config
            .recovery_config
            .as_ref()
            .map(|path| {
                crate::configuration::RecoveryHostConfig::load(&external(path)?)
                    .map_err(ApiError::from)
            })
            .transpose()?;
        let recovery_storage: Vec<_> = recovery_config
            .iter()
            .flat_map(|config| [config.data_dir.clone(), config.ownership_dir.clone()])
            .collect();
        for storage in &recovery_storage {
            // A dedicated authority directory must not contain application controls.
            if storage.starts_with(&data) || data.starts_with(storage) {
                return Err(ApiError::invalid(
                    "recovery storage must be separate from console storage",
                ));
            }
            for path in [
                Some(&config.token_file),
                config.harness_config.as_ref(),
                config.extensions_config.as_ref(),
                config.repair_config.as_ref(),
                config.monitors_config.as_ref(),
                config.ui_dir.as_ref(),
                config.recovery_config.as_ref(),
            ]
            .into_iter()
            .flatten()
            .chain(additional_protected.iter())
            {
                let path = external(path)?;
                if path.starts_with(storage) || storage.starts_with(&path) {
                    return Err(ApiError::invalid(
                        "recovery storage overlaps an application control path",
                    ));
                }
            }
        }
        let mut secret = Vec::new();
        File::open(external(&config.token_file)?)?
            .take(258)
            .read_to_end(&mut secret)?;
        while secret.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            secret.pop();
        }
        if !(32..=256).contains(&secret.len()) || secret.iter().any(|b| !b.is_ascii_graphic()) {
            return Err(ApiError::invalid(
                "token must contain 32..256 ASCII graphic bytes",
            ));
        }
        let extensions_config = config
            .extensions_config
            .as_ref()
            .map(|path| {
                ExtensionsConfig::load(external(path)?)
                    .map_err(|e| ApiError::invalid(e.to_string()))
            })
            .transpose()?;
        let loaded_repair = config
            .repair_config
            .as_ref()
            .map(|path| load_repair_config(&external(path)?, true).map_err(ApiError::from))
            .transpose()?;
        if let (Some(recovery), Some((repair, _))) = (&recovery_config, &loaded_repair) {
            if recovery.recovery.target.target_id == repair.target_id {
                return Err(ApiError::invalid(
                    "one target cannot use both recovery and legacy text authorities",
                ));
            }
            let harness_path = external(&repair.harness_config)?;
            if recovery_storage.iter().any(|storage| {
                harness_path.starts_with(storage) || storage.starts_with(&harness_path)
            }) {
                return Err(ApiError::invalid(
                    "recovery storage overlaps legacy Harness configuration",
                ));
            }
        }
        if let (Some(_), Some(extensions)) = (&recovery_config, &extensions_config) {
            for path in extensions
                .extensions
                .iter()
                .filter_map(|definition| definition.endpoint.ca_certificate.as_deref())
            {
                let path = external(path)?;
                if recovery_storage
                    .iter()
                    .any(|storage| path.starts_with(storage) || storage.starts_with(&path))
                {
                    return Err(ApiError::invalid(
                        "recovery storage overlaps a TLS trust file",
                    ));
                }
            }
        }
        let harness_path = config
            .harness_config
            .as_ref()
            .or_else(|| loaded_repair.as_ref().map(|(c, _)| &c.harness_config));
        let harness_config = harness_path
            .map(|path| {
                load_harness_config(&external(path)?).map_err(|e| ApiError::invalid(e.to_string()))
            })
            .transpose()?;
        let monitoring_config = config
            .monitors_config
            .as_ref()
            .map(|path| {
                MonitorsConfig::load(external(path)?)
                    .map(|config| MonitoringHostConfig {
                        config,
                        data_dir: data.join("monitoring"),
                    })
                    .map_err(|error| ApiError::invalid(error.to_string()))
            })
            .transpose()?;
        let mut host = HostRuntime::start(HostConfig {
            harnesses: harness_config,
            extensions: extensions_config,
            monitoring: monitoring_config,
        })
        .await
        .map_err(|e| ApiError::unavailable(e.to_string()))?;
        let registry = host.harnesses();
        let extensions = host.extensions();
        let monitoring = host.monitoring();
        let repair_config = loaded_repair.as_ref().map(|(c, _)| c.clone());
        let setup = async {
            let repair = if let Some((cfg, mut protected)) = loaded_repair {
                if recovery_storage
                    .iter()
                    .any(|path| path.starts_with(&cfg.data_dir) || cfg.data_dir.starts_with(path))
                {
                    return Err(ApiError::invalid(
                        "recovery and text repair storage must be separate",
                    ));
                }
                protected.extend_from_slice(additional_protected);
                protected.push(data.clone());
                protected.push(config.token_file.clone());
                if let Some(path) = &config.harness_config {
                    protected.push(external(path)?);
                }
                if let Some(path) = &config.ui_dir {
                    protected.push(path.clone());
                }
                if let Some(path) = &config.monitors_config {
                    protected.push(path.clone());
                }
                if let Some(path) = &config.extensions_config {
                    protected.push(path.clone());
                }
                if let Some(path) = &config.recovery_config {
                    protected.push(path.clone());
                }
                protected.extend(recovery_storage.iter().cloned());
                Some(RepairSession::open(cfg, registry.clone(), &protected)?)
            } else {
                None
            };
            let journal = Journal::open(&data)?;
            let simulation = Engine::open(data.join("simulation"), EngineConfig::default())
                .await
                .map_err(|e| ApiError::unavailable(e.to_string()))?;
            let recovery = if let Some(cfg) = recovery_config {
                match host.start_recovery(cfg).await {
                    Ok(recovery) => Some(recovery),
                    Err(error) => {
                        simulation
                            .shutdown()
                            .await
                            .map_err(|e| ApiError::unavailable(e.to_string()))?;
                        return Err(error.into());
                    }
                }
            } else {
                None
            };
            Ok::<_, ApiError>((repair, journal, simulation, recovery))
        }
        .await;
        let (repair, journal, simulation, recovery) = match setup {
            Ok(resources) => resources,
            Err(error) => {
                host.shutdown().await.map_err(|cleanup| {
                    ApiError::unavailable(format!("{error}; cleanup: {cleanup}"))
                })?;
                return Err(error);
            }
        };
        let handle = simulation.handle();
        Ok((
            Arc::new(Self {
                config,
                assets,
                host: tokio::sync::Mutex::new(host),
                accepting: std::sync::atomic::AtomicBool::new(true),
                secret,
                registry,
                extensions,
                monitoring,
                repair,
                repair_config,
                recovery,
                simulation: handle,
                journal: Mutex::new(journal),
                calls: Mutex::new(BTreeMap::new()),
                log_cursors: Mutex::new(BTreeMap::new()),
            }),
            simulation,
        ))
    }
    fn require(&self, permission: &str) -> Result<(), ApiError> {
        if !self.accepting.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ApiError::unavailable("host is shutting down"));
        }
        if self.config.permissions.iter().any(|p| p == permission) {
            Ok(())
        } else {
            Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                "operator lacks required permission",
            ))
        }
    }
    fn definition(&self, id: &str) -> Result<HarnessDefinition, ApiError> {
        self.registry
            .as_ref()
            .and_then(|r| {
                r.definitions()
                    .into_iter()
                    .find(|h| h.id == id && h.enabled)
            })
            .ok_or_else(|| ApiError::unavailable("Harness is not configured or enabled"))
    }
    fn workspace(&self, h: &HarnessDefinition, workspace: &str) -> Result<PathBuf, ApiError> {
        h.workspace_roots
            .iter()
            .find(|p| p.to_str() == Some(workspace))
            .cloned()
            .ok_or_else(|| ApiError::invalid("workspace is not registered for this Harness"))
    }
    fn begin(
        &self,
        id: String,
        kind: &str,
        context: Value,
    ) -> Result<HarnessCancellation, ApiError> {
        if !valid_id(&id) {
            return Err(ApiError::invalid("invalid operation_id"));
        }
        let mut journal = lock(&self.journal)?;
        if journal.records.contains_key(&id) {
            return Err(ApiError::conflict(
                "operation_id already accepted; query its result",
            ));
        }
        if kind == "repair"
            && context["taskId"].as_str().is_some_and(|task_id| {
                journal.records.values().any(|operation| {
                    operation.kind == "repair" && operation.context["taskId"] == task_id
                })
            })
        {
            return Err(ApiError::conflict(
                "repair task_id already accepted; inspect its existing repair instead of replaying it",
            ));
        }
        if journal.records.len() >= 1000 {
            return Err(ApiError::unavailable("operation capacity reached"));
        }
        let mut calls = lock(&self.calls)?;
        if !self.accepting.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ApiError::unavailable("host is shutting down"));
        }
        if calls.len() >= 8 {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "capacity",
                "too many active operations",
            ));
        }
        let token = HarnessCancellation::new();
        journal.append(Operation {
            id: id.clone(),
            kind: kind.into(),
            status: "running".into(),
            updated_at: timestamp(),
            result: None,
            error: None,
            auto_retry: false,
            context,
        })?;
        calls.insert(id, token.clone());
        Ok(token)
    }
    fn finish(&self, id: &str, result: Result<Value, String>) {
        if let Ok(mut journal) = lock(&self.journal)
            && let Some(mut op) = journal.records.get(id).cloned()
        {
            match result {
                Ok(value) => {
                    op.status = match value.get("status").and_then(Value::as_str) {
                        Some("unknown") => "unknown",
                        Some("canceled") => "canceled",
                        Some("failed") => "failed",
                        _ => "completed",
                    }
                    .into();
                    op.result = Some(value);
                }
                Err(message) => {
                    op.status = "unknown".into();
                    op.error = Some(message);
                }
            }
            op.updated_at = timestamp();
            if journal.append(op.clone()).is_err() {
                op.status = "unknown".into();
                op.result = None;
                op.error = Some("completion could not be persisted; do not retry".into());
                journal.records.insert(id.into(), op);
            }
        }
        if let Ok(mut calls) = lock(&self.calls) {
            calls.remove(id);
        }
    }
    pub async fn shutdown(&self) -> Result<(), ApiError> {
        self.wait_for_idle().await;
        self.host
            .lock()
            .await
            .shutdown()
            .await
            .map_err(|error| ApiError::unavailable(error.to_string()))?;
        if !lock(&self.calls)?.is_empty() {
            return Err(ApiError::unavailable(
                "operation receipts remain pending after shutdown",
            ));
        }
        Ok(())
    }
    pub async fn wait_for_idle(&self) {
        self.accepting
            .store(false, std::sync::atomic::Ordering::Release);
        self.host.lock().await.begin_shutdown();
        if let Ok(calls) = lock(&self.calls) {
            for token in calls.values() {
                token.cancel();
            }
        }
        for _ in 0..400 {
            if lock(&self.calls).is_ok_and(|c| c.is_empty()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

pub fn router(console: Arc<Console>) -> Router {
    http::router(console)
}

pub async fn run_cli(args: impl IntoIterator<Item = OsString>) -> Result<(), ApiError> {
    let args: Vec<_> = args.into_iter().collect();
    if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h" | "help")) {
        println!(
            "Usage: recuvora-host serve --config <external-console-config.json>\nSet ui_dir to serve an externally built official UI.\nThe listener must be loopback; use TLS proxy or SSH forwarding for remote access.\nSee docs/api/http.md for token, permission, storage and node configuration."
        );
        return Ok(());
    }
    if args.len() != 2 || args[0] != "--config" {
        return Err(ApiError::invalid(
            "usage: recuvora-host serve --config <external-config.json>",
        ));
    }
    let configuration_path = external(Path::new(&args[1]))?;
    let config: ServerConfig = read_json(&configuration_path)?;
    let listen = config.listen;
    let (console, engine) = Console::open_protected(config, &[configuration_path]).await?;
    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(listener) => listener,
        Err(error) => {
            console.shutdown().await?;
            engine
                .shutdown()
                .await
                .map_err(|e| ApiError::unavailable(e.to_string()))?;
            return Err(error.into());
        }
    };
    println!(
        "Recuvora console listening on http://{} (Bearer authentication required)",
        listener.local_addr()?
    );
    let shutdown = console.clone();
    let served = axum::serve(listener, router(console.clone()))
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown.wait_for_idle().await;
        })
        .await;
    let shutdown = console.shutdown().await;
    engine
        .shutdown()
        .await
        .map_err(|e| ApiError::unavailable(e.to_string()))?;
    shutdown?;
    served?;
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/server.rs"]
mod tests;
