//! Explicit offline conversion of complete legacy event journals.
//! The old owner must be stopped and all in-flight effects independently checked.
//! This adapter never invokes an executor or installs a partial conversion.
use super::{
    approval::ApprovalStoreConfig,
    incidents::IncidentStoreConfig,
    journal::{Journal, JournalError},
    knowledge::{self, KnowledgeStoreConfig},
};
use fs2::FileExt;
use recuvora_core::{
    operation::Prepared,
    recovery::{approval::*, incidents::*, knowledge::*},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
};

mod bundle;
pub use bundle::{
    LegacyRecoveryBundleConfig, LegacyRecoveryBundleReport, import_legacy_recovery_bundle,
};

#[derive(Clone, Debug)]
pub enum LegacyDomain {
    Approval(ApprovalStoreConfig),
    Incidents(IncidentStoreConfig),
    Knowledge(KnowledgeStoreConfig),
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("legacy import rejected: {0}")]
    Invalid(String),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error("legacy import I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("legacy import JSON: {0}")]
    Json(#[from] serde_json::Error),
}
fn invalid(error: impl std::fmt::Display) -> ImportError {
    ImportError::Invalid(error.to_string())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    format: u32,
    sequence: u64,
    #[serde(default)]
    now: Option<u64>,
    event: Value,
}

struct Staged(PathBuf);
impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Convert a frozen old journal to a new, absent destination file. The source is
/// never modified; the destination appears only after all domain validation and
/// durable writes succeed. Approval sources also require their old writer lock.
/// The caller supplies the original trusted limits and verifies external work is
/// quiescent; filesystem locks alone cannot prove an executor has stopped.
/// This is a maintenance Rust API, never a model tool or an HTTP write endpoint.
pub fn import_legacy_journal(
    source: &Path,
    destination: &Path,
    domain: LegacyDomain,
) -> Result<usize, ImportError> {
    let source = crate::boot::host::external_path(source).map_err(invalid)?;
    let destination = crate::boot::host::external_path(destination).map_err(invalid)?;
    if destination.exists() || source == destination {
        return Err(invalid("destination must be absent and distinct"));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("missing destination parent"))?;
    if !parent.is_dir() {
        return Err(invalid("destination parent must already exist"));
    }
    let (name, config, max_bytes) = match &domain {
        LegacyDomain::Approval(c) => {
            c.validate().map_err(invalid)?;
            ("approval", serde_json::to_value(c)?, c.max_journal_bytes)
        }
        LegacyDomain::Incidents(c) => {
            c.validate().map_err(invalid)?;
            ("incidents", serde_json::to_value(c)?, c.max_journal_bytes)
        }
        LegacyDomain::Knowledge(c) => {
            c.validate().map_err(invalid)?;
            ("knowledge", serde_json::to_value(c)?, c.max_journal_bytes)
        }
    };
    let _approval_lock = if matches!(domain, LegacyDomain::Approval(_)) {
        let path = source
            .parent()
            .ok_or_else(|| invalid("missing source parent"))?
            .join("approvals.lock");
        let lock = OpenOptions::new().read(true).write(true).open(&path)?;
        super::paths::validate_current(&path, &lock)?;
        lock.try_lock_exclusive()?;
        Some(lock)
    } else {
        None
    };
    let mut original = OpenOptions::new().read(true).write(true).open(&source)?;
    super::paths::validate_current(&source, &original)?;
    original.try_lock_exclusive()?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut original)
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes || (!bytes.is_empty() && bytes.last() != Some(&b'\n')) {
        return Err(invalid(
            "oversized journal or incomplete final record; source retained",
        ));
    }
    let mut entries = Vec::new();
    let max_record_bytes = if matches!(domain, LegacyDomain::Incidents(_)) {
        524_288
    } else {
        262_144
    };
    for (index, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if line.len() > max_record_bytes {
            return Err(invalid("oversized legacy record"));
        }
        let entry: Entry = serde_json::from_slice(&line[..line.len() - 1])?;
        if entry.format != 1 || entry.sequence != index as u64 + 1 {
            return Err(invalid("unsupported legacy format or sequence"));
        }
        entries.push(entry);
    }
    // The staging file is task-owned and cannot replace an existing path.
    let staged = Staged(parent.join(format!(
        ".legacy-import-{}.jsonl",
        crate::protocol::call_id()
    )));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged.0)?
        .sync_all()?;
    let mut journal = Journal::open(&staged.0, name, config, max_bytes)?;
    let count = entries.len();
    match domain {
        LegacyDomain::Approval(c) => {
            approval(&mut journal, entries, c)?;
        }
        LegacyDomain::Incidents(c) => {
            incidents(&mut journal, entries, c)?;
        }
        LegacyDomain::Knowledge(c) => {
            knowledge(&mut journal, entries, c)?;
        }
    }
    journal.close()?;
    drop(journal);
    // Verify the source still denotes the frozen file before publishing.
    super::paths::validate_current(&source, &original)?;
    if original.metadata()?.len() != bytes.len() as u64 {
        return Err(invalid("source changed during conversion"));
    }
    // Atomic create-if-absent, unlike rename which can overwrite a destination.
    std::fs::hard_link(&staged.0, &destination)?;
    std::fs::remove_file(&staged.0)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(count)
}

