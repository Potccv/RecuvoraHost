//! Lifecycle and service composition for trusted, cooperative in-process modules.
//!
//! A runtime has one startup/shutdown cycle. Scope is an exact string namespace;
//! there is no hierarchy, hot replacement, or process isolation in this crate.

pub mod operation;

mod context;
mod contracts;
mod dependencies;
mod events;
#[path = "runtime.rs"]
mod lifecycle;
mod resources;
mod state;

pub use context::ModuleContext;
pub use contracts::{
    FrameworkError, InstanceId, LifecycleFuture, LifecycleOptions, Module, ModuleError,
    ModuleMetadata, RuntimeSnapshot, ServiceKey, ShutdownReport, StartupError,
};
pub use events::{DeliveryReport, Event, EventSubscription};
pub use lifecycle::Runtime;
