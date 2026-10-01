//! Deliberately small schema subset, never advertised as full JSON Schema.

use super::ExtensionError;
use serde_json::Value;

pub fn validate_schema(schema: &Value) -> Result<(), ExtensionError> {
    schema_at(schema, 0)
}

fn bad(message: &str) -> ExtensionError {
    ExtensionError::Rejected(message.into())
}

fn schema_at(schema: &Value, depth: usize) -> Result<(), ExtensionError> {
    if depth > 12 || schema.to_string().len() > 64 * 1024 {
        return Err(bad("schema limit exceeded"));
    }
    let object = schema
        .as_object()
        .ok_or_else(|| bad("schema must be an object"))?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "type"
                | "properties"
                | "required"
                | "additionalProperties"
                | "items"
                | "enum"
                | "maxLength"
                | "maxItems"
                | "minimum"
                | "maximum"
                | "description"
        ) {
            return Err(bad("unsupported schema keyword"));
        }
    }
    let kind = schema
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("schema requires one string type"))?;
    if !matches!(
        kind,
        "object" | "array" | "string" | "integer" | "number" | "boolean" | "null"
    ) {
        return Err(bad("unsupported schema type"));
    }
    if let Some(properties) = schema.get("properties") {
        if kind != "object" {
            return Err(bad("properties require object"));
        }
        let props = properties
            .as_object()
            .ok_or_else(|| bad("properties must be object"))?;
        if props.len() > 64 {
            return Err(bad("too many properties"));
        }
        for child in props.values() {
            schema_at(child, depth + 1)?;
        }
    }
    if let Some(required) = schema.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| bad("required must be array"))?;
        if kind != "object"
            || required.len() > 64
            || required.iter().any(|v| {
                v.as_str().is_none()
                    || schema
                        .get("properties")
                        .and_then(|p| p.get(v.as_str().unwrap_or("")))
                        .is_none()
            })
        {
            return Err(bad("required references undefined property"));
        }
    }
    if let Some(additional) = schema.get("additionalProperties")
        && (kind != "object" || !additional.is_boolean())
    {
        return Err(bad("additionalProperties must be boolean"));
    }
    if let Some(items) = schema.get("items") {
        if kind != "array" {
            return Err(bad("items require array"));
        }
        schema_at(items, depth + 1)?;
    }
    for key in ["maxLength", "maxItems"] {
        if let Some(v) = schema.get(key)
            && (v.as_u64().is_none()
                || (key == "maxLength" && kind != "string")
                || (key == "maxItems" && kind != "array"))
        {
            return Err(bad("invalid schema bound"));
        }
    }
    for key in ["minimum", "maximum"] {
        if let Some(v) = schema.get(key)
            && (!v.is_number() || !matches!(kind, "number" | "integer") || !safe_bound(v))
        {
            return Err(bad("invalid numeric bound"));
        }
    }
    if let Some(enumeration) = schema.get("enum")
        && enumeration
            .as_array()
            .is_none_or(|a| a.is_empty() || a.len() > 64)
    {
        return Err(bad("enum must contain 1..64 values"));
    }
    if schema.get("description").is_some_and(|v| !v.is_string()) {
        return Err(bad("description must be text"));
    }
    Ok(())
}

pub fn validate_value(schema: &Value, value: &Value) -> Result<(), ExtensionError> {
    validate_schema(schema)?;
    depth_check(value, 0)?;
    value_at(schema, value, 0)
}

fn depth_check(value: &Value, depth: usize) -> Result<(), ExtensionError> {
    if depth > 24 {
        return Err(bad("value nesting exceeds 24 levels"));
    }
    match value {
        Value::Object(values) => {
            for value in values.values() {
                depth_check(value, depth + 1)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                depth_check(value, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn value_at(schema: &Value, value: &Value, depth: usize) -> Result<(), ExtensionError> {
    if depth > 24 || value.to_string().len() > 256 * 1024 {
        return Err(bad("value limit exceeded"));
    }
    let kind = schema["type"].as_str().unwrap_or("");
    let matches = match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    };
    if !matches {
        return Err(bad("value type does not match schema"));
    }
    if (schema.get("minimum").is_some() || schema.get("maximum").is_some()) && !safe_bound(value) {
        return Err(bad(
            "bounded numeric values must be within the exact integer range of binary64",
        ));
    }
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|e| !e.contains(value))
    {
        return Err(bad("value outside enum"));
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required {
                if !object.contains_key(key.as_str().unwrap_or("")) {
                    return Err(bad("missing required property"));
                }
            }
        }
        for (key, child) in object {
            if let Some(child_schema) = schema.get("properties").and_then(|p| p.get(key)) {
                value_at(child_schema, child, depth + 1)?;
            } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                return Err(bad("unknown property"));
            }
        }
    }
    if let Some(array) = value.as_array() {
        if schema
            .get("maxItems")
            .and_then(Value::as_u64)
            .is_some_and(|n| array.len() as u64 > n)
        {
            return Err(bad("array too long"));
        }
        if let Some(items) = schema.get("items") {
            for child in array {
                value_at(items, child, depth + 1)?;
            }
        }
    }
    if let Some(text) = value.as_str()
        && schema
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|n| text.chars().count() as u64 > n)
    {
        return Err(bad("string too long"));
    }
    if let Some(number) = value.as_f64()
        && (schema
            .get("minimum")
            .and_then(Value::as_f64)
            .is_some_and(|n| number < n)
            || schema
                .get("maximum")
                .and_then(Value::as_f64)
                .is_some_and(|n| number > n))
    {
        return Err(bad("number outside bounds"));
    }
    Ok(())
}

fn safe_bound(value: &Value) -> bool {
    const EXACT: u64 = 1_u64 << 53;
    if let Some(number) = value.as_u64() {
        return number <= EXACT;
    }
    if let Some(number) = value.as_i64() {
        return number.unsigned_abs() <= EXACT;
    }
    value
        .as_f64()
        .is_some_and(|number| number.is_finite() && number.abs() <= EXACT as f64)
}
