//! Pure validation of live and replayed approval state transitions.
use super::*;

impl ApprovalLedger {
    pub(super) fn apply(
        &self,
        event: &ApprovalEvent,
        now: u64,
        sequence: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let (id, change) = match event {
            ApprovalEvent::Recover => {
                return Err(ApprovalError::Invalid(
                    "recovery is an aggregate transition",
                ));
            }
            ApprovalEvent::Requested { request } => {
                request.operation.validate()?;
                request.policy.validate()?;
                if self.records.len() >= self.config.max_requests {
                    return Err(ApprovalError::Capacity);
                }
                if request.request_id != format!("approval-{sequence:016x}")
                    || request.created_at != now
                    || request.created_at.checked_add(request.policy.ttl_secs)
                        != Some(request.expires_at)
                    || self.records.values().any(|record| {
                        record.request.operation.task_id == request.operation.task_id
                            && record.request.operation.operation_id
                                == request.operation.operation_id
                    })
                {
                    return Err(ApprovalError::Conflict);
                }
                let (review_stage, human_deadline) = match &request.policy.reviewer {
                    ReviewerConfig::Human => (ReviewStage::NeedsHuman, None),
                    ReviewerConfig::Harness { .. } => (ReviewStage::ReadyHarness, None),
                    ReviewerConfig::HumanThenHarness {
                        human_wait_secs, ..
                    } => (
                        ReviewStage::WaitingHuman,
                        Some(
                            now.checked_add(*human_wait_secs)
                                .ok_or(ApprovalError::Invalid("human deadline overflow"))?,
                        ),
                    ),
                };
                return Ok(ApprovalRecord {
                    request: request.clone(),
                    state: if human_deadline.is_some() {
                        ApprovalState::WaitingHuman
                    } else {
                        ApprovalState::Pending
                    },
                    revision: 0,
                    updated_at: now,
                    assessment: None,
                    note: None,
                    review_stage,
                    human_deadline,
                    review_deadline: None,
                    review_attempt: 0,
                });
            }
            ApprovalEvent::Changed { request_id, change } => (request_id, change),
        };
        let mut record = self.records.get(id).ok_or(ApprovalError::NotFound)?.clone();
        if now < record.updated_at {
            return Err(ApprovalError::Invalid("clock moved backwards"));
        }
        let state = record.state;
        match change {
            ApprovalChange::HumanDecision {
                expected_revision,
                assessment,
            } => {
                if record.revision != *expected_revision {
                    return Err(ApprovalError::Conflict);
                }
                require_state(
                    state,
                    &[ApprovalState::Pending, ApprovalState::WaitingHuman],
                )?;
                fresh(&record, now)?;
                validate_assessment(assessment)?;
                if !matches!(assessment.reviewer, AssessmentSource::Human { .. }) {
                    return Err(ApprovalError::Invalid("human reviewer required"));
                }
                record.state = decision_state(&record, assessment.decision)?;
                record.assessment = Some(assessment.clone());
                finish_review(&mut record);
            }
            ApprovalChange::BeginReview {
                expected_revision,
                timeout_secs,
            } => {
                require_state(
                    state,
                    &[ApprovalState::Pending, ApprovalState::WaitingHuman],
                )?;
                fresh(&record, now)?;
                if record.revision != *expected_revision || !(1..=1800).contains(timeout_secs) {
                    return Err(ApprovalError::Conflict);
                }
                match (&record.request.policy.reviewer, record.review_stage) {
                    (ReviewerConfig::Harness { .. }, ReviewStage::ReadyHarness) => {}
                    (
                        ReviewerConfig::HumanThenHarness {
                            review_timeout_secs,
                            ..
                        },
                        ReviewStage::WaitingHuman,
                    ) => {
                        if *timeout_secs > *review_timeout_secs {
                            return Err(ApprovalError::Invalid("review timeout exceeds policy"));
                        }
                        if record.human_deadline.is_none_or(|deadline| now < deadline) {
                            return Err(ApprovalError::ReviewNotDue);
                        }
                    }
                    _ => return Err(ApprovalError::Conflict),
                }
                record.review_deadline = Some(
                    now.checked_add(*timeout_secs)
                        .ok_or(ApprovalError::Invalid("review deadline overflow"))?
                        .min(record.request.expires_at),
                );
                record.review_attempt = record
                    .review_attempt
                    .checked_add(1)
                    .ok_or(ApprovalError::Capacity)?;
                record.review_stage = ReviewStage::ReviewingHarness;
                record.state = ApprovalState::Pending;
                record.note = Some("independent harness review started".into());
            }
            ApprovalChange::AssessAttempt {
                attempt,
                assessment,
            } => {
                validate_attempt(&record, attempt)?;
                fresh(&record, now)?;
                if now >= attempt.deadline {
                    return Err(ApprovalError::ReviewTimedOut);
                }
                validate_assessment(assessment)?;
                if !matches!(&assessment.reviewer, AssessmentSource::Harness { harness_id, .. } if harness_id == &attempt.harness_id)
                {
                    return Err(ApprovalError::Conflict);
                }
                record.state = decision_state(&record, assessment.decision)?;
                record.assessment = Some(assessment.clone());
                finish_review(&mut record);
            }
            ApprovalChange::FailReview { attempt, reason } => {
                validate_attempt(&record, attempt)?;
                fresh(&record, now)?;
                text(reason, MAX_REASON)?;
                record.state = ApprovalState::WaitingHuman;
                record.review_stage = ReviewStage::NeedsHuman;
                record.note = Some(reason.clone());
            }
            ApprovalChange::Revoke { reason } | ApprovalChange::Cancel { reason } => {
                text(reason, MAX_REASON)?;
                require_state(
                    state,
                    &[
                        ApprovalState::Pending,
                        ApprovalState::WaitingHuman,
                        ApprovalState::Approved,
                        ApprovalState::Executing,
                    ],
                )?;
                record.state = if state == ApprovalState::Executing {
                    ApprovalState::Unknown
                } else if matches!(change, ApprovalChange::Revoke { .. }) {
                    ApprovalState::Revoked
                } else {
                    ApprovalState::Canceled
                };
                record.note = Some(reason.clone());
                record.review_stage = ReviewStage::Finished;
            }
            ApprovalChange::WaitingHuman { reason } => {
                require_state(state, &[ApprovalState::Pending])?;
                fresh(&record, now)?;
                text(reason, MAX_REASON)?;
                record.state = ApprovalState::WaitingHuman;
                record.note = Some(reason.clone());
                record.review_stage = ReviewStage::NeedsHuman;
            }
            ApprovalChange::Expire => {
                require_state(
                    state,
                    &[
                        ApprovalState::Pending,
                        ApprovalState::WaitingHuman,
                        ApprovalState::Approved,
                    ],
                )?;
                if now < record.request.expires_at {
                    return Err(ApprovalError::Invalid("premature expiry"));
                }
                record.state = ApprovalState::Expired;
                record.review_stage = ReviewStage::Finished;
            }
            ApprovalChange::Consume => {
                require_state(state, &[ApprovalState::Approved])?;
                fresh(&record, now)?;
                if !record.request.policy.allows(&record.request.operation) {
                    return Err(ApprovalError::OutOfScope);
                }
                if self.records.values().any(|other| {
                    other.request.operation.target == record.request.operation.target
                        && matches!(
                            other.state,
                            ApprovalState::Executing | ApprovalState::Unknown
                        )
                }) {
                    return Err(ApprovalError::TargetBusy);
                }
                record.state = ApprovalState::Executing;
            }
            ApprovalChange::Complete { outcome, reason } => {
                require_state(state, &[ApprovalState::Executing])?;
                text(reason, MAX_REASON)?;
                record.state = outcome_state(*outcome);
                record.note = Some(reason.clone());
            }
            ApprovalChange::RecoverUnknown => {
                require_state(state, &[ApprovalState::Executing])?;
                record.state = ApprovalState::Unknown;
                record.note = Some("host restarted with an unconfirmed execution intent".into());
            }
            ApprovalChange::Reconcile {
                outcome,
                reason,
                actor,
            } => {
                require_state(state, &[ApprovalState::Unknown])?;
                if *outcome == ExecutionOutcome::Unknown {
                    return Err(ApprovalError::Invalid(
                        "reconciliation requires a verified terminal outcome",
                    ));
                }
                text(reason, MAX_REASON)?;
                text(actor, MAX_ID)?;
                record.state = outcome_state(*outcome);
                record.note = Some(format!("verified by {actor}: {reason}"));
            }
        }
        record.revision = record
            .revision
            .checked_add(1)
            .ok_or(ApprovalError::Capacity)?;
        record.updated_at = now;
        Ok(record)
    }
}

