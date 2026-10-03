//! Pure preparation, caller-confirmed effects, and validated history replay.
use super::*;

impl ApprovalLedger {
    pub fn new(config: ApprovalLimits) -> Result<Self, ApprovalError> {
        config.validate()?;
        Ok(Self {
            digest: crate::control::binding::digest(&("approval", &config)),
            commit_ids: im::OrdSet::new(),
            config,
            recovery_required: false,
            sequence: 0,
            records: crate::control::collections::Map::new(),
            history: im::Vector::new(),
            identity: Arc::new(()),
        })
    }

    /// The caller supplies complete, committed history. No call or execution is replayed.
    /// Executing records remain blocking until `RecoverUnknown` is committed.
    pub fn restore(
        config: ApprovalLimits,
        entries: impl AsRef<[ApprovalEntry]>,
    ) -> Result<Self, ApprovalError> {
        let entries = entries.as_ref();
        let mut ledger = Self::new(config)?;
        for entry in entries {
            ledger
                .install(entry.clone())
                .map_err(|error| ApprovalError::Corrupt(error.to_string()))?;
        }
        ledger.recovery_required = !entries.is_empty();
        Ok(ledger)
    }

    pub fn recovery_required(&self) -> bool {
        self.recovery_required
    }

    /// The caller confirms this aggregate recovery before any resumed authorization.
    pub fn prepare_recovery(
        &self,
        commit_id: String,
        now: u64,
    ) -> Result<Prepared<Self, ApprovalEffect>, ApprovalError> {
        self.prepare(commit_id, ApprovalEvent::Recover, None, now)
    }

    pub fn revision(&self) -> u64 {
        self.sequence
    }
    /// Copies the complete history for export; use latest_entry for incremental persistence.
    pub fn entries(&self) -> Vec<ApprovalEntry> {
        self.history
            .iter()
            .map(|entry| entry.as_ref().clone())
            .collect()
    }

    /// The newest validated entry, without copying the history.
    pub fn latest_entry(&self) -> Option<&ApprovalEntry> {
        self.history.back().map(AsRef::as_ref)
    }
    pub fn get(&self, id: &str) -> Option<&ApprovalRecord> {
        self.records.get(id)
    }
    pub fn list(&self) -> Vec<ApprovalRecord> {
        self.records.values().cloned().collect()
    }

    /// Validates without changing this aggregate. The caller must atomically compare
    /// the expected aggregate revision and persist before confirming the result.
    /// Use a unique caller transaction ID for every preparation, including retries.
    pub fn prepare(
        &self,
        commit_id: String,
        event: ApprovalEvent,
        current_policy: Option<&ApprovalPolicy>,
        now: u64,
    ) -> Result<Prepared<Self, ApprovalEffect>, ApprovalError> {
        if matches!(
            &event,
            ApprovalEvent::Changed {
                change: ApprovalChange::Complete { .. },
                ..
            }
        ) {
            return Err(ApprovalError::Invalid(
                "completion requires an owned execution permit",
            ));
        }
        self.prepare_internal(commit_id, event, current_policy, now)
    }

    pub(super) fn prepare_internal(
        &self,
        commit_id: String,
        event: ApprovalEvent,
        current_policy: Option<&ApprovalPolicy>,
        now: u64,
    ) -> Result<Prepared<Self, ApprovalEffect>, ApprovalError> {
        if self.recovery_required && !matches!(event, ApprovalEvent::Recover) {
            return Err(ApprovalError::RecoveryRequired);
        }
        if self.commit_ids.contains(&commit_id) {
            return Err(ApprovalError::Conflict);
        }
        let input = serde_json::json!({"config":self.config,"prior_digest":self.digest,"event":event,"current_policy":current_policy,"now":now,"recovery_required":self.recovery_required});
        if let ApprovalEvent::Changed { request_id, change } = &event {
            let record = self
                .records
                .get(request_id)
                .ok_or(ApprovalError::NotFound)?;
            if matches!(
                change,
                ApprovalChange::HumanDecision { .. }
                    | ApprovalChange::BeginReview { .. }
                    | ApprovalChange::AssessAttempt { .. }
                    | ApprovalChange::FailReview { .. }
                    | ApprovalChange::WaitingHuman { .. }
                    | ApprovalChange::Consume
            ) {
                let policy =
                    current_policy.ok_or(ApprovalError::Invalid("current policy required"))?;
                policy.validate()?;
                if policy != &record.request.policy {
                    return Err(ApprovalError::Conflict);
                }
            }
        }
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(ApprovalError::Capacity)?;
        let entry = ApprovalEntry {
            prior_digest: self.digest.clone(),
            commit_id: commit_id.clone(),
            sequence,
            now,
            event,
        };
        let mut proposed = self.clone();
        let record = proposed.install(entry.clone())?;
        let effects = match &entry.event {
            ApprovalEvent::Changed {
                change: ApprovalChange::Consume,
                ..
            } => {
                let record = record.ok_or(ApprovalError::Conflict)?;
                vec![ApprovalEffect::Execute(ExecutionPermit {
                    request_id: record.request.request_id.clone(),
                    revision: record.revision,
                    operation: record.request.operation.clone(),
                    store_identity: self.identity.clone(),
                })]
            }
            ApprovalEvent::Changed {
                change: ApprovalChange::BeginReview { .. },
                ..
            } => {
                vec![ApprovalEffect::Review(
                    record
                        .ok_or(ApprovalError::Conflict)?
                        .active_review_attempt()
                        .ok_or(ApprovalError::Conflict)?,
                )]
            }
            _ => Vec::new(),
        };
        Ok(Prepared::new_bound(
            commit_id,
            self.sequence,
            "approval".into(),
            input,
            proposed,
            effects,
        )?)
    }

