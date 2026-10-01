mod extension_fixture;
mod network_peer;
use extension_fixture::{FixtureServers, TestResult, definition, definition_with_environment};
use recuvora_host::harnesses::HarnessCancellation;
use recuvora_host::integrations::extensions::{
    ExtensionError, ExtensionKind, validate_schema, validate_value,
};
use recuvora_host::integrations::extensions::{
    ExtensionRegistry, ExtensionsConfig, MONITORING_VIEW_METHOD, MonitoringViewRegistration,
};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn main() -> TestResult {
    let fixtures = FixtureServers::default();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(&fixtures))?;
    fixtures.shutdown()
}
async fn run(fixtures: &FixtureServers) -> TestResult {
    let plugin = definition(fixtures, "logs-plugin", ExtensionKind::Plugin, "good")?;
    let node = definition(fixtures, "logs-node", ExtensionKind::Node, "good")?;
    let registry = ExtensionRegistry::connect(ExtensionsConfig {
        schema_version: 1,
        extensions: vec![node.clone(), plugin.clone()],
    })
    .await?;
    assert_eq!(
        registry.contract_owner("com.example.logs", 1),
        Some("logs-plugin"),
        "the consumer plugin owns the contract even when a node provides it: {:?}",
        registry.statuses()
    );
    assert_eq!(
        registry.monitoring_view_registration("logs-plugin")?,
        None,
        "plugins without the optional marker remain compatible"
    );
    let result = registry
        .call_read_only(
            "logs-plugin",
            "com.example.logs",
            1,
            "query",
            json!({"needle":"error"}),
            Duration::from_secs(5),
            HarnessCancellation::new(),
        )
        .await?;
    assert_eq!(
        result,
        json!({"entries":["consumed:error"]}),
        "independently loaded plugin must consume node result"
    );
    assert!(
        registry
            .call_read_only(
                "logs-plugin",
                "com.example.logs",
                1,
                "unknown",
                json!({}),
                Duration::from_secs(5),
                HarnessCancellation::new()
            )
            .await
            .is_err()
    );
    assert!(
        registry
            .call_read_only(
                "logs-plugin",
                "com.example.logs",
                1,
                "query",
                json!({"needle":true}),
                Duration::from_secs(5),
                HarnessCancellation::new()
            )
            .await
            .is_err()
    );
    assert!(
        ExtensionRegistry::connect(ExtensionsConfig {
            schema_version: 1,
            extensions: vec![node.clone()]
        })
        .await?
        .metadata("logs-node")
        .is_none(),
        "a node cannot invent a business contract"
    );
    for mode in ["bad-version", "bad-identity", "bad-schema"] {
        let bad = definition(fixtures, "logs-plugin", ExtensionKind::Plugin, mode)?;
        assert!(
            ExtensionRegistry::connect(ExtensionsConfig {
                schema_version: 1,
                extensions: vec![bad]
            })
            .await?
            .metadata("logs-plugin")
            .is_none(),
            "reject {mode}"
        );
    }
    let mut reserved = plugin.clone();
    reserved.namespaces = vec!["recuvora".into()];
    assert!(
        ExtensionsConfig {
            schema_version: 1,
            extensions: vec![reserved]
        }
        .validate()
        .is_err()
    );
    let mut denied = plugin.clone();
    denied.allow_calls.clear();
    let denied = ExtensionRegistry::connect(ExtensionsConfig {
        schema_version: 1,
        extensions: vec![denied, node.clone()],
    })
    .await?;
    assert!(
        denied
            .call_read_only(
                "logs-plugin",
                "com.example.logs",
                1,
                "query",
                json!({"needle":"x"}),
                Duration::from_secs(5),
                HarnessCancellation::new()
            )
            .await
            .is_err()
    );
    let view = definition(fixtures, "view-plugin", ExtensionKind::Plugin, "view-good")?;
    let views = ExtensionRegistry::connect(ExtensionsConfig {
        schema_version: 1,
        extensions: vec![view],
    })
    .await?;
    let view_registration = MonitoringViewRegistration {
        extension_id: "view-plugin".into(),
        contract: "com.example.logs.monitoring_view".into(),
        version: 1,
        method: MONITORING_VIEW_METHOD.into(),
    };
    assert_eq!(
        views.monitoring_view_registration("view-plugin")?,
        Some(view_registration.clone())
    );
    assert_eq!(
        views.contract_owner("com.example.logs.monitoring_view", 1),
        Some("view-plugin")
    );
    assert_eq!(
        serde_json::to_value(view_registration.clone())?,
        json!({
            "extension_id":"view-plugin",
            "contract":"com.example.logs.monitoring_view",
            "version":1,
            "method":"describe_monitoring_view"
        })
    );
    assert_eq!(
        views
            .call_monitoring_view(
                &view_registration,
                json!({"schema_version":1}),
                Duration::from_secs(5),
                HarnessCancellation::new(),
            )
            .await?,
        json!({"schema_version":1,"title":"Fixture monitoring","sections":[]})
    );
    assert!(
        views
            .call_monitoring_view(
                &view_registration,
                json!({"schema_version":2}),
                Duration::from_secs(5),
                HarnessCancellation::new(),
            )
            .await
            .is_err(),
        "descriptor input is validated against the registered schema"
    );
    let bad_result_view = definition(
        fixtures,
        "view-plugin",
        ExtensionKind::Plugin,
        "view-bad-result",
    )?;
    let bad_result_views = ExtensionRegistry::connect(ExtensionsConfig {
        schema_version: 1,
        extensions: vec![bad_result_view],
    })
    .await?;
    let bad_result_registration = bad_result_views
        .monitoring_view_registration("view-plugin")?
        .expect("bad-result fixture registers a valid descriptor");
    assert!(
        matches!(
            bad_result_views
                .call_monitoring_view(
                    &bad_result_registration,
                    json!({"schema_version":1}),
                    Duration::from_secs(5),
                    HarnessCancellation::new(),
                )
                .await,
            Err(ExtensionError::Protocol(_))
        ),
        "descriptor output is validated against the registered schema"
    );
    let mut forged_registration = view_registration.clone();
    forged_registration.method = "query".into();
    assert!(matches!(
        views
            .call_monitoring_view(
                &forged_registration,
                json!({"schema_version":1}),
                Duration::from_secs(5),
                HarnessCancellation::new(),
            )
            .await,
        Err(ExtensionError::Rejected(_))
    ));
    let callback_view = definition(
        fixtures,
        "view-plugin",
        ExtensionKind::Plugin,
        "view-callback",
    )?;
    let callback_views = ExtensionRegistry::connect(ExtensionsConfig {
        schema_version: 1,
        extensions: vec![callback_view],
    })
    .await?;
    let callback_registration = callback_views
        .monitoring_view_registration("view-plugin")?
        .expect("callback fixture registers a valid descriptor");
    assert!(
        callback_views
            .call_monitoring_view(
                &callback_registration,
                json!({"schema_version":1}),
                Duration::from_secs(5),
                HarnessCancellation::new(),
            )
            .await
            .is_err(),
        "monitoring view descriptors cannot use the plugin node-read callback"
    );
    let root =
        PathBuf::from(std::env::var_os("RECUVORA_TEST_TEMP").ok_or("external test root required")?);
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    assert!(!root.starts_with(Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize()?));
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let directory = root.join(format!(
        "extension-view-capacity-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&directory)?;
    let marker = directory.join("descriptor-started");
    let isolated_view = definition_with_environment(
        fixtures,
        "view-plugin",
        ExtensionKind::Plugin,
        "view-wait-cancel",
        std::collections::BTreeMap::from([(
            "RECUVORA_VIEW_STARTED".into(),
            marker
                .to_str()
                .ok_or("view capacity marker path must be UTF-8")?
                .into(),
        )]),
    )?;
    let isolated_views = ExtensionRegistry::connect(ExtensionsConfig {
        schema_version: 1,
        extensions: vec![
            isolated_view,
            definition(fixtures, "logs-node", ExtensionKind::Node, "good")?,
        ],
    })
    .await?;
    let isolated_registration = isolated_views
        .monitoring_view_registration("view-plugin")?
        .expect("wait fixture registers a descriptor");
    let view_cancellation = HarnessCancellation::new();
    let pending_registry = isolated_views.clone();
    let pending_cancellation = view_cancellation.clone();
    let pending_view = tokio::spawn(async move {
        pending_registry
            .call_monitoring_view(
                &isolated_registration,
                json!({"schema_version":1}),
                Duration::from_secs(5),
                pending_cancellation,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !tokio::fs::try_exists(&marker).await? {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(|_| "monitoring view descriptor did not start")??;
    let ordinary = |needle: &'static str| {
        isolated_views.call_read_only(
            "view-plugin",
            "com.example.logs",
            1,
            "query",
            json!({"needle":needle}),
            Duration::from_secs(5),
            HarnessCancellation::new(),
        )
    };
    let results = tokio::join!(
        ordinary("one"),
        ordinary("two"),
        ordinary("three"),
        ordinary("four")
    );
    for result in [results.0, results.1, results.2, results.3] {
        assert!(
            result.is_ok(),
            "a monitoring view read consumed ordinary call capacity: {result:?}"
        );
    }
    view_cancellation.cancel();
    assert!(pending_view.await?.is_err());
    isolated_views.shutdown().await?;
    assert_eq!(directory.parent(), Some(root.as_path()));
    std::fs::remove_dir_all(&directory)?;
    for mode in ["view-no-allow", "view-duplicate", "view-not-read-only"] {
        let plugin = definition(fixtures, "view-plugin", ExtensionKind::Plugin, mode)?;
        let registry = ExtensionRegistry::connect(ExtensionsConfig {
            schema_version: 1,
            extensions: vec![plugin],
        })
        .await?;
        assert!(
            registry.metadata("view-plugin").is_some(),
            "an invalid optional view must not disable the plugin: {mode}"
        );
        assert!(
            matches!(
                registry.monitoring_view_registration("view-plugin"),
                Err(ExtensionError::Rejected(_))
            ),
            "reject invalid optional view registration: {mode}"
        );
    }
    let bad = definition(fixtures, "logs-node", ExtensionKind::Node, "bad-result")?;
    let bad = ExtensionRegistry::connect(ExtensionsConfig {
        schema_version: 1,
        extensions: vec![plugin.clone(), bad],
    })
    .await?;
    assert!(
        bad.call_read_only(
            "logs-node",
            "com.example.logs",
            1,
            "query",
            json!({"needle":"x"}),
            Duration::from_secs(5),
            HarnessCancellation::new()
        )
        .await
        .is_err()
    );
    for mode in ["disconnect", "wait-cancel"] {
        let node = definition(fixtures, "logs-node", ExtensionKind::Node, mode)?;
        let registry = ExtensionRegistry::connect(ExtensionsConfig {
            schema_version: 1,
            extensions: vec![plugin.clone(), node],
        })
        .await?;
        let result = registry
            .call_read_only(
                "logs-node",
                "com.example.logs",
                1,
                "query",
                json!({"needle":"x"}),
                Duration::from_millis(40),
                HarnessCancellation::new(),
            )
            .await;
        assert!(
            matches!(result, Err(ExtensionError::Unknown { .. })),
            "submitted {mode} must retain Unknown"
        );
    }
    assert!(validate_schema(&json!({"type":"object","oneOf":[]})).is_err());
    assert!(
        validate_value(
            &json!({"type":"array","maxItems":1,"items":{"type":"integer"}}),
            &json!([1, 2])
        )
        .is_err()
    );
    assert!(
        validate_value(
            &json!({"type":"integer","maximum":9007199254740992_u64}),
            &json!(9007199254740993_u64)
        )
        .is_err()
    );
    let oversized = ExtensionRegistry::connect(ExtensionsConfig {
        schema_version: 1,
        extensions: vec![
            plugin,
            definition(
                fixtures,
                "logs-node",
                ExtensionKind::Node,
                "oversized-frame",
            )?,
        ],
    })
    .await?;
    assert!(
        matches!(
            oversized
                .call_read_only(
                    "logs-node",
                    "com.example.logs",
                    1,
                    "query",
                    json!({"needle":"bounded"}),
                    Duration::from_secs(5),
                    HarnessCancellation::new(),
                )
                .await,
            Err(ExtensionError::Unknown { .. })
        ),
        "oversized WebSocket frames must fail the dispatched call"
    );
    oversized.shutdown().await?;
    println!(
        "extensions: plugin loading, schema ownership, read routing, rejection, disconnect, cancellation and bounds passed"
    );
    Ok(())
}
