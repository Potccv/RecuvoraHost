//! Bounded text and JSON validation without unbounded serialization copies.
use super::IncidentError;
use serde_json::Value;
use std::io::Write;

pub(super) fn bounded_text(
    value: &str,
    max: usize,
    field: &str,
    empty: bool,
) -> Result<(), IncidentError> {
    if value.len() > max || (!empty && value.trim().is_empty()) || value.contains('\0') {
        return Err(IncidentError::Invalid(format!(
            "{field} is empty, contains NUL or exceeds {max} bytes"
        )));
    }
    Ok(())
}

pub(super) fn bounded_object(value: &Value, max: usize, field: &str) -> Result<(), IncidentError> {
    if !value.is_object() {
        return Err(IncidentError::Invalid(format!(
            "{field} must be a JSON object"
        )));
    }
    // Bound nesting before recursive serialization, and count encoded bytes
    // without allocating a second potentially unbounded copy of caller data.
    let mut pending = vec![(value, 0usize)];
    let mut nodes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if depth > 32 || nodes > max {
            return Err(IncidentError::Capacity(format!("{field} JSON complexity")));
        }
        match value {
            Value::Array(values) => {
                if values.len() > max.saturating_sub(pending.len()) {
                    return Err(IncidentError::Capacity(format!("{field} JSON complexity")));
                }
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                if values.len() > max.saturating_sub(pending.len())
                    || values.keys().any(|key| key.len() > max)
                {
                    return Err(IncidentError::Capacity(format!("{field} JSON complexity")));
                }
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            Value::String(value) if value.len() > max => {
                return Err(IncidentError::Capacity(format!(
                    "{field} exceeds {max} bytes"
                )));
            }
            _ => {}
        }
    }
    serde_json::to_writer(ByteLimit { remaining: max }, value)
        .map_err(|_| IncidentError::Capacity(format!("{field} exceeds {max} bytes")))?;
    Ok(())
}

struct ByteLimit {
    remaining: usize,
}

impl Write for ByteLimit {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(std::io::Error::other("JSON byte limit"));
        }
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
