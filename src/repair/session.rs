//! Session initialization, queries and explicit trusted human decisions.
use super::*;

impl RepairSession {
    pub fn open(
        config: RepairConfig,
        registry: Option<Arc<HarnessRegistry>>,
        protected: &[PathBuf],
    ) -> Result<Arc<Self>, WorkflowError> {
        Self::open_with_store_config(config, registry, protected, ApprovalStoreConfig::default())
    }

    pub fn open_with_store_config(
        config: RepairConfig,
        registry: Option<Arc<HarnessRegistry>>,
        protected: &[PathBuf],
        store_config: ApprovalStoreConfig,
    ) -> Result<Arc<Self>, WorkflowError> {
        config.validate()?;
        store_config.validate()?;
        let files = ScopedFiles::open(&config.target_root, &config.allowed_files, protected)?;
        let data = config.data_dir.canonicalize()?;
        let reviewer = if config.reviewer_workspace.is_none() {
            Some(config.reviewer_directory.canonicalize()?)
        } else {
            None
        };
        if data.starts_with(files.root())
            || reviewer.is_some_and(|path| path.starts_with(files.root()))
        {
            return Err(WorkflowError::Invalid(
                "state and reviewer directories must be outside the target".into(),
            ));
        }
        let store = ApprovalStore::open(&data, store_config, now()?)?;
        Ok(Arc::new(Self {
            config,
            files,
            store: Mutex::new(store),
            registry,
        }))
    }

    pub fn records(&self) -> Result<Vec<ApprovalRecord>, WorkflowError> {
        Ok(self.store()?.list())
    }

    pub fn record(&self, id: &str) -> Result<ApprovalRecord, WorkflowError> {
        self.store()?
            .get(id)
            .cloned()
            .ok_or(ApprovalError::NotFound.into())
    }

    /// This endpoint is available to the local OS operator, never to the AI tools.
    /// It records a decision only; `apply` is an explicit separate execution.
    pub fn decide(
        &self,
        id: &str,
        decision: ApprovalDecision,
        reason: String,
    ) -> Result<ApprovalRecord, WorkflowError> {
        Ok(self.store()?.decide_human(
            id,
            decision,
            reason,
            "local-cli-operator".into(),
            &self.config.policy,
            now()?,
        )?)
    }

    pub fn revoke(&self, id: &str, reason: String) -> Result<ApprovalRecord, WorkflowError> {
        Ok(self.store()?.revoke(id, reason, now()?)?)
    }

    /// The transport authenticates this actor; model tools never expose this method.
    pub fn decide_authenticated(
        &self,
        id: &str,
        revision: u64,
        actor: &str,
        decision: ApprovalDecision,
        reason: String,
    ) -> Result<ApprovalRecord, WorkflowError> {
        let mut store = self.store()?;
        check_revision(&store, id, Some(revision))?;
        Ok(store.decide_human(
            id,
            decision,
            reason,
            actor.into(),
            &self.config.policy,
            now()?,
        )?)
    }

    pub fn revoke_authenticated(
        &self,
        id: &str,
        revision: u64,
        actor: &str,
        reason: String,
    ) -> Result<ApprovalRecord, WorkflowError> {
        let mut store = self.store()?;
        check_revision(&store, id, Some(revision))?;
        Ok(store.revoke(id, format!("{actor}: {reason}"), now()?)?)
    }
}
