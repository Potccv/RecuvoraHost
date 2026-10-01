//! Single authority for queued tasks, target ownership and durable transitions.
use super::{Command, EngineConfig, EngineError, Reply, TaskSnapshot, TaskSpec, TaskState};
use super::{
    journal::Journal,
    worker::{Outcome, simulate},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    time::Duration,
};
use tokio::{
    sync::mpsc,
    task::{AbortHandle, Id, JoinSet},
};

pub(super) struct Actor {
    config: EngineConfig,
    journal: Journal,
    records: BTreeMap<String, TaskSnapshot>,
    queue: VecDeque<String>,
    workers: JoinSet<Outcome>,
    active: HashMap<String, AbortHandle>,
    worker_ids: HashMap<Id, String>,
    targets: HashSet<String>,
    cancel_replies: HashMap<String, Vec<Reply<Option<TaskSnapshot>>>>,
    tx: mpsc::Sender<Command>,
    rx: mpsc::Receiver<Command>,
}

impl Actor {
    pub(super) fn new(
        config: EngineConfig,
        journal: Journal,
        records: BTreeMap<String, TaskSnapshot>,
        tx: mpsc::Sender<Command>,
        rx: mpsc::Receiver<Command>,
    ) -> Self {
        let targets = records
            .values()
            .filter(|record| record.state == TaskState::Unknown)
            .map(|record| record.spec.target.clone())
            .collect();
        Self {
            config,
            journal,
            records,
            queue: VecDeque::new(),
            workers: JoinSet::new(),
            active: HashMap::new(),
            worker_ids: HashMap::new(),
            targets,
            cancel_replies: HashMap::new(),
            tx,
            rx,
        }
    }

    async fn transition(&mut self, id: &str, state: TaskState) -> Result<(), EngineError> {
        let previous = self
            .records
            .get(id)
            .ok_or_else(|| EngineError::Internal("missing task".into()))?;
        if !previous.state.allows(state) {
            return Err(EngineError::Internal("invalid state transition".into()));
        }
        let mut next = previous.clone();
        next.state = state;
        next.revision += 1;
        self.journal.append(&next).await?;
        self.records.insert(id.into(), next);
        Ok(())
    }

    async fn dispatch(&mut self) -> Result<(), EngineError> {
        while self.active.len() < self.config.max_concurrency {
            let position = self.queue.iter().position(|id| {
                self.records
                    .get(id)
                    .is_some_and(|record| !self.targets.contains(&record.spec.target))
            });
            let Some(position) = position else { break };
            let Some(id) = self.queue.remove(position) else {
                break;
            };
            self.transition(&id, TaskState::Diagnosing).await?;
            let spec = self
                .records
                .get(&id)
                .ok_or_else(|| EngineError::Internal("missing dispatched task".into()))?
                .spec
                .clone();
            self.targets.insert(spec.target.clone());
            let sender = self.tx.clone();
            let timeout = Duration::from_millis(spec.timeout_ms);
            let worker = self.workers.spawn(async move {
                match tokio::time::timeout(timeout, simulate(spec, sender)).await {
                    Ok(outcome) => outcome,
                    Err(_) => Outcome::TimedOut,
                }
            });
            self.worker_ids.insert(worker.id(), id.clone());
            self.active.insert(id, worker);
        }
        Ok(())
    }

    async fn submit(
        &mut self,
        spec: TaskSpec,
        reply: Reply<TaskSnapshot>,
    ) -> Result<(), EngineError> {
        if let Some(previous) = self.records.get(&spec.id) {
            let _ = reply.send(if previous.spec == spec {
                Ok(previous.clone())
            } else {
                Err(EngineError::Conflict)
            });
            return Ok(());
        }
        if self.records.len() >= self.config.max_tasks
            || (spec.simulation_authorized && self.queue.len() >= self.config.max_queued)
        {
            let _ = reply.send(Err(EngineError::Capacity));
            return Ok(());
        }
        let snapshot = TaskSnapshot {
            state: if spec.simulation_authorized {
                TaskState::Queued
            } else {
                TaskState::Denied
            },
            spec,
            revision: 0,
        };
        if let Err(error) = self.journal.append(&snapshot).await {
            let _ = reply.send(Err(error));
            return Err(EngineError::Unavailable);
        }
        self.records
            .insert(snapshot.spec.id.clone(), snapshot.clone());
        if snapshot.state == TaskState::Queued {
            self.queue.push_back(snapshot.spec.id.clone());
        }
        let _ = reply.send(Ok(snapshot));
        Ok(())
    }

