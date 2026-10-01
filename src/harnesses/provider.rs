//! Stable provider futures and trusted adapter factory interfaces.
use super::{
    HarnessDefinition, HarnessError, HarnessProject, HarnessProjectCreateRequest,
    HarnessProjectListRequest, HarnessRunRequest, HarnessRunResult,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type HarnessRunFuture<'a> =
    Pin<Box<dyn Future<Output = Result<HarnessRunResult, HarnessError>> + Send + 'a>>;
pub type HarnessProjectListFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<HarnessProject>, HarnessError>> + Send + 'a>>;
pub type HarnessProjectCreateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<HarnessProject, HarnessError>> + Send + 'a>>;

/// A configured provider hides its vendor protocol behind the common result.
/// Providers observe request cancellation, finish resource cleanup, and retain
/// any unknown persistent outcome in the returned error. Callers must continue
/// awaiting the request after cancelling its token to receive that outcome.
pub trait HarnessProvider: Send + Sync {
    fn definition(&self) -> &HarnessDefinition;
    fn run<'a>(&'a self, request: HarnessRunRequest) -> HarnessRunFuture<'a>;

    fn list_projects<'a>(
        &'a self,
        _request: HarnessProjectListRequest,
    ) -> HarnessProjectListFuture<'a> {
        let harness = self.definition().id.clone();
        Box::pin(async move { Err(HarnessError::ProjectDiscoveryUnsupported { harness }) })
    }

    fn create_project<'a>(
        &'a self,
        _request: HarnessProjectCreateRequest,
    ) -> HarnessProjectCreateFuture<'a> {
        let harness = self.definition().id.clone();
        Box::pin(async move { Err(HarnessError::ProjectCreationUnsupported { harness }) })
    }
}

/// Factories are registered by trusted boot code. A configuration string can
/// select a factory but cannot cause a new SDK or executable to be installed.
pub trait HarnessAdapterFactory: Send + Sync {
    fn adapter_id(&self) -> &str;
    fn build(
        &self,
        definition: HarnessDefinition,
    ) -> Result<Arc<dyn HarnessProvider>, HarnessError>;
}
