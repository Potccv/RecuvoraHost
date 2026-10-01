//! HTTP and static delivery contracts; does not execute the UI JavaScript client.
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::{Value, json};
use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::{Child, Command},
    time::{sleep, timeout},
};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;
const DEADLINE: Duration = Duration::from_secs(10);
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

#[tokio::test]
async fn http_and_static_contract_survives_restart() -> TestResult {
    run(None).await
}

#[tokio::test]
#[ignore = "requires separately built UI assets in RECUVORA_UI_DIST"]
async fn external_ui_assets_and_http_contract() -> TestResult {
    let ui = fs::canonicalize(std::env::var_os("RECUVORA_UI_DIST").ok_or("set RECUVORA_UI_DIST")?)?;
    run(Some(ui)).await
}

async fn run(ui: Option<PathBuf>) -> TestResult {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = TestDirectory::create()?;
    let ui = match ui {
        Some(ui) => ui,
        None => {
            let ui = directory.path.join("ui");
            fs::create_dir(&ui)?;
            for name in ASSETS {
                fs::write(ui.join(name), format!("isolated asset: {name}"))?;
            }
            ui
        }
    };
    let mut random = [0; 32];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut random)
        .map_err(|_| io::Error::other("test token generation failed"))?;
    let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let token_path = directory.path.join("token");
    fs::write(&token_path, &token)?;
    let config = directory.path.join("console.json");
    fs::write(
        &config,
        serde_json::to_vec(&json!({
            "schema_version": 1, "listen": "127.0.0.1:0", "token_file": token_path,
            "operator": "integration-operator", "data_dir": directory.path, "ui_dir": ui,
            "permissions": ["simulation.run", "logs.read", "operation.cancel"], "allowed_origins": []
        }))?,
    )?;
    let mut host = Host::start(&config).await?;
    // Always await child exit before removing its data, including failed checks.
    let result = timeout(
        Duration::from_secs(60),
        check(&mut host, &config, &ui, &token),
    )
    .await;
    let stopped = host.stop().await;
    result??;
    stopped?;
    directory.clean()?;
    Ok(())
}

