//! Authenticated HTTP mapping of the existing trusted services.
mod handlers;
mod history;
mod http;
mod monitoring;
mod plugin_pages;
mod project_logs;
mod recovery;
mod static_files;
mod views;

#[cfg(test)]
use crate::application::journal::Journal;
use crate::application::{Application, ApplicationError, ApplicationErrorKind, Operation};
use crate::harnesses::{
    ConversationPlacement, ConversationVisibility, HarnessCancellation, HarnessDefinition,
    HarnessProjectListRequest, HarnessRunRequest,
};
use crate::monitoring::MonitorHandle;
use crate::persistence::approval::ApprovalDecision;
use crate::recovery::{RecoveryError, RecoveryService};
use crate::repair::WorkflowError;
use crate::simulation::{Engine, Simulation, TaskSpec};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path as RoutePath, Query, Request, State},
    http::{HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
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
impl From<ApplicationError> for ApiError {
    fn from(error: ApplicationError) -> Self {
        let (status, code) = match error.kind {
            ApplicationErrorKind::Invalid => (StatusCode::BAD_REQUEST, "invalid_request"),
            ApplicationErrorKind::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            ApplicationErrorKind::Conflict => (StatusCode::CONFLICT, "conflict"),
            ApplicationErrorKind::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ApplicationErrorKind::Unknown => (StatusCode::CONFLICT, "unknown"),
            ApplicationErrorKind::Capacity => (StatusCode::TOO_MANY_REQUESTS, "capacity"),
        };
        Self::new(status, code, error.message)
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
    fn from(error: WorkflowError) -> Self {
        ApplicationError::from(error).into()
    }
}
impl From<RecoveryError> for ApiError {
    fn from(error: RecoveryError) -> Self {
        ApplicationError::from(error).into()
    }
}

pub struct Console {
    config: ServerConfig,
    assets: static_files::Assets,
    secret: Vec<u8>,
    application: Arc<Application>,
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
    crate::configuration::external_path(path)
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
        let assets = static_files::Assets::load(config.ui_dir.as_deref())?;
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
        let mut control_paths = additional_protected.to_vec();
        control_paths.push(config.token_file.clone());
        control_paths.extend(config.ui_dir.iter().cloned());
        let (application, simulation) =
            crate::boot::application::open(crate::boot::application::ApplicationConfig {
                data_dir: config.data_dir.clone(),
                harness_config: config.harness_config.clone(),
                extensions_config: config.extensions_config.clone(),
                repair_config: config.repair_config.clone(),
                monitors_config: config.monitors_config.clone(),
                recovery_config: config.recovery_config.clone(),
                control_paths,
            })
            .await?;
        Ok((
            Arc::new(Self {
                config,
                assets,
                secret,
                application,
                log_cursors: Mutex::new(BTreeMap::new()),
            }),
            simulation,
        ))
    }
    fn require(&self, permission: &str) -> Result<(), ApiError> {
        self.application.ensure_accepting()?;
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
        self.application
            .harnesses()
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
    pub async fn shutdown(&self) -> Result<(), ApiError> {
        Ok(self.application.shutdown().await?)
    }
    pub async fn wait_for_idle(&self) {
        self.application.wait_for_idle().await;
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
