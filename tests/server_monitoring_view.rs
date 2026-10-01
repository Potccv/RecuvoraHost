use super::*;

fn declaration() -> Value {
    json!({
        "schema_version":1,
        "title":"Example",
        "summary":"Host-owned samples only",
        "sections":[{
            "id":"status",
            "title":"Status",
            "monitor_role":"primary",
            "fields":[{
                "id":"ready",
                "label":"Ready",
                "source":"last_value",
                "pointer":"/ready",
                "format":"boolean",
                "empty":"Not observed"
            }]
        }]
    })
}

#[test]
fn plugin_view_schema_is_closed_and_bounded() {
    assert_eq!(bounded_error("bad\0message"), "bad�message");
    assert_eq!(
        monitoring_view_error(ExtensionError::Protocol("bad descriptor".into())).0,
        "invalid_response"
    );
    assert_eq!(
        monitoring_view_error(ExtensionError::Unavailable("offline".into())).0,
        "unavailable"
    );
    assert!(validate_view(declaration()).is_ok());

    let mut omitted = declaration();
    omitted.as_object_mut().unwrap().remove("summary");
    omitted["sections"][0]
        .as_object_mut()
        .unwrap()
        .remove("description");
    omitted["sections"][0]["fields"][0]
        .as_object_mut()
        .unwrap()
        .remove("empty");
    let raw_size = serde_json::to_vec(&omitted).unwrap().len();
    let normalized = validate_view(omitted).unwrap();
    let normalized_value = serde_json::to_value(&normalized).unwrap();
    assert!(normalized_value.get("summary").is_none());
    assert!(normalized_value["sections"][0].get("description").is_none());
    assert!(
        normalized_value["sections"][0]["fields"][0]
            .get("empty")
            .is_none()
    );
    assert!(serde_json::to_vec(&normalized).unwrap().len() <= raw_size);

    let mut unknown = declaration();
    unknown["html"] = json!("<script>ignored</script>");
    assert!(validate_view(unknown).is_err());

    let mut pointer = declaration();
    pointer["sections"][0]["fields"][0]["pointer"] = json!("/bad~2escape");
    assert!(validate_view(pointer).is_err());

    let mut duplicate = declaration();
    let section = duplicate["sections"][0].clone();
    duplicate["sections"].as_array_mut().unwrap().push(section);
    assert!(validate_view(duplicate).is_err());

    let mut oversized = declaration();
    oversized["summary"] = json!("x".repeat(1025));
    assert!(validate_view(oversized).is_err());
}
