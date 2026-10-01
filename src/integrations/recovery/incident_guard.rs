//! Adapter from Host monitoring facts to the Core execution guard.
use super::IncidentTrigger;
use crate::integrations::recovery::{
    IncidentGuard, IncidentReadiness, ProblemContext, RecoveryError,
};
use crate::monitoring::MonitorHandle;
use recuvora_core::recovery::incidents::{IncidentKind, IncidentStatus, SignalCondition};
use std::collections::BTreeSet;

/// Connects Core workflow preflight to Host's atomic, fresh monitor view.
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

fn service(error: impl std::fmt::Display) -> RecoveryError {
    RecoveryError::Service(error.to_string())
}
