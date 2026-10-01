//! Module, service and lifecycle contracts exposed by the framework.
use super::ModuleContext;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct InstanceId(pub String);

impl InstanceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Exact service identity. Major versions and scopes never implicitly fall back.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ServiceKey {
    pub name: String,
    pub major: u32,
    pub scope: String,
}

impl ServiceKey {
    pub fn new(name: impl Into<String>, major: u32, scope: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            major,
            scope: scope.into(),
        }
    }
}

impl std::fmt::Display for ServiceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}[{}]", self.name, self.major, self.scope)
    }
}

#[derive(Clone, Debug)]
pub struct ModuleMetadata {
    pub instance: InstanceId,
    pub provides: Vec<ServiceKey>,
    pub requires: Vec<ServiceKey>,
}

impl ModuleMetadata {
    pub fn new(instance: InstanceId) -> Self {
        Self {
            instance,
            provides: Vec::new(),
            requires: Vec::new(),
        }
    }

    pub fn provides(mut self, service: ServiceKey) -> Self {
        self.provides.push(service);
        self
    }

    pub fn requires(mut self, service: ServiceKey) -> Self {
        self.requires.push(service);
        self
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{message}")]
pub struct ModuleError {
    pub message: String,
}

impl ModuleError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub type LifecycleFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ModuleError>> + Send + 'a>>;

/// Hooks must yield, be cancellation safe, and finish cleanup after partial startup.
/// Stop must tolerate a retry if its future was cancelled by the caller.
pub trait Module: Send {
    fn metadata(&self) -> ModuleMetadata;
    fn start<'a>(&'a mut self, context: &'a ModuleContext) -> LifecycleFuture<'a>;
    fn stop<'a>(&'a mut self, _context: &'a ModuleContext) -> LifecycleFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Debug)]
pub struct LifecycleOptions {
    pub start_timeout: Duration,
    pub stop_timeout: Duration,
    pub cleanup_timeout: Duration,
    pub max_modules: usize,
    pub max_resources_per_module: usize,
    pub max_subscriptions_per_module: usize,
    pub max_event_queue: usize,
    pub max_event_bytes: usize,
}

impl Default for LifecycleOptions {
    fn default() -> Self {
        Self {
            start_timeout: Duration::from_secs(5),
            stop_timeout: Duration::from_secs(5),
            cleanup_timeout: Duration::from_secs(2),
            max_modules: 64,
            max_resources_per_module: 128,
            max_subscriptions_per_module: 64,
            max_event_queue: 256,
            max_event_bytes: 64 * 1024,
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum FrameworkError {
    #[error("invalid configuration: {0}")]
    InvalidConfiguration(String),
    #[error("runtime already started or closed")]
    RuntimeNotFresh,
    #[error("duplicate instance: {0}")]
    DuplicateInstance(InstanceId),
    #[error("service provider conflict: {0}")]
    ProviderConflict(ServiceKey),
    #[error("{instance} requires missing service {service}")]
    MissingDependency {
        instance: InstanceId,
        service: ServiceKey,
    },
    #[error("cyclic module dependencies: {0:?}")]
    DependencyCycle(Vec<InstanceId>),
    #[error("module {instance} did not publish declared service {service}")]
    UnpublishedService {
        instance: InstanceId,
        service: ServiceKey,
    },
    #[error("service {service} is not declared by {instance}")]
    UndeclaredService {
        instance: InstanceId,
        service: ServiceKey,
    },
    #[error("service is unavailable: {0}")]
    ServiceUnavailable(ServiceKey),
    #[error("service Rust type does not match: {0}")]
    ServiceTypeMismatch(ServiceKey),
    #[error("instance is no longer accepting work: {0}")]
    InstanceInactive(InstanceId),
    #[error("capacity exceeded: {0}")]
    CapacityExceeded(String),
    #[error("{instance} {phase} failed: {message}")]
    LifecycleFailure {
        instance: InstanceId,
        phase: &'static str,
        message: String,
    },
    #[error("{instance} {phase} timed out")]
    LifecycleTimeout {
        instance: InstanceId,
        phase: &'static str,
    },
    #[error("framework state lock was poisoned")]
    StatePoisoned,
}

impl From<FrameworkError> for ModuleError {
    fn from(error: FrameworkError) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Debug, Default)]
pub struct ShutdownReport {
    pub issues: Vec<FrameworkError>,
}

impl ShutdownReport {
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }
}

#[derive(Debug, Error)]
#[error("startup failed: {cause}; cleanup issues: {cleanup:?}")]
pub struct StartupError {
    pub cause: FrameworkError,
    pub cleanup: ShutdownReport,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    pub active_instances: usize,
    pub pending_cleanup_instances: usize,
    pub services: usize,
    pub subscriptions: usize,
    pub disposers: usize,
    pub background_tasks: usize,
}
