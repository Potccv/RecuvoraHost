//! Recheck, consume once, execute, record uncertainty and check results by reading.
use super::*;

impl RepairSession {
    pub fn apply(
        &self,
        id: &str,
        cancellation: &HarnessCancellation,
    ) -> Result<ApprovalRecord, WorkflowError> {
        self.apply_versioned(id, None, cancellation)
    }

    pub fn apply_versioned(
        &self,
        id: &str,
        revision: Option<u64>,
        cancellation: &HarnessCancellation,
    ) -> Result<ApprovalRecord, WorkflowError> {
        check_revision(&*self.store()?, id, revision)?;
        let record = self.record(id)?;
        let action = &record.request.operation.action;
        if record.request.operation.target != self.config.target_id
            || action.get("target_root") != Some(&json!(self.files.root()))
            || action.get("kind").and_then(Value::as_str) != Some("replace_text")
        {
            return Err(WorkflowError::Invalid(
                "stored action belongs to a different target".into(),
            ));
        }
        let edit = decode_stored_edit(action)?;
        if cancellation.is_cancelled() {
            return Err(WorkflowError::Invalid(
                "execution canceled before dispatch".into(),
            ));
        }
        // Pin the exact target and validate its precondition before consuming.
        // Keep the short store lock through local write/receipt so concurrent
        // revocation cannot report success between consumption and dispatch.
        let prepared = match self.files.prepare(&edit) {
            Ok(prepared) => prepared,
            Err(error) => {
                if record.state == ApprovalState::Approved {
                    let mut store = self.store()?;
                    check_revision(&store, id, revision)?;
                    store.revoke(
                        id,
                        format!("execution precondition failed: {error}"),
                        now()?,
                    )?;
                }
                return Err(error.into());
            }
        };
        let mut store = self.store()?;
        check_revision(&store, id, revision)?;
        if cancellation.is_cancelled() {
            return Err(WorkflowError::Invalid(
                "execution canceled before dispatch".into(),
            ));
        }
        if store.list().iter().any(|r| {
            same_root(&r.request.operation, self.files.root())
                && matches!(r.state, ApprovalState::Executing | ApprovalState::Unknown)
        }) {
            return Err(ApprovalError::TargetBusy.into());
        }
        let permit = store.consume(id, &record.request.operation, &self.config.policy, now()?)?;
        if cancellation.is_cancelled() {
            return store
                .complete(
                    permit,
                    ExecutionOutcome::Failed,
                    "canceled after intent, before dispatch; no write performed".into(),
                    now()?,
                )
                .map_err(|error| WorkflowError::OutcomeUnknown {
                    request_id: id.into(),
                    message: error.to_string(),
                });
        }
        let result = prepared.execute();
        let (outcome, reason) = match result {
            Ok(receipt) => (ExecutionOutcome::Executed, serde_json::to_string(&receipt)?),
            Err(ActionError::Unknown(reason)) => (ExecutionOutcome::Unknown, reason),
            Err(error) => (ExecutionOutcome::Failed, error.to_string()),
        };
        // On a journal failure the durable Executing intent recovers as Unknown.
        let completed_at = now().map_err(|error| WorkflowError::OutcomeUnknown {
            request_id: id.into(),
            message: error.to_string(),
        })?;
        store
            .complete(permit, outcome, reason, completed_at)
            .map_err(|error| WorkflowError::OutcomeUnknown {
                request_id: id.into(),
                message: error.to_string(),
            })
    }

    /// Check results without repeating the write: only exact before/after contents
    /// can resolve an unknown outcome. Partial or unrelated content stays unknown.
    pub fn check_result(&self, id: &str) -> Result<ApprovalRecord, WorkflowError> {
        self.check_result_authenticated(id, None, "local-cli-operator")
    }

    pub fn check_result_authenticated(
        &self,
        id: &str,
        revision: Option<u64>,
        actor: &str,
    ) -> Result<ApprovalRecord, WorkflowError> {
        let mut store = self.store()?;
        check_revision(&store, id, revision)?;
        let record = store.get(id).cloned().ok_or(ApprovalError::NotFound)?;
        if record.request.operation.target != self.config.target_id
            || record.request.operation.action.get("target_root") != Some(&json!(self.files.root()))
        {
            return Err(WorkflowError::Invalid(
                "stored action belongs to another target".into(),
            ));
        }
        let edit = decode_stored_edit(&record.request.operation.action)?;
        let current = self.files.read(&edit.path)?;
        let outcome = if current == edit.replacement {
            ExecutionOutcome::Executed
        } else if current == edit.expected {
            ExecutionOutcome::Failed
        } else {
            return Err(WorkflowError::Invalid(
                "target matches neither approved baseline nor replacement; outcome remains unknown"
                    .into(),
            ));
        };
        Ok(store.reconcile_unknown(
            id,
            outcome,
            "explicit local result check against stored full text".into(),
            actor.into(),
            now()?,
        )?)
    }
}
