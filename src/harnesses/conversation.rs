//! Conversation placement, role, request builders and provider run results.
use super::limits::DEFAULT_TURN_DURATION;
use super::{HarnessCancellation, HarnessTool, HarnessToolHandler, RemoteWorkspace};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Roles share an instance but never share an approval/execution conversation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HarnessRole {
    #[default]
    Execution,
    Approval,
}

/// Whether a provider conversation is stored for a conversation client.
///
/// `Hidden` maps to an ephemeral provider thread. It describes client history,
/// not data privacy: a provider still receives the request. `Client` asks the
/// provider to persist the thread so a compatible client can display it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConversationVisibility {
    Hidden,
    Client,
}

/// What has been verified about a separate conversation client's project UI.
///
/// A provider-native project response and a desktop client's project grouping
/// can use different identifiers and persistence layers. Providers must return
/// `Unverified` unless they have observed the separate client state through a
/// supported integration. `Confirmed { client_project_id: None }` means that
/// the client was observed outside every project at that point in time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientProjectGrouping {
    NotApplicable,
    Unverified,
    Confirmed { client_project_id: Option<String> },
}

/// Optional provider-native project assignment for a conversation.
///
/// Project identifiers are opaque and scoped to one configured Harness
/// instance. A provider confirming this assignment does not by itself promise
/// that a separate desktop client has refreshed its own project membership.
/// The execution directory remains an independent, mandatory security boundary
/// even when no project is assigned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConversationPlacement {
    NoNativeProject,
    ExistingProject {
        harness_id: String,
        project_id: String,
    },
}

impl ConversationPlacement {
    pub fn project_id(&self) -> Option<&str> {
        match self {
            Self::NoNativeProject => None,
            Self::ExistingProject { project_id, .. } => Some(project_id),
        }
    }
}

/// One request to create a new provider conversation and run one turn.
#[derive(Clone)]
pub struct HarnessRunRequest {
    pub(super) project_directory: PathBuf,
    pub(super) remote_workspace: Option<RemoteWorkspace>,
    pub(super) prompt: String,
    pub(super) model: Option<String>,
    pub(super) timeout: Duration,
    pub(super) visibility: ConversationVisibility,
    pub(super) placement: ConversationPlacement,
    pub(super) cancellation: HarnessCancellation,
    pub(super) role: HarnessRole,
    pub(super) tools: Vec<HarnessTool>,
    pub(super) tool_handler: Option<Arc<dyn HarnessToolHandler>>,
}

impl std::fmt::Debug for HarnessRunRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessRunRequest")
            .field("project_directory", &self.project_directory)
            .field("role", &self.role)
            .field("tools", &self.tools)
            .field("timeout", &self.timeout)
            .field("visibility", &self.visibility)
            .finish_non_exhaustive()
    }
}

impl HarnessRunRequest {
    pub fn new(project_directory: impl Into<PathBuf>, prompt: impl Into<String>) -> Self {
        Self {
            project_directory: project_directory.into(),
            remote_workspace: None,
            prompt: prompt.into(),
            model: None,
            timeout: DEFAULT_TURN_DURATION,
            visibility: ConversationVisibility::Client,
            placement: ConversationPlacement::NoNativeProject,
            cancellation: HarnessCancellation::new(),
            role: HarnessRole::Execution,
            tools: Vec::new(),
            tool_handler: None,
        }
    }

    /// Selects a node-owned workspace identifier, never a host filesystem path.
    pub fn remote(
        node_id: impl Into<String>,
        workspace_id: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        let workspace = RemoteWorkspace {
            node_id: node_id.into(),
            workspace_id: workspace_id.into(),
        };
        let mut request = Self::new(workspace.resource_path(), prompt);
        request.remote_workspace = Some(workspace);
        request
    }

    pub fn remote_workspace(&self) -> Option<&RemoteWorkspace> {
        self.remote_workspace.as_ref()
    }

    pub fn with_role(mut self, role: HarnessRole) -> Self {
        self.role = role;
        if role == HarnessRole::Approval {
            self.visibility = ConversationVisibility::Hidden;
            self.placement = ConversationPlacement::NoNativeProject;
        }
        self
    }

    pub fn role(&self) -> HarnessRole {
        self.role
    }

    pub fn with_tools(
        mut self,
        tools: Vec<HarnessTool>,
        handler: Arc<dyn HarnessToolHandler>,
    ) -> Self {
        self.tools = tools;
        self.tool_handler = Some(handler);
        self
    }

    pub fn tools(&self) -> &[HarnessTool] {
        &self.tools
    }

    pub fn tool_handler(&self) -> Option<&Arc<dyn HarnessToolHandler>> {
        self.tool_handler.as_ref()
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_cancellation(mut self, cancellation: HarnessCancellation) -> Self {
        self.cancellation = cancellation;
        self
    }

    pub fn cancellation(&self) -> &HarnessCancellation {
        &self.cancellation
    }

    pub fn with_visibility(mut self, visibility: ConversationVisibility) -> Self {
        self.visibility = visibility;
        self
    }

    pub fn with_placement(mut self, placement: ConversationPlacement) -> Self {
        self.placement = placement;
        self
    }

    /// Compatibility builder for callers that use the provider protocol term.
    /// Prefer [`Self::with_visibility`] in user-facing code.
    pub fn ephemeral(mut self, ephemeral: bool) -> Self {
        self.visibility = if ephemeral {
            ConversationVisibility::Hidden
        } else {
            ConversationVisibility::Client
        };
        self
    }

    pub fn project_directory(&self) -> &Path {
        &self.project_directory
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn visibility(&self) -> ConversationVisibility {
        self.visibility
    }

    pub fn placement(&self) -> &ConversationPlacement {
        &self.placement
    }

    pub fn is_ephemeral(&self) -> bool {
        self.visibility == ConversationVisibility::Hidden
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessRunResult {
    pub harness_id: String,
    pub adapter: String,
    pub address: String,
    pub thread_id: String,
    pub session_id: String,
    pub project_directory: PathBuf,
    pub visibility: ConversationVisibility,
    /// The provider-native project id confirmed for the new thread, if any.
    /// This is not necessarily an id used by a separate desktop client.
    pub native_project_id: Option<String>,
    pub client_project_grouping: ClientProjectGrouping,
    pub final_response: String,
}
