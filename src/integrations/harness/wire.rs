//! Strict remote Harness result DTOs, separate from local domain contracts.
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WireProject {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) roots: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WireRunResult {
    pub(super) thread_id: String,
    pub(super) session_id: String,
    pub(super) project_directory: String,
    pub(super) visibility: String,
    pub(super) native_project_id: Option<String>,
    pub(super) client_project_grouping: WireGrouping,
    pub(super) final_response: String,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum WireGrouping {
    NotApplicable,
    Unverified,
    Confirmed { client_project_id: Option<String> },
}