fn decision_state(
    record: &ApprovalRecord,
    decision: ApprovalDecision,
) -> Result<ApprovalState, ApprovalError> {
    match decision {
        ApprovalDecision::Approve if !record.request.policy.allows(&record.request.operation) => {
            Err(ApprovalError::OutOfScope)
        }
        ApprovalDecision::Approve => Ok(ApprovalState::Approved),
        ApprovalDecision::Deny => Ok(ApprovalState::Denied),
        ApprovalDecision::Escalate => Ok(ApprovalState::WaitingHuman),
    }
}

fn finish_review(record: &mut ApprovalRecord) {
    record.review_stage = if record.state == ApprovalState::WaitingHuman {
        ReviewStage::NeedsHuman
    } else {
        ReviewStage::Finished
    };
}

pub(super) fn validate_attempt(
    record: &ApprovalRecord,
    attempt: &ReviewAttempt,
) -> Result<(), ApprovalError> {
    require_state(record.state, &[ApprovalState::Pending])?;
    let harness = match &record.request.policy.reviewer {
        ReviewerConfig::Harness { harness_id }
        | ReviewerConfig::HumanThenHarness { harness_id, .. } => harness_id,
        ReviewerConfig::Human => return Err(ApprovalError::Conflict),
    };
    if record.request.request_id != attempt.request_id
        || record.revision != attempt.revision
        || record.review_attempt != attempt.attempt
        || record.review_stage != ReviewStage::ReviewingHarness
        || record.review_deadline != Some(attempt.deadline)
        || harness != &attempt.harness_id
    {
        return Err(ApprovalError::Conflict);
    }
    Ok(())
}

fn validate_assessment(assessment: &ApprovalAssessment) -> Result<(), ApprovalError> {
    text(&assessment.reason, MAX_REASON)?;
    match &assessment.reviewer {
        AssessmentSource::Harness {
            harness_id,
            session_id,
        } => {
            text(harness_id, MAX_ID)?;
            text(session_id, MAX_ID)
        }
        AssessmentSource::Human { actor } => text(actor, MAX_ID),
    }
}

fn fresh(record: &ApprovalRecord, now: u64) -> Result<(), ApprovalError> {
    if now >= record.request.expires_at {
        Err(ApprovalError::Expired)
    } else {
        Ok(())
    }
}

fn require_state(state: ApprovalState, allowed: &[ApprovalState]) -> Result<(), ApprovalError> {
    if allowed.contains(&state) {
        Ok(())
    } else {
        Err(ApprovalError::InvalidState(state))
    }
}

fn outcome_state(outcome: ExecutionOutcome) -> ApprovalState {
    match outcome {
        ExecutionOutcome::Executed => ApprovalState::Executed,
        ExecutionOutcome::Failed => ApprovalState::Failed,
        ExecutionOutcome::Unknown => ApprovalState::Unknown,
    }
}