fn persist<S, E>(
    journal: &mut Journal,
    pending: Prepared<S, E>,
    payload: Value,
) -> Result<recuvora_core::operation::Committed<S, E>, ImportError> {
    let receipt = journal.commit(pending.request(), payload)?;
    pending.confirm(receipt).map_err(invalid)
}
fn approval(
    journal: &mut Journal,
    entries: Vec<Entry>,
    config: ApprovalStoreConfig,
) -> Result<ApprovalLedger, ImportError> {
    let limits = ApprovalLimits {
        max_requests: config.max_requests,
    };
    let state = ApprovalLedger::new(limits.clone()).map_err(invalid)?;
    if entries.is_empty() {
        return Ok(state);
    }
    let history = entries
        .into_iter()
        .map(|entry| {
            Ok(LegacyApprovalEntry {
                sequence: entry.sequence,
                now: entry
                    .now
                    .ok_or_else(|| invalid("approval timestamp required"))?,
                event: entry.event,
            })
        })
        .collect::<Result<Vec<_>, ImportError>>()?;
    let now = history.iter().map(|entry| entry.now).max().unwrap_or(0);
    let imported = ApprovalImport::validate(limits.clone(), history).map_err(invalid)?;
    let pending = state
        .prepare_import(journal.next_id(), imported, now)
        .map_err(invalid)?;
    let payload = serde_json::to_value(
        pending
            .state()
            .latest_entry()
            .ok_or_else(|| invalid("missing approval import entry"))?,
    )?;
    let committed = persist(journal, pending, payload)?;
    if !committed.effects.is_empty() {
        return Err(invalid("approval import cannot release effects"));
    }
    ApprovalLedger::restore(limits, committed.state.entries()).map_err(invalid)?;
    Ok(committed.state)
}
fn incidents(
    journal: &mut Journal,
    entries: Vec<Entry>,
    config: IncidentStoreConfig,
) -> Result<IncidentLedger, ImportError> {
    let limits = IncidentLimits {
        max_incidents: config.max_incidents,
        max_monitors: config.max_monitors,
    };
    let mut state = IncidentLedger::new(limits.clone()).map_err(invalid)?;
    for entry in entries {
        if entry.now.is_some() {
            return Err(invalid("unexpected incident timestamp field"));
        }
        let event: IncidentEvent = serde_json::from_value(entry.event)?;
        let pending = match event {
            IncidentEvent::Monitor { commit } => state
                .prepare_monitor(journal.next_id(), commit)
                .map_err(invalid)?
                .ok_or_else(|| invalid("duplicate persisted monitor commit"))?,
            IncidentEvent::Acknowledge {
                id,
                expected_revision,
                actor,
                note,
                now_ms,
            } => state
                .prepare_acknowledge(
                    journal.next_id(),
                    id,
                    expected_revision,
                    actor,
                    note,
                    now_ms,
                )
                .map_err(invalid)?,
        };
        let payload = serde_json::to_value(
            pending
                .state()
                .latest_entry()
                .ok_or_else(|| invalid("missing incident entry"))?,
        )?;
        state = persist(journal, pending, payload)?.state;
    }
    IncidentLedger::restore(limits, state.entries()).map_err(invalid)?;
    Ok(state)
}
fn knowledge(
    journal: &mut Journal,
    entries: Vec<Entry>,
    config: KnowledgeStoreConfig,
) -> Result<KnowledgeState, ImportError> {
    let mut state = KnowledgeState::new(KnowledgeConfig {
        max_records: config.max_records,
        max_cases_per_record: config.max_cases_per_record,
    })
    .map_err(invalid)?;
    for entry in entries {
        if entry.now.is_some() {
            return Err(invalid("unexpected knowledge timestamp field"));
        }
        let mut event = entry
            .event
            .as_object()
            .cloned()
            .ok_or_else(|| invalid("knowledge event must be an object"))?;
        let kind = event
            .remove("type")
            .ok_or_else(|| invalid("knowledge event type missing"))?;
        let payload = match kind.as_str() {
            Some("candidate") if event.len() == 1 => {
                json!({"UpsertCandidate":event.remove("candidate").ok_or_else(|| invalid("missing candidate"))?})
            }
            Some("outcome") => json!({"RecordOutcome":event}),
            Some("disable") => json!({"Disable":event}),
            _ => {
                return Err(invalid(
                    "legacy checkpoint requires full original command history; no facts discarded",
                ));
            }
        };
        let command = knowledge::decode_command(payload.clone()).map_err(invalid)?;
        let pending = state.propose(journal.next_id(), command).map_err(invalid)?;
        state = persist(journal, pending, payload)?.state;
    }
    Ok(state)
}
