//! Provider-native project discovery and explicit creation request contracts.
use super::limits::DEFAULT_TURN_DURATION;
use super::{HarnessCancellation, RemoteWorkspace};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A provider-native project discovered or created by one Harness.
///
/// `id` is meaningful only together with `harness_id`. Roots are provider
/// metadata and must be revalidated before they influence an execution scope.
/// A separate desktop client may maintain another project id and membership
/// layer that this value does not update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessProject {
    pub harness_id: String,
    pub id: String,
    pub name: String,
    pub roots: Vec<PathBuf>,
}

/// Bounded request to discover the projects visible to one Harness instance.
#[derive(Clone, Debug)]
pub struct HarnessProjectListRequest {
    pub(super) context_directory: PathBuf,
    pub(super) remote_workspace: Option<RemoteWorkspace>,
    pub(super) timeout: Duration,
    pub(super) cancellation: HarnessCancellation,
}

impl HarnessProjectListRequest {
    pub fn new(context_directory: impl Into<PathBuf>) -> Self {
        Self {
            context_directory: context_directory.into(),
            remote_workspace: None,
            timeout: DEFAULT_TURN_DURATION,
            cancellation: HarnessCancellation::new(),
        }
    }

    pub fn remote(node_id: impl Into<String>, workspace_id: impl Into<String>) -> Self {
        let workspace = RemoteWorkspace {
            node_id: node_id.into(),
            workspace_id: workspace_id.into(),
        };
        let mut request = Self::new(workspace.resource_path());
        request.remote_workspace = Some(workspace);
        request
    }
    pub fn remote_workspace(&self) -> Option<&RemoteWorkspace> {
        self.remote_workspace.as_ref()
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn context_directory(&self) -> &Path {
        &self.context_directory
    }

    pub fn with_cancellation(mut self, cancellation: HarnessCancellation) -> Self {
        self.cancellation = cancellation;
        self
    }

    pub fn cancellation(&self) -> &HarnessCancellation {
        &self.cancellation
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

/// Request to register one existing, allowed directory as a provider project.
///
/// This contract never creates the root directory. The idempotency key lets a
/// supporting provider make an explicit create request safe to retry.
#[derive(Clone, Debug)]
pub struct HarnessProjectCreateRequest {
    pub(super) name: String,
    pub(super) root_directory: PathBuf,
    pub(super) remote_workspace: Option<RemoteWorkspace>,
    pub(super) idempotency_key: String,
    pub(super) timeout: Duration,
    pub(super) cancellation: HarnessCancellation,
}

impl HarnessProjectCreateRequest {
    pub fn new(
        name: impl Into<String>,
        root_directory: impl Into<PathBuf>,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            root_directory: root_directory.into(),
            remote_workspace: None,
            idempotency_key: idempotency_key.into(),
            timeout: DEFAULT_TURN_DURATION,
            cancellation: HarnessCancellation::new(),
        }
    }

    pub fn remote(
        node_id: impl Into<String>,
        workspace_id: impl Into<String>,
        name: impl Into<String>,
        idempotency_key: impl Into<String>,
    ) -> Self {
        let workspace = RemoteWorkspace {
            node_id: node_id.into(),
            workspace_id: workspace_id.into(),
        };
        let mut request = Self::new(name, workspace.resource_path(), idempotency_key);
        request.remote_workspace = Some(workspace);
        request
    }
    pub fn remote_workspace(&self) -> Option<&RemoteWorkspace> {
        self.remote_workspace.as_ref()
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn with_cancellation(mut self, cancellation: HarnessCancellation) -> Self {
        self.cancellation = cancellation;
        self
    }

    pub fn cancellation(&self) -> &HarnessCancellation {
        &self.cancellation
    }

    pub fn root_directory(&self) -> &Path {
        &self.root_directory
    }

    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}
