//! Service handles and ownership of the simulation scheduler.
use super::{Command, actor::Actor, contract::MAX_TEXT_BYTES, journal::Journal};
use super::{EngineConfig, EngineError, TaskSnapshot, TaskSpec};
use std::path::Path;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

/// Owns a single scheduler. Dropping it aborts the scheduler and its simulations.
pub struct Engine {
    tx: mpsc::Sender<Command>,
    actor: Option<JoinHandle<Result<(), EngineError>>>,
}

/// Cloneable service contract without ownership of engine shutdown.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<Command>,
}

impl Engine {
    pub async fn open(
        data_dir: impl AsRef<Path>,
        config: EngineConfig,
    ) -> Result<Self, EngineError> {
        config.validate()?;
        let dir = data_dir.as_ref().to_owned();
        let limits = config.clone();
        let (mut journal, mut records) =
            tokio::task::spawn_blocking(move || Journal::open(&dir, &limits))
                .await
                .map_err(|error| EngineError::Internal(error.to_string()))??;
        // Do not replay accepted work or uncertain operations after a host restart.
        for record in records.values_mut() {
            if !record.state.is_terminal() {
                record.state = record.state.interrupted(false);
                record.revision += 1;
                journal.append(record).await?;
            }
        }
        let (tx, rx) = mpsc::channel(
            config
                .max_queued
                .saturating_add(config.max_concurrency)
                .min(4096),
        );
        let actor = Actor::new(config, journal, records, tx.clone(), rx);
        Ok(Self {
            tx,
            actor: Some(tokio::spawn(actor.run())),
        })
    }

    pub fn handle(&self) -> EngineHandle {
        EngineHandle {
            tx: self.tx.clone(),
        }
    }

    pub async fn submit(&self, spec: TaskSpec) -> Result<TaskSnapshot, EngineError> {
        self.handle().submit(spec).await
    }

    pub async fn query(&self, id: &str) -> Result<Option<TaskSnapshot>, EngineError> {
        self.handle().query(id).await
    }

    pub async fn cancel(&self, id: &str) -> Result<Option<TaskSnapshot>, EngineError> {
        self.handle().cancel(id).await
    }

    pub async fn shutdown(mut self) -> Result<(), EngineError> {
        let (reply, result) = oneshot::channel();
        let sent = self.tx.send(Command::Shutdown(reply)).await.is_ok();
        if sent {
            result.await.map_err(|_| EngineError::Unavailable)??;
        }
        let outcome = match self.actor.as_mut() {
            Some(actor) => actor
                .await
                .map_err(|error| EngineError::Internal(error.to_string()))?,
            None => Err(EngineError::Unavailable),
        };
        self.actor.take();
        outcome
    }
}

impl EngineHandle {
    /// Returns only after durable acceptance (or a durable duplicate/denial).
    pub async fn submit(&self, spec: TaskSpec) -> Result<TaskSnapshot, EngineError> {
        spec.validate()?;
        let (reply, result) = oneshot::channel();
        self.tx
            .try_send(Command::Submit(spec, reply))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => EngineError::Capacity,
                mpsc::error::TrySendError::Closed(_) => EngineError::Unavailable,
            })?;
        result.await.map_err(|_| EngineError::Unavailable)?
    }

    pub async fn query(&self, id: &str) -> Result<Option<TaskSnapshot>, EngineError> {
        validate_lookup_id(id)?;
        let (reply, result) = oneshot::channel();
        self.tx
            .send(Command::Query(id.into(), reply))
            .await
            .map_err(|_| EngineError::Unavailable)?;
        result.await.map_err(|_| EngineError::Unavailable)?
    }

    /// Waits for the canceled simulation to be reaped before returning its state.
    pub async fn cancel(&self, id: &str) -> Result<Option<TaskSnapshot>, EngineError> {
        validate_lookup_id(id)?;
        let (reply, result) = oneshot::channel();
        self.tx
            .send(Command::Cancel(id.into(), reply))
            .await
            .map_err(|_| EngineError::Unavailable)?;
        result.await.map_err(|_| EngineError::Unavailable)?
    }
}

fn validate_lookup_id(id: &str) -> Result<(), EngineError> {
    if id.is_empty() || id.len() > MAX_TEXT_BYTES || id.chars().any(char::is_control) {
        return Err(EngineError::Invalid(
            "ID must contain 1..128 printable bytes",
        ));
    }
    Ok(())
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(actor) = &self.actor {
            actor.abort();
        }
    }
}
