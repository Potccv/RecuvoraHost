//! Offline activation of a complete, validated legacy recovery generation.
use super::*;
use crate::integrations::recovery::{CanonicalTarget, TargetOwnership};
use recuvora_core::recovery::workflow::{
    LegacyRecoveryRevision, LegacyRecoveryTask, RecoveryConfig, RecoveryImport, RecoveryState,
};
use serde::Serialize;
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
};

/// Original trusted limits. The old physical recovery limit is removed only
/// when comparing its configuration with the new pure domain configuration.
#[derive(Clone, Debug)]
pub struct LegacyRecoveryBundleConfig {
    pub recovery: RecoveryConfig,
    pub recovery_journal_bytes: u64,
    pub approval: ApprovalStoreConfig,
    pub knowledge: KnowledgeStoreConfig,
    pub incidents: IncidentStoreConfig,
}

#[derive(Clone, Debug)]
pub struct LegacyRecoveryBundleReport {
    /// Stable root identity; keep using this for RecoveryService and ownership.
    pub recovery_directory: PathBuf,
    pub generation_directory: PathBuf,
    /// Explicitly select this directory for monitoring after the offline switch.
    pub monitoring_directory: PathBuf,
    pub recovery_revisions: usize,
    pub tasks: usize,
    pub approvals: usize,
    pub incidents: usize,
    pub knowledge_records: usize,
}

struct FrozenFile {
    file: File,
    path: PathBuf,
    bytes: Vec<u8>,
}
impl FrozenFile {
    fn open(path: PathBuf, maximum: u64) -> Result<Self, ImportError> {
        let path = crate::boot::host::external_path(&path).map_err(invalid)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(3).custom_flags(0x0020_0000);
        }
        let mut file = options.open(&path)?;
        crate::persistence::paths::validate_current(&path, &file)?;
        file.try_lock_exclusive()?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(maximum + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > maximum {
            return Err(invalid("legacy source exceeds trusted byte limit"));
        }
        Ok(Self { file, path, bytes })
    }
    fn unchanged(&mut self) -> Result<(), ImportError> {
        crate::persistence::paths::validate_current(&self.path, &self.file)?;
        self.file.seek(SeekFrom::Start(0))?;
        let mut current = Vec::new();
        Read::by_ref(&mut self.file)
            .take(self.bytes.len() as u64 + 1)
            .read_to_end(&mut current)?;
        if current != self.bytes {
            return Err(invalid("frozen legacy source changed during conversion"));
        }
        Ok(())
    }
}