    /// The operation identity is an idempotency key. A repeated exact request
    /// returns the original record through `find_operation`; conflicting reuse fails.
    pub fn prepare_request(
        &self,
        commit_id: String,
        operation: ProposedOperation,
        policy: ApprovalPolicy,
        now: u64,
    ) -> Result<Prepared<Self, ApprovalEffect>, ApprovalError> {
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(ApprovalError::Capacity)?;
        let expires_at = now
            .checked_add(policy.ttl_secs)
            .ok_or(ApprovalError::Invalid("expiry overflow"))?;
        self.prepare(
            commit_id,
            ApprovalEvent::Requested {
                request: ApprovalRequest {
                    request_id: format!("approval-{sequence:016x}"),
                    operation,
                    policy,
                    created_at: now,
                    expires_at,
                },
            },
            None,
            now,
        )
    }

    pub fn find_operation(&self, task_id: &str, operation_id: &str) -> Option<&ApprovalRecord> {
        self.records.values().find(|record| {
            record.request.operation.task_id == task_id
                && record.request.operation.operation_id == operation_id
        })
    }

    /// A capability consumed by this function cannot be reused. A failed or
    /// uncertain caller commit must be resolved through the persisted intent.
    pub fn prepare_complete(
        &self,
        commit_id: String,
        permit: ExecutionPermit,
        outcome: ExecutionOutcome,
        reason: String,
        now: u64,
    ) -> Result<Prepared<Self, ApprovalEffect>, ApprovalError> {
        let record = self
            .records
            .get(&permit.request_id)
            .ok_or(ApprovalError::NotFound)?;
        if !Arc::ptr_eq(&self.identity, &permit.store_identity)
            || record.revision != permit.revision
            || record.request.operation != permit.operation
        {
            return Err(ApprovalError::Conflict);
        }
        self.prepare_internal(
            commit_id,
            ApprovalEvent::Changed {
                request_id: permit.request_id,
                change: ApprovalChange::Complete { outcome, reason },
            },
            None,
            now,
        )
    }

    fn install(&mut self, entry: ApprovalEntry) -> Result<Option<ApprovalRecord>, ApprovalError> {
        if entry.prior_digest != self.digest {
            return Err(ApprovalError::Conflict);
        }
        if !crate::control::identity::valid_id(&entry.commit_id) {
            return Err(ApprovalError::Invalid("commit identity"));
        }
        if self.commit_ids.contains(&entry.commit_id)
            || self.sequence.checked_add(1) != Some(entry.sequence)
        {
            return Err(ApprovalError::Conflict);
        }
        let record = if matches!(entry.event, ApprovalEvent::Recover) {
            let mut recovered = Vec::new();
            for record in self.records.values() {
                if entry.now < record.updated_at {
                    return Err(ApprovalError::Invalid("clock moved backwards"));
                }
                let change = if record.state == ApprovalState::Executing {
                    Some(ApprovalChange::RecoverUnknown)
                } else if matches!(
                    record.state,
                    ApprovalState::Pending | ApprovalState::WaitingHuman | ApprovalState::Approved
                ) && entry.now >= record.request.expires_at
                {
                    Some(ApprovalChange::Expire)
                } else {
                    record
                        .active_review_attempt()
                        .map(|attempt| ApprovalChange::FailReview {
                            attempt,
                            reason: "Host restarted before independent review completed".into(),
                        })
                };
                if let Some(change) = change {
                    recovered.push(self.apply(
                        &ApprovalEvent::Changed {
                            request_id: record.request.request_id.clone(),
                            change,
                        },
                        entry.now,
                        entry.sequence,
                    )?);
                }
            }
            for record in recovered {
                self.records
                    .insert(record.request.request_id.clone(), record);
            }
            self.recovery_required = false;
            None
        } else {
            let record = self.apply(&entry.event, entry.now, entry.sequence)?;
            self.records
                .insert(record.request.request_id.clone(), record.clone());
            Some(record)
        };
        self.sequence = entry.sequence;
        self.digest = crate::control::binding::digest(&entry);
        self.commit_ids.insert(entry.commit_id.clone());
        self.history.push_back(std::sync::Arc::new(entry));
        Ok(record)
    }
}
