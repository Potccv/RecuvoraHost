//! Public recovery facade and ownership of supervised calls and aggregate lifetime.
use super::*;
use super::{aggregate::State, incident_gate::incident, platform::Platform};
use crate::persistence::{approval::ApprovalStoreConfig, knowledge::KnowledgeStoreConfig};
use crate::runtime::operation::{CallScope, Cancellation};
use recuvora_core::recovery::{
    approval::{ApprovalDecision, ApprovalRecord},
    engine::{RecoveryEngine, SessionCommand},
    knowledge::KnowledgeQuery,
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

/// Owns protected storage, trusted ports and supervised calls; Core owns transitions.
pub struct RecoveryService {
    pub(super) config: RecoveryConfig,
    pub(super) backend: Arc<dyn RepairBackend>,
    pub(super) clock: Arc<dyn RecoveryClock>,
    pub(super) state: Mutex<Option<State>>,
    active: Mutex<BTreeSet<String>>,
    calls: CallScope,
    accepting: AtomicBool,
    pub(super) incident_guard: OnceLock<Arc<dyn IncidentGuard>>,
    state_directory: PathBuf,
    ownership: Mutex<Option<Box<dyn TargetLease>>>,
}
impl RecoveryService {
    pub fn open(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
    ) -> Result<Arc<Self>, RecoveryError> {
        Self::open_with_clock_and_store_configs(
            dir,
            config,
            backend,
            Arc::new(SystemRecoveryClock),
            ApprovalStoreConfig::default(),
            KnowledgeStoreConfig::default(),
        )
    }
    pub fn open_with_store_configs(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
        approvals: ApprovalStoreConfig,
        knowledge: KnowledgeStoreConfig,
    ) -> Result<Arc<Self>, RecoveryError> {
        Self::open_with_clock_and_store_configs(
            dir,
            config,
            backend,
            Arc::new(SystemRecoveryClock),
            approvals,
            knowledge,
        )
    }
    pub fn open_with_clock(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
        clock: Arc<dyn RecoveryClock>,
    ) -> Result<Arc<Self>, RecoveryError> {
        Self::open_with_clock_and_store_configs(
            dir,
            config,
            backend,
            clock,
            ApprovalStoreConfig::default(),
            KnowledgeStoreConfig::default(),
        )
    }
    pub fn open_with_clock_and_store_configs(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
        clock: Arc<dyn RecoveryClock>,
        approval_config: ApprovalStoreConfig,
        knowledge_config: KnowledgeStoreConfig,
    ) -> Result<Arc<Self>, RecoveryError> {
        let root = dir.as_ref();
        let state = State::open(
            root,
            &config,
            backend.persistence_binding(),
            clock.as_ref(),
            approval_config,
            knowledge_config,
        )?;
        Ok(Arc::new(Self {
            config,
            backend,
            clock,
            state: Mutex::new(Some(state)),
            active: Mutex::new(BTreeSet::new()),
            calls: CallScope::default(),
            accepting: AtomicBool::new(true),
            incident_guard: OnceLock::new(),
            state_directory: root.to_path_buf(),
            ownership: Mutex::new(None),
        }))
    }
    pub fn config(&self) -> &RecoveryConfig {
        &self.config
    }
    pub fn bind_target_ownership(
        &self,
        authority: Arc<dyn TargetOwnership>,
    ) -> Result<(), RecoveryError> {
        self.accepting()?;
        let mut owner = lock(&self.ownership)?;
        if owner.is_some() {
            return Err(RecoveryError::Invalid(
                "target ownership already bound".into(),
            ));
        }
        let target = CanonicalTarget::new(self.config.target.target_id.clone())?;
        let lease = authority.acquire(&target, &self.state_directory)?;
        if lease.target() != &target
            || lease.recovery_directory() != self.state_directory.canonicalize()?
        {
            return Err(RecoveryError::Invalid(
                "ownership target or storage mismatch".into(),
            ));
        }
        lease.validate()?;
        *owner = Some(lease);
        Ok(())
    }
    pub fn bind_incident_guard(&self, guard: Arc<dyn IncidentGuard>) -> Result<(), RecoveryError> {
        self.accepting()?;
        self.incident_guard
            .set(guard)
            .map_err(|_| RecoveryError::Invalid("incident guard already bound".into()))
    }
    fn accepting(&self) -> Result<(), RecoveryError> {
        if self.accepting.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(RecoveryError::Stopped)
        }
    }
    pub(super) fn owner(&self) -> Result<(), RecoveryError> {
        lock(&self.ownership)?
            .as_ref()
            .ok_or_else(|| service("target ownership not bound"))?
            .validate()
    }
    pub(super) fn with_state<T>(
        &self,
        f: impl FnOnce(&mut State) -> Result<T, RecoveryError>,
    ) -> Result<T, RecoveryError> {
        let mut state = lock(&self.state)?;
        let state = state.as_mut().ok_or(RecoveryError::Stopped)?;
        state.root_lock.validate()?;
        if state.dispatch_pending {
            return Err(RecoveryError::Busy);
        }
        f(state)
    }
    fn read_state<T>(
        &self,
        f: impl FnOnce(&State) -> Result<T, RecoveryError>,
    ) -> Result<T, RecoveryError> {
        let state = lock(&self.state)?;
        f(state.as_ref().ok_or(RecoveryError::Stopped)?)
    }
    pub fn query(&self, id: &str) -> Result<Option<RecoveryTask>, RecoveryError> {
        self.read_state(|s| Ok(s.session.task(id).cloned()))
    }
    pub fn tasks(&self) -> Result<Vec<RecoveryTask>, RecoveryError> {
        self.read_state(|s| Ok(s.session.tasks().cloned().collect()))
    }
    pub fn experiences(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<Vec<recuvora_core::recovery::knowledge::RepairExperience>, RecoveryError> {
        self.read_state(|s| Ok(s.session.experiences(query)?))
    }

    /// Retry committed experience delivery even when every business task ended.
    /// This never reconstructs execution effects or changes operation identity.
    pub fn deliver_pending(&self) -> Result<(), RecoveryError> {
        self.accepting()?;
        self.owner()?;
        self.with_state(|s| {
            s.apply(SessionCommand::Deliver, self.clock.now_ms())?;
            Ok(())
        })
    }
    pub fn submit(&self, problem: ProblemContext) -> Result<RecoveryTask, RecoveryError> {
        self.accepting()?;
        self.owner()?;
        let guard = self
            .incident_guard
            .get()
            .ok_or_else(|| service("incident guard not bound"))?;
        let mut result = None;
        guard.with_current(&problem, &mut |current| {
            if result.is_some() {
                return Err(service("incident guard repeated callback"));
            }
            let incident = incident(&problem, current)?;
            result = Some(self.with_state(|s| {
                s.apply(
                    SessionCommand::Register {
                        problem: problem.clone(),
                        incident,
                    },
                    self.clock.now_ms(),
                )?;
                s.session
                    .tasks()
                    .find(|t| t.problem.incident_id == problem.incident_id)
                    .cloned()
                    .ok_or_else(|| service("registered task missing"))
            })?);
            Ok(())
        })?;
        result.ok_or_else(|| service("incident guard did not check facts"))
    }
    pub fn approval(&self, id: &str) -> Result<Option<ApprovalRecord>, RecoveryError> {
        self.read_state(|s| {
            s.task(id)?;
            Ok(s.session.approval(id).cloned())
        })
    }
    pub fn decide_human(
        &self,
        id: &str,
        revision: u64,
        decision: ApprovalDecision,
        actor: String,
        reason: String,
    ) -> Result<ApprovalRecord, RecoveryError> {
        self.accepting()?;
        self.owner()?;
        self.with_state(|s| {
            s.apply(
                SessionCommand::HumanDecision {
                    task_id: id.into(),
                    revision,
                    decision,
                    actor,
                    reason,
                },
                self.clock.now_ms(),
            )?;
            s.session
                .approval(id)
                .cloned()
                .ok_or_else(|| service("missing approval"))
        })
    }
    pub fn resume(&self, id: &str, revision: u64) -> Result<RecoveryTask, RecoveryError> {
        self.accepting()?;
        self.owner()?;
        self.with_state(|s| {
            s.apply(
                SessionCommand::Resume {
                    task_id: id.into(),
                    revision,
                },
                self.clock.now_ms(),
            )?;
            s.task(id)
        })
    }
    pub async fn advance(
        self: &Arc<Self>,
        id: &str,
        cancellation: Cancellation,
    ) -> Result<RecoveryTask, RecoveryError> {
        self.accepting()?;
        self.owner()?;
        let service = self.clone();
        let id = id.to_owned();
        let cancel = cancellation.clone();
        self.calls
            .run(cancellation, async move {
                if !lock(&service.active)?.insert(id.clone()) {
                    return Err(RecoveryError::Busy);
                }
                let _active = Active {
                    service: service.clone(),
                    id: id.clone(),
                };
                RecoveryEngine::advance(&Platform::new(service.clone(), cancel), &id)
                    .await
                    .map_err(Into::into)
            })
            .await
            .map_err(super::service)?
    }
    /// Retry only post-repair summaries and delivery; never redispatch repair work.
    pub fn summarize_pending(
        self: &Arc<Self>,
        cancellation: Cancellation,
    ) -> RecoveryFuture<'_, ()> {
        Box::pin(self.summarize_pending_with_retry(cancellation, false))
    }
    /// Explicit operator retry after the automatic three-attempt summary budget.
    pub fn retry_experiences(
        self: &Arc<Self>,
        cancellation: Cancellation,
    ) -> RecoveryFuture<'_, ()> {
        Box::pin(self.summarize_pending_with_retry(cancellation, true))
    }
    async fn summarize_pending_with_retry(
        self: &Arc<Self>,
        cancellation: Cancellation,
        explicit: bool,
    ) -> Result<(), RecoveryError> {
        self.accepting()?;
        let service = self.clone();
        let cancel = cancellation.clone();
        self.calls
            .run(cancellation, async move {
                RecoveryEngine::summarize_pending(&Platform::new(service, cancel), explicit)
                    .await
                    .map_err(RecoveryError::from)
            })
            .await
            .map_err(super::service)?
    }
    pub fn pending_experiences(
        &self,
    ) -> Result<Vec<recuvora_core::recovery::workflow::ExperienceJob>, RecoveryError> {
        self.read_state(|s| Ok(s.session.pending_experiences()))
    }
    pub fn check_result(
        &self,
        id: &str,
        revision: u64,
        execution: ExecutionResultCheck,
        verification: BusinessVerification,
        actor: String,
    ) -> Result<RecoveryTask, RecoveryError> {
        self.accepting()?;
        self.owner()?;
        self.with_state(|s| {
            s.apply(
                SessionCommand::CheckResult {
                    task_id: id.into(),
                    revision,
                    execution,
                    verification,
                    actor,
                },
                self.clock.now_ms(),
            )?;
            s.task(id)
        })
    }
    pub async fn shutdown(&self) -> Result<(), RecoveryError> {
        self.accepting.store(false, Ordering::Release);
        self.calls.shutdown().await.map_err(service)?;
        let mut state = lock(&self.state)?;
        if let Some(current) = state.as_mut() {
            current.root_lock.validate()?;
            // A cached terminal state cannot release ownership after an uncertain
            // write or external file change in any participating aggregate.
            current.journal.prepare_close().map_err(service)?;
            current.dispatch.prepare_close().map_err(service)?;
            let terminal = current.session.releasable();
            if terminal && let Some(lease) = lock(&self.ownership)?.as_mut() {
                lease.release()?;
            }
            current.journal.finish_close();
            current.dispatch.finish_close();
            state.take();
            lock(&self.ownership)?.take();
        }
        Ok(())
    }
}
impl Drop for RecoveryService {
    fn drop(&mut self) {
        self.accepting.store(false, Ordering::Release);
        let _ = self.calls.close();
    }
}

struct Active {
    service: Arc<RecoveryService>,
    id: String,
}
impl Drop for Active {
    fn drop(&mut self) {
        if let Ok(mut active) = self.service.active.lock() {
            active.remove(&self.id);
        }
    }
}

#[cfg(test)]
#[path = "../../tests/recovery_commits.rs"]
mod commit_tests;
