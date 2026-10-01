//! Recovery management implemented by Core, exposed through Host service names.
use super::{
    BusinessVerification, ExecutionResultCheck, IncidentGuard, ProblemContext, RecoveryClock,
    RecoveryConfig, RecoveryError, RecoveryTask, RepairBackend, TargetOwnership,
};
use recuvora_core::operation::Cancellation;
use recuvora_core::recovery::approval::{ApprovalDecision, ApprovalRecord, ApprovalStoreConfig};
use recuvora_core::recovery::knowledge::{KnowledgeQuery, KnowledgeRecord, KnowledgeStoreConfig};
use recuvora_core::recovery::workflow::RecoveryService as CoreRecoveryService;
use std::{path::Path, sync::Arc};

pub struct RecoveryService {
    core: Arc<CoreRecoveryService>,
}

impl RecoveryService {
    pub fn open(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
    ) -> Result<Arc<Self>, RecoveryError> {
        Ok(Arc::new(Self {
            core: CoreRecoveryService::open(dir, config, backend)?,
        }))
    }

    pub fn open_with_store_configs(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
        approval_store: ApprovalStoreConfig,
        knowledge_store: KnowledgeStoreConfig,
    ) -> Result<Arc<Self>, RecoveryError> {
        Ok(Arc::new(Self {
            core: CoreRecoveryService::open_with_store_configs(
                dir,
                config,
                backend,
                approval_store,
                knowledge_store,
            )?,
        }))
    }

    pub fn open_with_clock(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
        clock: Arc<dyn RecoveryClock>,
    ) -> Result<Arc<Self>, RecoveryError> {
        Ok(Arc::new(Self {
            core: CoreRecoveryService::open_with_clock(dir, config, backend, clock)?,
        }))
    }

    pub fn open_with_clock_and_store_configs(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
        clock: Arc<dyn RecoveryClock>,
        approval_store: ApprovalStoreConfig,
        knowledge_store: KnowledgeStoreConfig,
    ) -> Result<Arc<Self>, RecoveryError> {
        Ok(Arc::new(Self {
            core: CoreRecoveryService::open_with_clock_and_store_configs(
                dir,
                config,
                backend,
                clock,
                approval_store,
                knowledge_store,
            )?,
        }))
    }

    pub fn config(&self) -> &RecoveryConfig {
        self.core.config()
    }

    /// All stores protecting the same canonical target must share this authority.
    /// Opening a service without binding ownership permits queries, not dispatch.
    pub fn bind_target_ownership(
        &self,
        authority: Arc<dyn TargetOwnership>,
    ) -> Result<(), RecoveryError> {
        self.core.bind_target_ownership(authority)
    }

    pub fn bind_incident_guard(&self, guard: Arc<dyn IncidentGuard>) -> Result<(), RecoveryError> {
        self.core.bind_incident_guard(guard)
    }

    pub fn query(&self, id: &str) -> Result<Option<RecoveryTask>, RecoveryError> {
        self.core.query(id)
    }

    pub fn tasks(&self) -> Result<Vec<RecoveryTask>, RecoveryError> {
        self.core.tasks()
    }

    pub fn knowledge(&self, query: &KnowledgeQuery) -> Result<Vec<KnowledgeRecord>, RecoveryError> {
        self.core.knowledge(query)
    }

    pub fn submit(&self, problem: ProblemContext) -> Result<RecoveryTask, RecoveryError> {
        self.core.submit(problem)
    }

    pub fn approval(&self, id: &str) -> Result<Option<ApprovalRecord>, RecoveryError> {
        self.core.approval(id)
    }

    pub fn decide_human(
        &self,
        id: &str,
        revision: u64,
        decision: ApprovalDecision,
        actor: String,
        reason: String,
    ) -> Result<ApprovalRecord, RecoveryError> {
        self.core
            .decide_human(id, revision, decision, actor, reason)
    }

    pub fn resume(&self, id: &str, revision: u64) -> Result<RecoveryTask, RecoveryError> {
        self.core.resume(id, revision)
    }

    pub async fn advance(
        &self,
        id: &str,
        cancellation: Cancellation,
    ) -> Result<RecoveryTask, RecoveryError> {
        self.core.advance(id, cancellation).await
    }

    pub fn check_result(
        &self,
        id: &str,
        revision: u64,
        execution: ExecutionResultCheck,
        verification: BusinessVerification,
        actor: String,
    ) -> Result<RecoveryTask, RecoveryError> {
        self.core
            .check_result(id, revision, execution, verification, actor)
    }

    pub async fn shutdown(&self) -> Result<(), RecoveryError> {
        self.core.shutdown().await
    }
}