    async fn cancel(
        &mut self,
        id: String,
        reply: Reply<Option<TaskSnapshot>>,
    ) -> Result<(), EngineError> {
        if let Some(worker) = self.active.get(&id) {
            // Multiple outstanding cancel requests are bounded by the command channel
            // only until completed_pending, so refuse excess waiters for this task explicitly.
            let waiters = self.cancel_replies.entry(id).or_default();
            if waiters.len() >= 64 {
                let _ = reply.send(Err(EngineError::Capacity));
            } else {
                waiters.push(reply);
                worker.abort();
            }
            return Ok(());
        }
        if self
            .records
            .get(&id)
            .is_some_and(|record| record.state == TaskState::Queued)
        {
            self.queue.retain(|queued| queued != &id);
            self.transition(&id, TaskState::Canceled).await?;
        }
        let _ = reply.send(Ok(self.records.get(&id).cloned()));
        Ok(())
    }

    async fn complete(
        &mut self,
        completed: Result<(Id, Outcome), tokio::task::JoinError>,
    ) -> Result<(), EngineError> {
        let worker_id = match &completed {
            Ok((id, _)) => *id,
            Err(error) => error.id(),
        };
        let id = self
            .worker_ids
            .remove(&worker_id)
            .ok_or_else(|| EngineError::Internal("unknown completed worker".into()))?;
        let previous = self
            .records
            .get(&id)
            .ok_or_else(|| EngineError::Internal("missing completed task".into()))?;
        let target = previous.spec.target.clone();
        let state = match completed {
            Ok((_, Outcome::Succeeded)) => TaskState::Succeeded,
            Ok((_, Outcome::Failed)) => TaskState::Failed,
            Ok((_, Outcome::MissingReceipt)) => previous.state.interrupted(false),
            Ok((_, Outcome::TimedOut)) => previous.state.interrupted(true),
            Err(_) => previous.state.interrupted(false),
        };
        self.transition(&id, state).await?;
        self.active.remove(&id);
        // Unknown work requires a result check before conflicting follow-up work.
        // This simulation has no result check or authorization API for real actions.
        if state != TaskState::Unknown {
            self.targets.remove(&target);
        }
        if let Some(waiters) = self.cancel_replies.remove(&id) {
            for reply in waiters {
                let _ = reply.send(Ok(self.records.get(&id).cloned()));
            }
        }
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), EngineError> {
        self.rx.close();
        for worker in self.active.values() {
            worker.abort();
        }
        while let Some(completed) = self.workers.join_next_with_id().await {
            self.complete(completed).await?;
        }
        while let Some(id) = self.queue.pop_front() {
            self.transition(&id, TaskState::Canceled).await?;
        }
        // A received but unhandled command was never acknowledged as durable.
        while self.rx.try_recv().is_ok() {}
        Ok(())
    }

    pub(super) async fn run(mut self) -> Result<(), EngineError> {
        loop {
            self.dispatch().await?;
            tokio::select! {
                completed = self.workers.join_next_with_id(), if !self.workers.is_empty() => {
                    if let Some(completed) = completed { self.complete(completed).await?; }
                }
                command = self.rx.recv() => {
                    match command {
                        Some(Command::Submit(spec, reply)) => self.submit(spec, reply).await?,
                        Some(Command::Query(id, reply)) => { let _ = reply.send(Ok(self.records.get(&id).cloned())); }
                        Some(Command::Cancel(id, reply)) => self.cancel(id, reply).await?,
                        Some(Command::Stage(id, state, reply)) => {
                            // Timed-out or canceled simulations cannot advance a finished task.
                            if self.active.contains_key(&id) && !self.cancel_replies.contains_key(&id) && !reply.is_closed() {
                                self.transition(&id, state).await?;
                                let _ = reply.send(Ok(()));
                            } else {
                                let _ = reply.send(Err(EngineError::Unavailable));
                            }
                        }
                        Some(Command::Shutdown(reply)) => {
                            match self.stop().await {
                                Ok(()) => { let _ = reply.send(Ok(())); return Ok(()); }
                                Err(error) => { let _ = reply.send(Err(error)); return Err(EngineError::Unavailable); }
                            }
                        }
                        None => return self.stop().await,
                    }
                }
            }
        }
    }
}
