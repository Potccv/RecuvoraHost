//! External UI delivery is a fixed startup snapshot, separate from authenticated APIs.
use super::{AUTH, call, config};
use crate::server::{Console, router};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ASSETS: &[&str] = &[
    "index.html",
    "styles.css",
    "app.js",
    "api.js",
    "dom.js",
    "history.js",
    "monitoring.js",
    "plugin-monitoring.js",
    "project-logs.js",
    "refresh.js",
    "data.js",
    "shell.js",
    "favicon.svg",
    "favicon.ico",
];

fn write_assets(directory: &Path) {
    std::fs::create_dir(directory).unwrap();
    for name in ASSETS {
        std::fs::write(directory.join(name), format!("isolated asset: {name}")).unwrap();
    }
}

async fn get(address: std::net::SocketAddr, path: &str) -> (u16, String, String) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        let response = String::from_utf8(bytes).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        (
            headers.split_whitespace().nth(1).unwrap().parse().unwrap(),
            headers.to_ascii_lowercase(),
            body.into(),
        )
    })
    .await
    .expect("static HTTP request exceeded its deadline")
}

#[tokio::test]
async fn external_ui_serves_only_catalog_snapshot_and_keeps_api_authentication() {
    let mut cfg = config("ui-snapshot");
    let directory = cfg.data_dir.clone();
    let assets = directory.join("ui");
    write_assets(&assets);
    std::fs::write(directory.join("outside.txt"), "outside-secret").unwrap();
    std::fs::write(assets.join("private.txt"), "uncatalogued-secret").unwrap();
    cfg.ui_dir = Some(assets.clone());
    let (state, engine) = Console::open(cfg).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (status, headers, body) = get(address, "/").await;
    assert_eq!(status, 200);
    assert_eq!(body, "isolated asset: index.html");
    assert!(headers.contains("content-type: text/html; charset=utf-8"));
    assert!(headers.contains("x-content-type-options: nosniff"));
    assert!(headers.contains("cache-control: no-cache"));
    let (status, headers, body) = get(address, "/api.js").await;
    assert_eq!(status, 200);
    assert!(headers.contains("content-type: text/javascript; charset=utf-8"));
    assert_eq!(body, "isolated asset: api.js");
    std::fs::write(assets.join("index.html"), "changed after startup").unwrap();
    assert_eq!(get(address, "/").await.2, "isolated asset: index.html");

    for path in [
        "/private.txt",
        "/missing.js",
        "/%2e%2e%2foutside.txt",
        "/%2e%2e%5coutside.txt",
    ] {
        let (status, _, body) = get(address, path).await;
        assert_eq!(status, 404, "unexpected static route result for {path}");
        assert!(
            !body.contains("secret"),
            "private file leaked through {path}"
        );
    }
    assert_eq!(
        call(address, "GET", "/api/v1/bootstrap", "", "").await.0,
        401
    );
    assert_eq!(
        call(address, "GET", "/api/v1/bootstrap", AUTH, "").await.0,
        200
    );

    server.abort();
    let _ = server.await;
    state.shutdown().await.unwrap();
    engine.shutdown().await.unwrap();
    drop(state);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn unconfigured_ui_returns_not_found_while_http_api_remains_available() {
    let cfg = config("without-ui");
    let directory = cfg.data_dir.clone();
    let (state, engine) = Console::open(cfg).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    assert_eq!(get(address, "/").await.0, 404);
    assert_eq!(get(address, "/index.html").await.0, 404);
    assert_eq!(
        call(address, "GET", "/api/v1/bootstrap", AUTH, "").await.0,
        200
    );
    server.abort();
    let _ = server.await;
    state.shutdown().await.unwrap();
    engine.shutdown().await.unwrap();
    drop(state);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn ui_configuration_rejects_missing_and_oversized_assets_before_serving() {
    let mut cfg = config("invalid-ui");
    let directory = cfg.data_dir.clone();
    let assets = directory.join("ui");
    write_assets(&assets);
    cfg.ui_dir = Some(assets.clone());
    std::fs::remove_file(assets.join("api.js")).unwrap();
    assert!(Console::open(cfg.clone()).await.is_err());
    std::fs::write(assets.join("api.js"), vec![b'x'; 2 * 1024 * 1024 + 1]).unwrap();
    assert!(Console::open(cfg.clone()).await.is_err());
    for name in ["api.js", "app.js", "dom.js", "history.js"] {
        std::fs::write(assets.join(name), vec![b'x'; 2 * 1024 * 1024]).unwrap();
    }
    assert!(Console::open(cfg).await.is_err());
    std::fs::remove_dir_all(directory).unwrap();
}