struct Generation {
    path: PathBuf,
    activated: bool,
}
impl Drop for Generation {
    fn drop(&mut self) {
        if !self.activated {
            // This exact directory was created with create_dir by this call;
            // never enumerate or clean any other generation or source directory.
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowEntry {
    format: u32,
    sequence: u64,
    config: Value,
    task: LegacyRecoveryTask,
}

fn event_entries(bytes: &[u8], max_record_bytes: usize) -> Result<Vec<Entry>, ImportError> {
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        return Err(invalid("incomplete legacy event journal; source retained"));
    }
    bytes
        .split_inclusive(|b| *b == b'\n')
        .enumerate()
        .map(|(index, line)| {
            if line.len() > max_record_bytes {
                return Err(invalid("oversized legacy event"));
            }
            let entry: Entry = serde_json::from_slice(line)?;
            if entry.format != 1 || entry.sequence != index as u64 + 1 {
                return Err(invalid("legacy event format or sequence mismatch"));
            }
            Ok(entry)
        })
        .collect()
}

fn workflow_revisions(
    bytes: &[u8],
    config: &LegacyRecoveryBundleConfig,
) -> Result<Vec<LegacyRecoveryRevision>, ImportError> {
    if bytes.is_empty() || bytes.last() != Some(&b'\n') {
        return Err(invalid(
            "legacy recovery requires complete nonempty history",
        ));
    }
    let mut expected = serde_json::to_value(&config.recovery)?;
    expected["max_journal_bytes"] = json!(config.recovery_journal_bytes);
    bytes
        .split_inclusive(|b| *b == b'\n')
        .enumerate()
        .map(|(index, line)| {
            if line.len() > 256 * 1024 {
                return Err(invalid("oversized legacy recovery revision"));
            }
            let entry: WorkflowEntry = serde_json::from_slice(line)?;
            if entry.config != expected || entry.sequence != index as u64 + 1 {
                return Err(invalid(
                    "legacy recovery configuration or sequence mismatch",
                ));
            }
            Ok(LegacyRecoveryRevision {
                format: entry.format,
                sequence: entry.sequence,
                task: entry.task,
            })
        })
        .collect()
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), ImportError> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Freeze all four source domains, validate their complete histories and publish
/// one redirect only after the new generation is durable. The caller must stop
/// the old Host and independently settle in-flight external calls first. Unknown
/// evidence may remain Unknown; this function never executes or verifies work.
///
/// The original root/recovery.lock object and target ownership claim survive.
/// Original workflow bytes are retained in the generation; other source logs
/// stay untouched. Old binaries cannot parse the activated root redirect.
pub fn import_legacy_recovery_bundle(
    root: &Path,
    incident_journal: &Path,
    config: LegacyRecoveryBundleConfig,
    ownership: &dyn TargetOwnership,
    now_ms: u64,
) -> Result<LegacyRecoveryBundleReport, ImportError> {
    import_with_activation(
        root,
        incident_journal,
        config,
        ownership,
        now_ms,
        |source, target| std::fs::rename(source, target),
    )
}

fn import_with_activation(
    root: &Path,
    incident_journal: &Path,
    config: LegacyRecoveryBundleConfig,
    ownership: &dyn TargetOwnership,
    now_ms: u64,
    activate: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<LegacyRecoveryBundleReport, ImportError> {
    config.recovery.validate().map_err(invalid)?;
    config.approval.validate().map_err(invalid)?;
    config.knowledge.validate().map_err(invalid)?;
    config.incidents.validate().map_err(invalid)?;
    if !(4096..=128 * 1024 * 1024).contains(&config.recovery_journal_bytes) {
        return Err(invalid("legacy recovery byte limit out of range"));
    }
    let root = crate::boot::host::external_path(root).map_err(invalid)?;
    if !root.is_dir() {
        return Err(invalid("legacy recovery root must already exist"));
    }
    let mut root_lock = FrozenFile::open(root.join("recovery.lock"), 0)?;
    let mut approval_lock = FrozenFile::open(root.join("approvals/approvals.lock"), 0)?;
    let mut recovery_source =
        FrozenFile::open(root.join("recovery.jsonl"), config.recovery_journal_bytes)?;
    let mut approval_source = FrozenFile::open(
        root.join("approvals/approvals.jsonl"),
        config.approval.max_journal_bytes,
    )?;
    let mut knowledge_source = FrozenFile::open(
        root.join("knowledge.jsonl"),
        config.knowledge.max_journal_bytes,
    )?;
    let mut incident_source = FrozenFile::open(
        incident_journal.to_path_buf(),
        config.incidents.max_journal_bytes,
    )?;
    let revisions = workflow_revisions(&recovery_source.bytes, &config)?;
    let target = CanonicalTarget::new(config.recovery.target.target_id.clone()).map_err(invalid)?;
    let lease = ownership.acquire(&target, &root).map_err(invalid)?;
    if lease.target() != &target || lease.recovery_directory() != root.canonicalize()? {
        return Err(invalid(
            "ownership does not bind the original root and target",
        ));
    }
    lease.validate().map_err(invalid)?;

    let name = format!(".core02-{}", crate::protocol::call_id());
    let generation_path = root.join(&name);
    std::fs::create_dir(&generation_path)?;
    let mut generation = Generation {
        path: generation_path.clone(),
        activated: false,
    };
    std::fs::create_dir(generation_path.join("approvals"))?;
    let mut approval_journal = Journal::open(
        generation_path.join("approvals/approvals.jsonl"),
        "approval",
        serde_json::to_value(&config.approval)?,
        config.approval.max_journal_bytes,
    )?;
    let mut approvals = super::approval(
        &mut approval_journal,
        event_entries(&approval_source.bytes, 262_144)?,
        config.approval.clone(),
    )?;
    let mut knowledge_journal = Journal::open(
        generation_path.join("knowledge.jsonl"),
        "knowledge",
        serde_json::to_value(&config.knowledge)?,
        config.knowledge.max_journal_bytes,
    )?;
    let knowledge = super::knowledge(
        &mut knowledge_journal,
        event_entries(&knowledge_source.bytes, 262_144)?,
        config.knowledge.clone(),
    )?;
    let mut incidents_journal = Journal::open(
        generation_path.join("incidents.jsonl"),
        "incidents",
        serde_json::to_value(&config.incidents)?,
        config.incidents.max_journal_bytes,
    )?;
    let incidents = super::incidents(
        &mut incidents_journal,
        event_entries(&incident_source.bytes, 524_288)?,
        config.incidents.clone(),
    )?;
    for revision in &revisions {
        let problem = &revision.task.problem;
        let incident = incidents
            .get(&problem.incident_id)
            .ok_or_else(|| invalid("workflow references a missing incident"))?;
        if incident.target_id != problem.target_id
            || incident.revision < problem.incident_revision
            || incident.occurrences < problem.occurrences
        {
            return Err(invalid(
                "workflow incident identity, revision or sample count differs from frozen facts",
            ));
        }
    }
    let revision_count = revisions.len();
    let imported = RecoveryImport::validate(
        config.recovery.clone(),
        revisions.clone(),
        &approvals,
        &knowledge,
    )
    .map_err(invalid)?;
    for uncertainty in imported.execution_uncertainties() {
        let pending = approvals
            .prepare_legacy_uncertain(approval_journal.next_id(), uncertainty, now_ms / 1000)
            .map_err(invalid)?;
        let payload = serde_json::to_value(
            pending
                .state()
                .latest_entry()
                .ok_or_else(|| invalid("missing legacy uncertainty transaction"))?,
        )?;
        let committed = persist(&mut approval_journal, pending, payload)?;
        if !committed.effects.is_empty() {
            return Err(invalid(
                "legacy uncertainty unexpectedly produced external effects",
            ));
        }
        approvals = committed.state;
    }
    let imported =
        RecoveryImport::validate(config.recovery.clone(), revisions, &approvals, &knowledge)
            .map_err(invalid)?;
    let state = RecoveryState::new(config.recovery.clone()).map_err(invalid)?;
    let mut recovery_journal = Journal::open(
        generation_path.join("recovery.jsonl"),
        "recovery",
        serde_json::to_value(&config.recovery)?,
        256 * 1024 * 1024,
    )?;
    let pending = state
        .prepare_import(recovery_journal.next_id(), imported, now_ms)
        .map_err(invalid)?;
    let payload = serde_json::to_value(
        pending
            .state()
            .latest_entry()
            .ok_or_else(|| invalid("missing imported recovery transaction"))?,
    )?;
    let committed = persist(&mut recovery_journal, pending, payload)?;
    if !committed.effects.is_empty() {
        return Err(invalid(
            "legacy import unexpectedly produced external effects",
        ));
    }
    RecoveryState::restore(config.recovery.clone(), committed.state.entries()).map_err(invalid)?;
    let report = LegacyRecoveryBundleReport {
        recovery_directory: root.clone(),
        generation_directory: generation_path.clone(),
        monitoring_directory: generation_path.clone(),
        recovery_revisions: revision_count,
        tasks: committed.state.tasks().count(),
        approvals: approvals.list().len(),
        incidents: incidents.list().len(),
        knowledge_records: knowledge.snapshot().records.len(),
    };
    let mut dispatch = Journal::open(
        generation_path.join("dispatch.jsonl"),
        "dispatch",
        serde_json::to_value(&config.recovery)?,
        64 * 1024 * 1024,
    )?;
    // No dispatch records are invented for legacy execution; absent evidence
    // must stay Unknown until independently checked after activation.
    dispatch.close()?;
    approval_journal.close()?;
    knowledge_journal.close()?;
    incidents_journal.close()?;
    recovery_journal.close()?;
    write_new(
        &generation_path.join("legacy-recovery.jsonl"),
        &recovery_source.bytes,
    )?;
    #[derive(Serialize)]
    struct Manifest<'a> {
        format: u32,
        recovery_revisions: usize,
        target_id: &'a str,
    }
    write_new(
        &generation_path.join("import.json"),
        &serde_json::to_vec(&Manifest {
            format: 1,
            recovery_revisions: revision_count,
            target_id: target.as_str(),
        })?,
    )?;
    for source in [
        &mut root_lock,
        &mut approval_lock,
        &mut recovery_source,
        &mut approval_source,
        &mut knowledge_source,
        &mut incident_source,
    ] {
        source.unchanged()?;
    }
    lease.validate().map_err(invalid)?;
    let marker_path = root.join(format!(".recovery-redirect-{}", crate::protocol::call_id()));
    let marker = Staged(marker_path.clone());
    let mut bytes = serde_json::to_vec(&json!({"host_recovery_layout":1,"generation":name}))?;
    bytes.push(b'\n');
    write_new(&marker_path, &bytes)?;
    #[cfg(unix)]
    {
        File::open(generation_path.join("approvals"))?.sync_all()?;
        File::open(&generation_path)?.sync_all()?;
    }
    // Windows requires closing the old leaf before replacement. Both original
    // writer locks and stable target ownership remain held through activation.
    drop(recovery_source);
    #[cfg(test)]
    crash_activation("before_marker");
    activate(&marker_path, &root.join("recovery.jsonl"))?;
    generation.activated = true;
    #[cfg(unix)]
    File::open(&root)?.sync_all()?;
    #[cfg(test)]
    crash_activation("after_marker");
    drop(marker);
    Ok(report)
}

#[cfg(test)]
fn crash_activation(boundary: &str) {
    if std::env::var("RECUVORA_IMPORT_CRASH").as_deref() == Ok(boundary) {
        std::process::exit(94);
    }
}

#[cfg(test)]
#[path = "../../../tests/legacy_activation.rs"]
mod tests;
