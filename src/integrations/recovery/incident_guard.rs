//! Host gate binding authoritative monitor facts to recovery dispatch.
use super::IncidentTrigger;
use crate::control::recovery::incidents::{IncidentKind, IncidentStatus, SignalCondition};
use crate::integrations::recovery::{
    IncidentDispatchLease, IncidentGuard, IncidentReadiness, ProblemContext, RecoveryError,
    RecoveryFuture,
};
use crate::monitoring::{MonitorHandle, MonitorIncidentLease};
use std::collections::BTreeSet;

/// Supplies control workflow evidence under Host's atomic, fresh monitor gate.
pub struct MonitorIncidentGuard {
    monitor: MonitorHandle,
    triggers: Vec<IncidentTrigger>,
}

impl MonitorIncidentGuard {
    pub fn new(
        monitor: MonitorHandle,
        triggers: Vec<IncidentTrigger>,
    ) -> Result<Self, RecoveryError> {
        if triggers.is_empty() || triggers.len() > 64 {
            return Err(RecoveryError::Invalid(
                "incident guard requires 1..64 triggers".into(),
            ));
        }
        let mut bindings = BTreeSet::new();
        for trigger in &triggers {
            trigger.validate()?;
            if !bindings.insert((&trigger.monitor_id, &trigger.rule_id)) {
                return Err(RecoveryError::Invalid(
                    "duplicate incident guard binding".into(),
                ));
            }
        }
        Ok(Self { monitor, triggers })
    }
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
                triggers: self.triggers.clone(),
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
                |record| {
                    if record.id != problem.incident_id
                        || record.target_id != problem.target_id
                        || record.kind != IncidentKind::Target
                        || record.revision < problem.incident_revision
                        || !self.triggers.iter().any(|trigger| {
                            trigger.monitor_id == record.monitor_id
                                && trigger.rule_id == record.rule_id
                                && trigger.fingerprint == problem.fingerprint
                                && trigger.conditions == problem.conditions
                                && trigger.keywords == problem.keywords
                        })
                    {
                        return Err(RecoveryError::Invalid(
                            "incident no longer matches its trusted trigger".into(),
                        ));
                    }
                    if record.status == IncidentStatus::Resolved
                        && record.condition == SignalCondition::Clear
                    {
                        commit(IncidentReadiness::Resolved {
                            revision: record.revision,
                        })
                    } else if record.condition == SignalCondition::Active
                        && record.status != IncidentStatus::Resolved
                    {
                        commit(IncidentReadiness::Active {
                            revision: record.revision,
                        })
                    } else {
                        commit(IncidentReadiness::Unavailable {
                            reason: "incident has no current active evidence".into(),
                        })
                    }
                },
            )
            .map_err(service)?
    }
}

struct GuardLease {
    lease: MonitorIncidentLease,
    problem: ProblemContext,
    triggers: Vec<IncidentTrigger>,
}
impl IncidentDispatchLease for GuardLease {
    fn current(&self) -> Result<IncidentReadiness, RecoveryError> {
        let record = self.lease.current().map_err(service)?;
        if !self.triggers.iter().any(|trigger| {
            trigger.monitor_id == record.monitor_id
                && trigger.rule_id == record.rule_id
                && trigger.fingerprint == self.problem.fingerprint
                && trigger.conditions == self.problem.conditions
                && trigger.keywords == self.problem.keywords
        }) {
            return Err(RecoveryError::Invalid(
                "incident no longer matches its trusted trigger".into(),
            ));
        }
        if record.status == IncidentStatus::Resolved && record.condition == SignalCondition::Clear {
            Ok(IncidentReadiness::Resolved {
                revision: record.revision,
            })
        } else if record.condition == SignalCondition::Active
            && record.status != IncidentStatus::Resolved
        {
            Ok(IncidentReadiness::Active {
                revision: record.revision,
            })
        } else {
            Ok(IncidentReadiness::Unavailable {
                reason: "incident has no current active evidence".into(),
            })
        }
    }
}

fn service(error: impl std::fmt::Display) -> RecoveryError {
    RecoveryError::Service(error.to_string())
}