async fn check(host: &mut Host, config: &Path, ui: &Path, token: &str) -> TestResult {
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(DEADLINE)
        .build()?;
    let get = |route: &str| {
        client
            .get(format!("{}{route}", host.address))
            .bearer_auth(token)
    };
    let bootstrap = json_response(get("/api/v1/bootstrap"), StatusCode::OK).await?;
    require(bootstrap["mode"] == "live", "bootstrap mode")?;
    json_response(
        client.get(format!("{}/api/v1/bootstrap", host.address)),
        StatusCode::UNAUTHORIZED,
    )
    .await?;
    json_response(
        get("/api/v1/bootstrap").header("Origin", "https://invalid.example"),
        StatusCode::FORBIDDEN,
    )
    .await?;
    for name in ASSETS {
        let route = if *name == "index.html" { "" } else { name };
        let response = client
            .get(format!("{}/{route}", host.address))
            .send()
            .await?;
        require(
            response.status() == StatusCode::OK,
            &format!("static status: {name}"),
        )?;
        require(
            response
                .headers()
                .get("x-content-type-options")
                .is_some_and(|v| v == "nosniff"),
            "static nosniff",
        )?;
        let bytes = bounded_body(response).await?;
        require(
            bytes == fs::read(ui.join(name))?,
            &format!("static bytes: {name}"),
        )?;
    }
    require(
        client
            .get(format!("{}/console.json", host.address))
            .send()
            .await?
            .status()
            == StatusCode::NOT_FOUND,
        "configuration must not be served",
    )?;
    let input = json!({"operation_id":"ui-operation", "task_id":"ui-task", "target":"closed-simulation", "scenario":"succeed", "timeout_ms":1000});
    let accepted = submit(&client, host, token, &input, StatusCode::ACCEPTED).await?;
    require(
        accepted["operation_id"] == input["operation_id"],
        "accepted operation identity",
    )?;
    let completed = timeout(DEADLINE, async {
        loop {
            let result =
                json_response(get("/api/v1/operations/ui-operation"), StatusCode::OK).await?;
            if result["status"] != "running" {
                return Ok::<_, Box<dyn Error + Send + Sync>>(result);
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    require(
        completed["status"] == "completed" && completed["result"]["task"]["state"] == "Succeeded",
        "simulation completion",
    )?;
    let duplicate = submit(&client, host, token, &input, StatusCode::CONFLICT).await?;
    require(
        duplicate["auto_retry"] == false,
        "duplicate must not auto-retry",
    )?;
    let tasks = json_response(get("/api/v1/simulations"), StatusCode::OK).await?;
    require(tasks["items"][0]["state"] == "succeeded", "simulation list")?;
    let logs = json_response(get("/api/v1/logs?limit=10"), StatusCode::OK).await?;
    require(
        logs["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "simulation logs",
    )?;
    json_response(
        client
            .post(format!("{}/api/v1/approvals/missing/approve", host.address))
            .bearer_auth(token)
            .json(&json!({"revision":1,"reason":"test"})),
        StatusCode::FORBIDDEN,
    )
    .await?;
    let interrupted = json!({"operation_id":"interrupted", "task_id":"interrupted-task", "target":"other-target", "scenario":"hang", "timeout_ms":60000});
    submit(&client, host, token, &interrupted, StatusCode::ACCEPTED).await?;
    let running = json_response(get("/api/v1/operations/interrupted"), StatusCode::OK).await?;
    require(
        running["status"] == "running",
        "operation must be running before interruption",
    )?;
    host.stop().await?;
    *host = Host::start(config).await?;
    for (id, expected) in [("ui-operation", "completed"), ("interrupted", "unknown")] {
        let result = json_response(
            client
                .get(format!("{}/api/v1/operations/{id}", host.address))
                .bearer_auth(token),
            StatusCode::OK,
        )
        .await?;
        require(
            result["status"] == expected,
            &format!("restart receipt: {id}"),
        )?;
    }
    let duplicate = submit(&client, host, token, &interrupted, StatusCode::CONFLICT).await?;
    require(
        duplicate["auto_retry"] == false,
        "Unknown must not auto-retry",
    )
}

async fn submit(
    client: &Client,
    host: &Host,
    token: &str,
    input: &Value,
    status: StatusCode,
) -> TestResult<Value> {
    json_response(
        client
            .post(format!("{}/api/v1/simulations", host.address))
            .bearer_auth(token)
            .json(input),
        status,
    )
    .await
}

async fn json_response(request: RequestBuilder, expected: StatusCode) -> TestResult<Value> {
    let response = request.send().await?;
    let status = response.status();
    let body = bounded_body(response).await?;
    require(
        status == expected,
        &format!(
            "expected HTTP {expected}, got {status}: {}",
            String::from_utf8_lossy(&body)
        ),
    )?;
    Ok(serde_json::from_slice(&body)?)
}

async fn bounded_body(mut response: reqwest::Response) -> TestResult<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        require(
            body.len() + chunk.len() <= 2 * 1024 * 1024,
            "response size limit exceeded",
        )?;
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

struct Host {
    child: Child,
    address: String,
}
impl Host {
    async fn start(config: &Path) -> TestResult<Self> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_recuvora-host"));
        command
            .args(["serve", "--config"])
            .arg(config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let mut host = Self {
            child: command.spawn()?,
            address: String::new(),
        };
        let stdout = host.child.stdout.take().ok_or("missing Host stdout")?;
        let ready = timeout(Duration::from_secs(15), async {
            let mut lines = BufReader::new(stdout.take(16384)).lines();
            while let Some(line) = lines.next_line().await? {
                if let Some((_, suffix)) = line.split_once("listening on ") {
                    let address = suffix
                        .split_whitespace()
                        .next()
                        .ok_or("missing listen address")?;
                    let socket = address
                        .strip_prefix("http://")
                        .ok_or("invalid listen URL")?
                        .parse::<std::net::SocketAddr>()?;
                    require(
                        socket.ip() == std::net::Ipv4Addr::LOCALHOST && socket.port() != 0,
                        "Host must listen on loopback",
                    )?;
                    return Ok::<_, Box<dyn Error + Send + Sync>>(address.to_owned());
                }
            }
            Err("Host exited without a listen address".into())
        })
        .await;
        match ready {
            Ok(Ok(address)) => {
                host.address = address;
                Ok(host)
            }
            result => {
                host.stop().await?;
                Err(format!("Host startup failed: {result:?}").into())
            }
        }
    }

    async fn stop(&mut self) -> TestResult {
        if self.child.try_wait()?.is_none() {
            // Deliberate abrupt exit to check durable receipts after restart.
            self.child.start_kill()?;
        }
        timeout(DEADLINE, self.child.wait()).await??;
        Ok(())
    }
}

struct TestDirectory {
    root: PathBuf,
    path: PathBuf,
}
impl TestDirectory {
    fn create() -> TestResult<Self> {
        let root = fs::canonicalize(
            std::env::var_os("RECUVORA_TEST_TEMP").ok_or("set RECUVORA_TEST_TEMP")?,
        )?;
        for source in [env!("CARGO_MANIFEST_DIR"), env!("RECUVORA_CORE_SOURCE_DIR")] {
            require(
                !root.starts_with(fs::canonicalize(source)?),
                "test root must be outside Host/Core source",
            )?;
        }
        require(root.is_dir(), "test root must be an existing directory")?;
        let path = root.join(format!(
            "host-ui-contract-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir(&path)?;
        Ok(Self { root, path })
    }

    fn clean(&self) -> TestResult {
        let resolved = match fs::canonicalize(&self.path) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        require(
            resolved == self.path && resolved.parent() == Some(&self.root),
            "refusing to clean changed test path",
        )?;
        fs::remove_dir_all(resolved)?;
        Ok(())
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        if let Err(error) = self.clean() {
            eprintln!("UI contract cleanup failed: {error}");
        }
    }
}

fn require(condition: bool, message: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message).into())
    }
}
