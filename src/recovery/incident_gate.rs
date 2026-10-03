//! Trusted origin-specific evidence checks before a workflow consumes authority.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IncidentReadiness {
    Active {
        revision: u64,
    },
    /// Immutable source error receipt is bound and available; no health claim.
    Received {
        revision: u64,
    },
    Resolved {
        revision: u64,
    },
    Unavailable {
        reason: String,
    },
}

/// Holds the source mutation gate until the executor dispatch boundary.
/// `current` rechecks the applicable origin binding and running instance while held.
pub trait IncidentDispatchLease: Send + Sync {
    fn current(&self) -> Result<IncidentReadiness, RecoveryError>;
}

/// Trusted host integration, never a model tool or an assertion from log text.
/// Check the matching origin: current incident facts, or an immutable error
/// receipt bound to the accepted problem. Receipt is not a target health claim.
/// Acknowledgements may advance revision without changing the received evidence.
/// Synchronous registration checks hold the source gate through `commit`.
/// Execution additionally requires an owned `acquire_dispatch` lease, held from
/// authorization through the network send and rechecked after the handshake.
/// Do not reenter that source from a synchronous callback.
pub trait IncidentGuard: Send + Sync {
    fn acquire_dispatch<'a>(
        &'a self,
        _problem: &'a ProblemContext,
    ) -> RecoveryFuture<'a, Box<dyn IncidentDispatchLease>> {
        Box::pin(async {
            Err(service(
                "incident guard must provide an owned dispatch lease",
            ))
        })
    }
    /// Call `commit` exactly once with current facts, or fail without calling it.
    fn with_current(
        &self,
        problem: &ProblemContext,
        commit: &mut dyn FnMut(IncidentReadiness) -> Result<(), RecoveryError>,
    ) -> Result<(), RecoveryError>;

    /// Read a snapshot; final authorization uses `with_current` instead.
    fn check(&self, problem: &ProblemContext) -> Result<IncidentReadiness, RecoveryError> {
        let mut readiness = None;
        self.with_current(problem, &mut |current| {
            if readiness.is_some() {
                return Err(service("incident guard repeated its callback"));
            }
            readiness = Some(current);
            Ok(())
        })?;
        readiness.ok_or_else(|| service("incident guard did not check current facts"))
    }
}

pub(super) fn incident(
    problem: &ProblemContext,
    current: IncidentReadiness,
) -> Result<recuvora_core::recovery::workflow::IncidentEvidence, RecoveryError> {
    match current {
        IncidentReadiness::Active { revision }
            if problem.origin == ProblemOrigin::Incident
                && revision >= problem.incident_revision =>
        {
            Ok(recuvora_core::recovery::workflow::IncidentEvidence {
                incident_id: problem.incident_id.clone(),
                revision,
                active: true,
                received: false,
            })
        }
        IncidentReadiness::Received { revision }
            if problem.origin == ProblemOrigin::ErrorLog
                && revision >= problem.incident_revision =>
        {
            Ok(recuvora_core::recovery::workflow::IncidentEvidence {
                incident_id: problem.incident_id.clone(),
                revision,
                active: false,
                received: true,
            })
        }
        IncidentReadiness::Resolved { .. } => Err(RecoveryError::Invalid(
            "incident resolved before dispatch".into(),
        )),
        IncidentReadiness::Unavailable { reason } => Err(service(reason)),
        _ => Err(RecoveryError::Conflict),
    }
}
