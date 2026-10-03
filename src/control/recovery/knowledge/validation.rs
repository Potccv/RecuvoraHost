//! Bounded neutral content; concrete action formats belong to the caller.
use super::*;
use std::collections::BTreeSet;

pub(super) fn text(value: &str, max: usize, label: &str) -> Result<(), KnowledgeError> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        return Err(KnowledgeError::Invalid(format!(
            "{label} is empty or exceeds bounds"
        )));
    }
    Ok(())
}

fn strings(
    values: &[String],
    max_items: usize,
    max_bytes: usize,
    label: &str,
) -> Result<(), KnowledgeError> {
    if values.len() > max_items {
        return Err(KnowledgeError::Invalid(format!("too many {label}")));
    }
    let mut seen = BTreeSet::new();
    for value in values {
        text(value, max_bytes, label)?;
        if !seen.insert(value) {
            return Err(KnowledgeError::Invalid(format!("duplicate {label}")));
        }
    }
    Ok(())
}

pub(super) fn evidence(values: &[String]) -> Result<(), KnowledgeError> {
    if values.is_empty() {
        return Err(KnowledgeError::Invalid(
            "at least one evidence reference required".into(),
        ));
    }
    strings(values, 32, 1024, "evidence references")
}

pub(super) fn artifact(value: &RepairArtifact) -> Result<(), KnowledgeError> {
    Ok(value.validate()?)
}
pub(super) fn query(value: &KnowledgeQuery) -> Result<(), KnowledgeError> {
    Ok(value.validate()?)
}
