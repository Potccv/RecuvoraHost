//! Accepted work runs independently of an HTTP connection and retains its receipt.
use super::*;
use crate::harnesses::{ConversationVisibility, HarnessRunRequest};
use crate::persistence::approval::{ApprovalDecision, ApprovalRecord};
use crate::repair::WorkflowError;
use crate::simulation::TaskSpec;

impl Application {
    pub(crate) async fn text_repair_decision(
        &self,
        id: String,
        action: String,
        revision: u64,
        actor: String,
        reason: String,
    ) -> Result<ApprovalRecord, ApplicationError> {
        let repair = self
            .text_repair
            .clone()
            .ok_or_else(|| ApplicationError::unavailable("repair is not configured"))?;
        Ok(tokio::task::spawn_blocking(move || match action.as_str() {
            "approve" => repair.decide_authenticated(
                &id,
                revision,
                &actor,
                ApprovalDecision::Approve,
                reason,
            ),
            "deny" => {
                repair.decide_authenticated(&id, revision, &actor, ApprovalDecision::Deny, reason)
            }
            "revoke" => repair.revoke_authenticated(&id, revision, &actor, reason),
            "apply" => repair.apply_versioned(&id, Some(revision), &HarnessCancellation::new()),
            "check_result" => repair.check_result_authenticated(&id, Some(revision), &actor),
            _ => Err(WorkflowError::Invalid("unknown approval action".into())),
        })
        .await
        .map_err(|error| ApplicationError::unavailable(error.to_string()))??)
    }

    pub(crate) fn start_harness_run(
        self: &Arc<Self>,
        operation_id: String,
        harness_id: String,
        request: HarnessRunRequest,
        workspace_id: String,
    ) -> Result<(), ApplicationError> {
        let registry = self
            .registry
            .clone()
            .ok_or_else(|| ApplicationError::unavailable("Harness unavailable"))?;
        let token = self.begin(
            operation_id.clone(),
            "harness",
            json!({"harnessId":harness_id,"workspaceId":workspace_id}),
        )?;
        let application = self.clone();
        tokio::spawn(async move {
            let result = registry
                .run(Some(&harness_id), request.with_cancellation(token))
                .await;
            let receipt = match result {
                Ok(r) => {
                    json!({"status":"completed","harness_id":r.harness_id,"adapter":r.adapter,"address":r.address,"session_id":r.session_id,"thread_id":r.thread_id,"cwd":r.project_directory,"visibility":match r.visibility {ConversationVisibility::Hidden=>"hidden",ConversationVisibility::Client=>"client"},"final_response":r.final_response,"native_project_id":r.native_project_id,"client_project_grouping":crate::presentation::grouping_json(&r.client_project_grouping),"business_verified":false})
                }
                Err(error) => crate::presentation::error_json(&error),
            };
            application.finish(&operation_id, Ok(receipt));
        });
        Ok(())
    }

    pub(crate) fn start_text_repair(
        self: &Arc<Self>,
        operation_id: String,
        task_id: String,
        prompt: String,
        source_incident: Option<Value>,
    ) -> Result<(), ApplicationError> {
        let repair = self
            .text_repair
            .clone()
            .ok_or_else(|| ApplicationError::unavailable("repair is not configured"))?;
        if !valid_id(&task_id) || prompt.trim().is_empty() || prompt.len() > 8192 {
            return Err(ApplicationError::invalid("invalid repair task or prompt"));
        }
        if repair
            .records()?
            .iter()
            .any(|record| record.request.operation.task_id == task_id)
        {
            return Err(ApplicationError::conflict(
                "repair task_id already has persistent approvals; inspect its existing requests instead of starting another run",
            ));
        }
        let target = self
            .text_repair_config
            .as_ref()
            .map(|config| &config.target_id);
        let token = self.begin(
            operation_id.clone(),
            "repair",
            json!({"taskId":task_id,"target":target,"sourceIncident":source_incident}),
        )?;
        let application = self.clone();
        tokio::spawn(async move {
            let result = repair.run(task_id, prompt, token).await;
            application.finish(
                &operation_id,
                result
                    .map(|result| crate::presentation::repair_result_json(&result))
                    .map_err(|error| error.to_string()),
            );
        });
        Ok(())
    }

    pub(crate) fn start_simulation(
        self: &Arc<Self>,
        operation_id: String,
        spec: TaskSpec,
        scenario: String,
    ) -> Result<(), ApplicationError> {
        let token = self.begin(
            operation_id.clone(),
            "simulation",
            json!({"taskId":spec.id,"target":spec.target,"scenario":scenario}),
        )?;
        let application = self.clone();
        tokio::spawn(async move {
            let engine = application.simulation.clone();
            let task_id = spec.id.clone();
            let result = async {
                let mut snapshot = engine.submit(spec).await?;
                while !snapshot.state.is_terminal() {
                    tokio::select! {
                        _ = token.cancelled() => { if let Some(next) = engine.cancel(&task_id).await? { snapshot = next; } },
                        _ = tokio::time::sleep(Duration::from_millis(50)) => { if let Some(next) = engine.query(&task_id).await? { snapshot = next; } },
                    }
                }
                Ok::<_, crate::simulation::EngineError>(json!({"status":match snapshot.state {crate::simulation::TaskState::Unknown=>"unknown",crate::simulation::TaskState::Canceled=>"canceled",_=>"completed"},"task":snapshot}))
            }.await;
            application.finish(&operation_id, result.map_err(|error| error.to_string()));
        });
        Ok(())
    }
}
