//! Durable source continuity binding and restart reconstruction.
use super::super::config::binding;
use super::super::{MonitorDefinition, MonitorError};
use super::MonitorState;
use recuvora_core::recovery::incidents::IncidentStore;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceCheckpoint {
    pub(super) binding: String,
    pub(super) generation: Option<String>,
    pub(super) cursor: Option<String>,
    pub(super) last_sequence: Option<u64>,
    pub(super) recent_ids: VecDeque<String>,
    pub(super) retired_generations: VecDeque<String>,
}

impl SourceCheckpoint {
    fn new(config: &MonitorDefinition) -> Result<Self, MonitorError> {
        Ok(Self {
            binding: binding(config)?,
            generation: None,
            cursor: None,
            last_sequence: None,
            recent_ids: VecDeque::new(),
            retired_generations: VecDeque::new(),
        })
    }
}

pub(in crate::monitoring) fn restore_state(
    definition: MonitorDefinition,
    store: &IncidentStore,
) -> Result<MonitorState, MonitorError> {
    let previous = store.checkpoint(&definition.id);
    let checkpoint = match &previous {
        Some(previous) => serde_json::from_value::<SourceCheckpoint>(previous.value.clone())
            .map_err(|error| {
                MonitorError::Configuration(format!("invalid monitor checkpoint: {error}"))
            })?,
        None => SourceCheckpoint::new(&definition)?,
    };
    if checkpoint.binding != binding(&definition)? {
        return Err(MonitorError::Configuration(format!(
            "monitor {} changed source identity; use a new monitor ID",
            definition.id
        )));
    }
    Ok(MonitorState::new(
        definition,
        checkpoint,
        previous.map_or(0, |value| value.sequence),
    ))
}
