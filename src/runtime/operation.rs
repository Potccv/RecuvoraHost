//! Cooperative cancellation and ownership of in-flight service calls.
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::watch;

/// Shared, irreversible cancellation. Request cancellation, then await the
/// original operation to observe cleanup and any unknown persistent outcome.
#[derive(Clone, Debug)]
pub struct Cancellation {
    sender: watch::Sender<bool>,
}

impl Cancellation {
    pub fn new() -> Self {
        let (sender, _) = watch::channel(false);
        Self { sender }
    }

    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }

    pub async fn cancelled(&self) {
        let mut receiver = self.sender.subscribe();
        while !*receiver.borrow_and_update() {
            let _ = receiver.changed().await;
        }
    }
}

impl Default for Cancellation {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("service is shutting down")]
    Closed,
    #[error("service call capacity exhausted")]
    Capacity,
    #[error("service call state lock was poisoned")]
    Poisoned,
    #[error("service call supervisor failed: {0}")]
    Supervisor(String),
}

#[derive(Default)]
struct Calls {
    closed: bool,
    next: u64,
    active: BTreeMap<u64, Cancellation>,
}

struct SharedCalls {
    state: Mutex<Calls>,
    count: watch::Sender<usize>,
}

/// Bounded service dispatch with cooperative shutdown. The supervised future
/// owns its slot even if its caller is dropped; shutdown waits for that future.
/// This does not prove that a remote executor has stopped external effects.
#[derive(Clone)]
pub struct CallScope {
    shared: Arc<SharedCalls>,
}

impl Default for CallScope {
    fn default() -> Self {
        Self {
            shared: Arc::new(SharedCalls {
                state: Mutex::new(Calls::default()),
                count: watch::channel(0).0,
            }),
        }
    }
}

impl CallScope {
    pub async fn run<T: Send + 'static>(
        &self,
        cancellation: Cancellation,
        operation: impl Future<Output = T> + Send + 'static,
    ) -> Result<T, DispatchError> {
        let id = {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| DispatchError::Poisoned)?;
            if state.closed {
                return Err(DispatchError::Closed);
            }
            if state.active.len() >= 1024 {
                return Err(DispatchError::Capacity);
            }
            let id = state.next;
            state.next = state.next.checked_add(1).ok_or(DispatchError::Capacity)?;
            state.active.insert(id, cancellation.clone());
            self.shared.count.send_replace(state.active.len());
            id
        };
        let guard = ActiveCall {
            shared: self.shared.clone(),
            id,
        };
        let mut caller = CancelOnDrop(Some(cancellation));
        let task = tokio::spawn(async move {
            let _guard = guard;
            operation.await
        });
        let result = task
            .await
            .map_err(|error| DispatchError::Supervisor(error.to_string()));
        if result.is_ok() {
            caller.0 = None;
        }
        result
    }

    /// Synchronously revoke dispatch, including when no executor is available.
    pub fn close(&self) -> Result<(), DispatchError> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| DispatchError::Poisoned)?;
        state.closed = true;
        for cancellation in state.active.values() {
            cancellation.cancel();
        }
        Ok(())
    }

    /// Idempotently reject new calls and cancel existing calls before draining.
    /// Dropping this future leaves the scope closed and allows a later retry.
    pub async fn shutdown(&self) -> Result<(), DispatchError> {
        let mut count = self.shared.count.subscribe();
        self.close()?;
        while *count.borrow_and_update() != 0 {
            count.changed().await.map_err(|_| DispatchError::Poisoned)?;
        }
        Ok(())
    }
}

struct ActiveCall {
    shared: Arc<SharedCalls>,
    id: u64,
}

impl Drop for ActiveCall {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.active.remove(&self.id);
            self.shared.count.send_replace(state.active.len());
        }
    }
}

struct CancelOnDrop(Option<Cancellation>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancellation) = &self.0 {
            cancellation.cancel();
        }
    }
}
