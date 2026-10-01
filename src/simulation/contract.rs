//! Closed simulation input, state and capacity contracts.
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

pub(super) const MAX_TEXT_BYTES: usize = 128;
const MAX_TIMEOUT_MS: u64 = 3_600_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Simulation {
    Succeed,
    Fail,
    Hang,
    /// Simulates a missing executor receipt; never terminates the host process.
    Exit,
    VerificationFailed,
    Delay {
        millis: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub id: String,
    pub target: String,
    pub simulation: Simulation,
    /// Only authorizes the closed, side-effect-free Simulation variants above.
    pub simulation_authorized: bool,
    pub timeout_ms: u64,
}

impl TaskSpec {
    pub fn simulated(
        id: impl Into<String>,
        target: impl Into<String>,
        simulation: Simulation,
    ) -> Self {
        Self {
            id: id.into(),
            target: target.into(),
            simulation,
            simulation_authorized: false,
            timeout_ms: 30_000,
        }
    }

    /// This is NOT identity verification or authorization for any real action.
    pub fn authorize_simulation(mut self) -> Self {
        self.simulation_authorized = true;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        self
    }

    pub(super) fn validate(&self) -> Result<(), EngineError> {
        for value in [&self.id, &self.target] {
            if value.is_empty()
                || value.len() > MAX_TEXT_BYTES
                || value.chars().any(char::is_control)
            {
                return Err(EngineError::Invalid(
                    "ID and target must contain 1..128 printable bytes",
                ));
            }
        }
        if self.timeout_ms == 0 || self.timeout_ms > MAX_TIMEOUT_MS {
            return Err(EngineError::Invalid(
                "timeout must be between 1 ms and one hour",
            ));
        }
        if matches!(self.simulation, Simulation::Delay { millis } if millis > MAX_TIMEOUT_MS) {
            return Err(EngineError::Invalid("simulation delay exceeds one hour"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    Queued,
    Diagnosing,
    Executing,
    Verifying,
    Succeeded,
    Failed,
    Denied,
    Canceled,
    TimedOut,
    Unknown,
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        !matches!(
            self,
            Self::Queued | Self::Diagnosing | Self::Executing | Self::Verifying
        )
    }

    pub(super) fn interrupted(self, timeout: bool) -> Self {
        match self {
            Self::Executing | Self::Verifying => Self::Unknown,
            _ if timeout => Self::TimedOut,
            _ => Self::Canceled,
        }
    }

    pub(super) fn allows(self, next: Self) -> bool {
        match self {
            Self::Queued => matches!(next, Self::Diagnosing | Self::Canceled),
            Self::Diagnosing => matches!(
                next,
                Self::Executing | Self::Canceled | Self::TimedOut | Self::Failed
            ),
            Self::Executing => matches!(next, Self::Verifying | Self::Failed | Self::Unknown),
            Self::Verifying => matches!(next, Self::Succeeded | Self::Failed | Self::Unknown),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub spec: TaskSpec,
    pub state: TaskState,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineConfig {
    pub max_concurrency: usize,
    pub max_queued: usize,
    pub max_tasks: usize,
    pub max_journal_bytes: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_concurrency: 4,
            max_queued: 64,
            max_tasks: 10_000,
            max_journal_bytes: 16 * 1024 * 1024,
        }
    }
}

impl EngineConfig {
    pub fn validate(&self) -> Result<(), EngineError> {
        if !(1..=256).contains(&self.max_concurrency)
            || !(1..=100_000).contains(&self.max_queued)
            || !(1..=1_000_000).contains(&self.max_tasks)
            || !(128..=1024 * 1024 * 1024).contains(&self.max_journal_bytes)
        {
            return Err(EngineError::Invalid("invalid engine capacity limits"));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("invalid configuration or task: {0}")]
    Invalid(&'static str),
    #[error("task capacity or submission queue is full")]
    Capacity,
    #[error("task ID already exists with different content")]
    Conflict,
    #[error("engine is stopped or its durable store is unavailable")]
    Unavailable,
    #[error("another engine owns the runtime directory: {0}")]
    Locked(std::io::Error),
    #[error("journal error: {0}")]
    Storage(#[from] std::io::Error),
    #[error("invalid journal: {0}")]
    Corrupt(String),
    #[error("journal size limit reached")]
    JournalFull,
    #[error("internal task failed: {0}")]
    Internal(String),
}
