//! Closed simulation of scheduling, cancellation and conservative recovery.
//! Its authorization is never valid for external actions.
mod actor;
mod contract;
mod engine;
mod journal;
mod worker;

pub use contract::{EngineConfig, EngineError, Simulation, TaskSnapshot, TaskSpec, TaskState};
pub use engine::{Engine, EngineHandle};
use tokio::sync::oneshot;

type Reply<T> = oneshot::Sender<Result<T, EngineError>>;

enum Command {
    Submit(TaskSpec, Reply<TaskSnapshot>),
    Query(String, Reply<Option<TaskSnapshot>>),
    Cancel(String, Reply<Option<TaskSnapshot>>),
    Stage(String, TaskState, Reply<()>),
    Shutdown(Reply<()>),
}
