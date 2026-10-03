//! Bind protected immutable error receipts to Core intake and actual dispatch.
use crate::control::recovery::incidents::{IncidentKind, IncidentRecord};
use crate::monitoring::{MonitorHandle, MonitorIncidentLease, NodeErrorLog};
use crate::recovery::{
    IncidentDispatchLease, IncidentGuard, IncidentReadiness, ProblemContext, ProblemOrigin,
    RecoveryError, RecoveryFuture,
};
use recuvora_core::recovery::workflow::ErrorLogEvidence;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Supplies receipt evidence, not a judgment about current target health.
pub struct MonitorIncidentGuard {
    monitor: MonitorHandle,
}
impl MonitorIncidentGuard {
    pub fn new(monitor: MonitorHandle) -> Self {
        Self { monitor }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceivedError {
    kind: String,
    extension_id: String,
    source_id: String,
    generation: String,
    log: NodeErrorLog,
}

/// Preserve the original log and source evidence in Core. Conditions come only
/// from trusted target configuration; no host rule interprets the log message.
pub(super) fn problem_from_record(
    record: &IncidentRecord,
    conditions: BTreeMap<String, String>,
) -> Result<ProblemContext, RecoveryError> {
    if record.kind != IncidentKind::ErrorLog {
        return Err(RecoveryError::Invalid("error log receipt required".into()));
    }
    let received: ReceivedError = serde_json::from_value(record.evidence.clone())?;
    if received.kind != "node_error" || received.log.message != record.summary {
        return Err(RecoveryError::Invalid(
            "error log receipt content mismatch".into(),
        ));
    }
    let problem = ProblemContext {
        origin: ProblemOrigin::ErrorLog,
        report: Some(ErrorLogEvidence {
            source_id: received.source_id,
            generation: received.generation,
            record_id: received.log.id,
            sequence: received.log.sequence,
            age_ms: received.log.age_ms,
            evidence: received.log.evidence,
        }),
        incident_id: record.id.clone(),
        incident_revision: record.revision,
        target_id: record.target_id.clone(),
        fingerprint: received.log.fingerprint,
        summary: received.log.message,
        occurrences: record.occurrences,
        keywords: Vec::new(),
        conditions,
        evidence_refs: vec![
            format!("node-error:{}", received.extension_id),
            format!("error-receipt:{}", record.id),
        ],
    };
    problem.validate().map_err(service)?;
    Ok(problem)
}

fn receipt_readiness(
    record: &IncidentRecord,
    problem: &ProblemContext,
) -> Result<IncidentReadiness, RecoveryError> {
    if record.id != problem.incident_id
        || record.target_id != problem.target_id
        || record.revision < problem.incident_revision
        || problem.origin != ProblemOrigin::ErrorLog
    {
        return Err(RecoveryError::Invalid(
            "error receipt identity or revision mismatch".into(),
        ));
    }
    let mut expected = problem_from_record(record, problem.conditions.clone())?;
    // An acknowledgement may advance receipt revision without changing its raw
    // content, original source identity or the accepted Core problem binding.
    expected.incident_revision = problem.incident_revision;
    if expected != *problem {
        return Err(RecoveryError::Invalid(
            "error report differs from its durable receipt".into(),
        ));
    }
    Ok(IncidentReadiness::Received {
        revision: record.revision,
    })
}

impl IncidentGuard for MonitorIncidentGuard {
    fn acquire_dispatch<'a>(
        &'a self,
        problem: &'a ProblemContext,
    ) -> RecoveryFuture<'a, Box<dyn IncidentDispatchLease>> {
        Box::pin(async move {
            let lease = self
                .monitor
                .acquire_repair_incident(
                    &problem.incident_id,
                    &problem.target_id,
                    problem.incident_revision,
                )
                .await
                .map_err(service)?;
            let lease = GuardLease {
                lease,
                problem: problem.clone(),
            };
            lease.current()?;
            Ok(Box::new(lease) as Box<dyn IncidentDispatchLease>)
        })
    }
    fn with_current(
        &self,
        problem: &ProblemContext,
        commit: &mut dyn FnMut(IncidentReadiness) -> Result<(), RecoveryError>,
    ) -> Result<(), RecoveryError> {
        self.monitor
            .with_repair_incident(
                &problem.incident_id,
                &problem.target_id,
                problem.incident_revision,
                |record| commit(receipt_readiness(&record, problem)?),
            )
            .map_err(service)?
    }
}

struct GuardLease {
    lease: MonitorIncidentLease,
    problem: ProblemContext,
}
impl IncidentDispatchLease for GuardLease {
    fn current(&self) -> Result<IncidentReadiness, RecoveryError> {
        receipt_readiness(&self.lease.current().map_err(service)?, &self.problem)
    }
}
fn service(error: impl std::fmt::Display) -> RecoveryError {
    RecoveryError::Service(error.to_string())
}
