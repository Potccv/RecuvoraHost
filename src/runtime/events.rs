//! Ephemeral bounded event subscriptions owned by a module instance.
use super::{FrameworkError, InstanceId, ModuleContext};
use tokio::sync::mpsc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    pub topic: String,
    pub data: String,
}

/// An event is ephemeral. Full queues drop this event and are reported to its sender.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeliveryReport {
    pub delivered: usize,
    pub full: usize,
    pub closed: usize,
}

pub struct EventSubscription {
    receiver: mpsc::Receiver<Event>,
}

impl EventSubscription {
    pub async fn recv(&mut self) -> Option<Event> {
        self.receiver.recv().await
    }

    pub fn try_recv(&mut self) -> Result<Event, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }
}

pub(super) struct Listener {
    pub(super) owner: InstanceId,
    topic: String,
    sender: mpsc::Sender<Event>,
}

impl ModuleContext {
    pub fn subscribe(
        &self,
        topic: impl Into<String>,
        capacity: usize,
    ) -> Result<EventSubscription, FrameworkError> {
        let topic = topic.into();
        if topic.is_empty()
            || topic.len() > self.options.max_event_bytes
            || capacity == 0
            || capacity > self.options.max_event_queue
            || capacity > tokio::sync::Semaphore::MAX_PERMITS
        {
            return Err(FrameworkError::InvalidConfiguration(
                "event topic or queue capacity".into(),
            ));
        }
        let mut state = self.lock()?;
        state.active(&self.instance)?;
        state
            .listeners
            .retain(|listener| !listener.sender.is_closed());
        if state
            .listeners
            .iter()
            .filter(|listener| listener.owner == self.instance)
            .count()
            >= self.options.max_subscriptions_per_module
        {
            return Err(FrameworkError::CapacityExceeded(
                "instance subscriptions".into(),
            ));
        }
        let (sender, receiver) = mpsc::channel(capacity);
        state.listeners.push(Listener {
            owner: self.instance.clone(),
            topic,
            sender,
        });
        Ok(EventSubscription { receiver })
    }

    pub fn emit(
        &self,
        topic: impl Into<String>,
        data: impl Into<String>,
    ) -> Result<DeliveryReport, FrameworkError> {
        let event = Event {
            topic: topic.into(),
            data: data.into(),
        };
        if event.topic.is_empty()
            || event.topic.len().saturating_add(event.data.len()) > self.options.max_event_bytes
        {
            return Err(FrameworkError::CapacityExceeded("event bytes".into()));
        }
        let mut state = self.lock()?;
        state.active(&self.instance)?;
        let mut report = DeliveryReport::default();
        state.listeners.retain(|listener| {
            if listener.topic != event.topic {
                return !listener.sender.is_closed();
            }
            match listener.sender.try_send(event.clone()) {
                Ok(()) => {
                    report.delivered += 1;
                    true
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    report.full += 1;
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    report.closed += 1;
                    false
                }
            }
        });
        Ok(report)
    }
}
