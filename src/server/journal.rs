//! Bounded transport receipts. Business authority remains in recovery services.
use super::{ApiError, Operation, timestamp};
use fs2::FileExt;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::Path,
};

const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_OPERATIONS: usize = 1000;
const MAX_EVENTS: usize = 8000;

pub(super) struct Journal {
    file: File,
    pub records: BTreeMap<String, Operation>,
    pub events: Vec<Operation>,
    bytes: u64,
    failed: bool,
}

impl Journal {
    pub fn open(dir: &Path) -> Result<Self, ApiError> {
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(dir.join("operations.jsonl"))?;
        file.try_lock_exclusive()?;
        let bytes = file.metadata()?.len();
        if bytes > MAX_JOURNAL_BYTES {
            return Err(ApiError::unavailable("operation journal capacity exceeded"));
        }
        let mut records = BTreeMap::new();
        let mut events = Vec::new();
        let mut reader = BufReader::new(&mut file);
        loop {
            let mut line = Vec::new();
            let count = reader
                .by_ref()
                .take(MAX_RECORD_BYTES as u64 + 1)
                .read_until(b'\n', &mut line)?;
            if count == 0 {
                break;
            }
            if line.len() > MAX_RECORD_BYTES || line.last() != Some(&b'\n') {
                return Err(ApiError::unavailable(
                    "operation journal contains an oversized or incomplete record",
                ));
            }
            let raw: serde_json::Value = serde_json::from_slice(&line)?;
            let fields = raw.as_object().ok_or_else(|| {
                ApiError::unavailable("operation journal record must be an object")
            })?;
            let expected = [
                "id",
                "kind",
                "status",
                "updatedAt",
                "result",
                "error",
                "auto_retry",
                "context",
            ];
            if fields.len() != expected.len()
                || expected.iter().any(|key| !fields.contains_key(*key))
            {
                return Err(ApiError::unavailable(
                    "operation journal record fields do not match this version",
                ));
            }
            let value: Operation = serde_json::from_value(raw)?;
            validate_transition(records.get(&value.id), &value)?;
            if (!records.contains_key(&value.id) && records.len() >= MAX_OPERATIONS)
                || events.len() >= MAX_EVENTS
            {
                return Err(ApiError::unavailable("operation journal capacity exceeded"));
            }
            records.insert(value.id.clone(), value.clone());
            events.push(value);
        }
        let mut journal = Self {
            file,
            records,
            events,
            bytes,
            failed: false,
        };
        let interrupted: Vec<_> = journal
            .records
            .values()
            .filter(|v| v.status == "running")
            .cloned()
            .collect();
        for mut op in interrupted {
            op.status = "unknown".into();
            op.error = Some("host restarted before completion; do not automatically retry".into());
            op.updated_at = timestamp();
            journal.append(op)?;
        }
        Ok(journal)
    }

    pub fn append(&mut self, value: Operation) -> Result<(), ApiError> {
        if self.failed {
            return Err(ApiError::unavailable("operation journal unavailable"));
        }
        validate_transition(self.records.get(&value.id), &value)?;
        let mut line = serde_json::to_vec(&value)?;
        line.push(b'\n');
        if line.len() > MAX_RECORD_BYTES
            || self.bytes + line.len() as u64 > MAX_JOURNAL_BYTES
            || self.events.len() >= MAX_EVENTS
            || (!self.records.contains_key(&value.id) && self.records.len() >= MAX_OPERATIONS)
        {
            return Err(ApiError::unavailable("operation journal capacity exceeded"));
        }
        if let Err(error) = self
            .file
            .write_all(&line)
            .and_then(|()| self.file.sync_data())
        {
            self.failed = true;
            return Err(error.into());
        }
        self.bytes += line.len() as u64;
        self.records.insert(value.id.clone(), value.clone());
        self.events.push(value);
        Ok(())
    }
}

fn validate_transition(previous: Option<&Operation>, value: &Operation) -> Result<(), ApiError> {
    if !super::valid_id(&value.id)
        || !super::valid_id(&value.kind)
        || !value.context.is_object()
        || value.auto_retry
        || value.updated_at == 0
        || !matches!(
            value.status.as_str(),
            "running" | "completed" | "failed" | "canceled" | "unknown"
        )
    {
        return Err(ApiError::unavailable("invalid operation journal record"));
    }
    if value.status == "running" {
        if previous.is_some() || value.result.is_some() || value.error.is_some() {
            return Err(ApiError::unavailable(
                "operation journal cannot replay a call or attach a result before completion",
            ));
        }
    } else {
        let previous = previous.ok_or_else(|| {
            ApiError::unavailable("operation journal completion has no accepted intent")
        })?;
        if previous.status != "running"
            || previous.kind != value.kind
            || previous.context != value.context
        {
            return Err(ApiError::unavailable(
                "operation journal changes identity or an already terminal outcome",
            ));
        }
        if (value.status != "unknown" && (value.result.is_none() || value.error.is_some()))
            || (value.status == "unknown" && value.result.is_none() && value.error.is_none())
        {
            return Err(ApiError::unavailable(
                "operation journal outcome has no consistent result or error",
            ));
        }
        if let Some(status) = value
            .result
            .as_ref()
            .and_then(|result| result.get("status"))
            .and_then(serde_json::Value::as_str)
        {
            let expected = match status {
                "unknown" => "unknown",
                "failed" => "failed",
                "canceled" => "canceled",
                _ => "completed",
            };
            if value.status != expected {
                return Err(ApiError::unavailable(
                    "operation journal result disagrees with its terminal status",
                ));
            }
        }
    }
    Ok(())
}
