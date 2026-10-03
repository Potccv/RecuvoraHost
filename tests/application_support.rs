//! White-box setup for existing HTTP projection and recovery contract tests.
use super::*;

impl Application {
    pub(crate) fn set_recovery_for_test(&mut self, recovery: Arc<RecoveryService>) {
        self.recovery = Some(recovery);
    }
    pub(crate) fn set_text_repair_config_for_test(&mut self, config: RepairConfig) {
        self.text_repair_config = Some(config);
    }
}
