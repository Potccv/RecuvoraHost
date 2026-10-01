//! Fixed, side-effect-free simulation stages; no external executor injection.
use super::{Command, EngineError, Simulation, TaskSpec, TaskState};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub(super) enum Outcome {
    Succeeded,
    Failed,
    MissingReceipt,
    TimedOut,
}

async fn stage(
    sender: &mpsc::Sender<Command>,
    id: &str,
    state: TaskState,
) -> Result<(), EngineError> {
    let (reply, result) = oneshot::channel();
    sender
        .send(Command::Stage(id.into(), state, reply))
        .await
        .map_err(|_| EngineError::Unavailable)?;
    result.await.map_err(|_| EngineError::Unavailable)?
}

pub(super) async fn simulate(spec: TaskSpec, sender: mpsc::Sender<Command>) -> Outcome {
    // Yielding is part of the mock diagnosis; there is no real target inspection.
    tokio::task::yield_now().await;
    if !spec.simulation_authorized
        || stage(&sender, &spec.id, TaskState::Executing)
            .await
            .is_err()
    {
        return Outcome::MissingReceipt;
    }
    match spec.simulation {
        Simulation::Fail => return Outcome::Failed,
        Simulation::Exit => return Outcome::MissingReceipt,
        Simulation::Hang => std::future::pending::<()>().await,
        Simulation::Delay { millis } => tokio::time::sleep(Duration::from_millis(millis)).await,
        Simulation::Succeed | Simulation::VerificationFailed => {}
    }
    if stage(&sender, &spec.id, TaskState::Verifying)
        .await
        .is_err()
    {
        return Outcome::MissingReceipt;
    }
    tokio::task::yield_now().await;
    if matches!(spec.simulation, Simulation::VerificationFailed) {
        Outcome::Failed
    } else {
        Outcome::Succeeded
    }
}
