//! Language-neutral v1 contract vectors and schema boundary regressions.
use recuvora_host::protocol::{Message, valid_id, validate_schema, validate_value};
use serde_json::{Value, json};

#[test]
fn documented_wire_vectors_remain_compatible() {
    let vectors: Value =
        serde_json::from_str(include_str!("../docs/extensions/examples/protocol.json")).unwrap();
    for value in vectors["valid"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(message).unwrap(), *value);
    }
    for value in vectors["invalid"].as_array().unwrap() {
        assert!(serde_json::from_value::<Message>(value.clone()).is_err());
    }
}

#[test]
fn ready_defaults_are_flat_and_unknown_is_preserved() {
    let ready: Message = serde_json::from_value(json!({
        "type": "ready", "protocol_version": 1, "id": "example-node",
        "kind": "node", "contracts": []
    }))
    .unwrap();
    let encoded = serde_json::to_value(ready).unwrap();
    assert_eq!(encoded["capabilities"], json!([]));
    assert_eq!(encoded["workspaces"], json!([]));
    assert!(encoded.get("metadata").is_none());
    let unknown: Message = serde_json::from_value(json!({
        "type": "error", "id": "call-001", "code": "lost_receipt",
        "message": "uncertain", "outcome": "unknown"
    }))
    .unwrap();
    assert_eq!(serde_json::to_value(unknown).unwrap()["outcome"], "unknown");
}

#[test]
fn ids_enforce_documented_ascii_and_length_limits() {
    for id in ["", "contains space", "a/b", "目标"] {
        assert!(!valid_id(id), "{id}");
    }
    assert!(valid_id("a.A_0-9"));
    assert!(valid_id(&"a".repeat(128)));
    assert!(!valid_id(&"a".repeat(129)));
}

#[test]
fn schema_enforces_required_extra_properties_and_unicode_length() {
    let schema = json!({
        "type": "object", "properties": {"value": {"type": "string", "maxLength": 2}},
        "required": ["value"], "additionalProperties": false
    });
    assert!(validate_value(&schema, &json!({"value": "目标"})).is_ok());
    for value in [
        json!({}),
        json!({"value": 2}),
        json!({"value": "目标值"}),
        json!({"value": "ok", "extra": true}),
    ] {
        assert!(validate_value(&schema, &value).is_err());
    }
    assert!(validate_schema(&json!({"type": "object", "$ref": "remote"})).is_err());
    assert!(validate_value(&json!({"type": "array", "maxItems": 0}), &json!([1])).is_err());
}

#[test]
fn full_value_depth_is_bounded_even_without_property_schemas() {
    let schema = json!({"type": "object"});
    let mut value = json!(null);
    for _ in 0..24 {
        value = json!({"child": value});
    }
    assert!(validate_value(&schema, &value).is_ok());
    value = json!({"child": value});
    assert!(validate_value(&schema, &value).is_err());
}

#[test]
fn bounded_numbers_do_not_accept_imprecise_large_integers() {
    let schema = json!({"type": "integer", "minimum": -9007199254740992_i64,
        "maximum": 9007199254740992_i64});
    assert!(validate_value(&schema, &json!(9007199254740992_i64)).is_ok());
    assert!(validate_value(&schema, &json!(9007199254740993_i64)).is_err());
}
