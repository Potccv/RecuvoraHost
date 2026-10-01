//! Loopback-only network peer for the public extension protocol.
use futures_util::{SinkExt, StreamExt};
use recuvora_core::operation::Cancellation;
use recuvora_host::integrations::extensions::{
    ContractDeclaration, ExtensionClient, ExtensionKind, ExtensionMetadata, MAX_FRAME_BYTES,
    Message, MethodDeclaration, NetworkEndpoint, Outcome, PROTOCOL_VERSION,
};
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer};
use tokio_tungstenite::tungstenite::Message as WsMessage;

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Copy, Debug)]
pub enum Behavior {
    Echo,
    Callback,
    WaitForCancel,
    Disconnect,
    WrongCorrelation,
    Redirect,
    Unavailable,
}

enum Outbound {
    Message(Message),
    Disconnect,
}

#[derive(Default)]
struct Session {
    outgoing: Mutex<VecDeque<Outbound>>,
    changed: Notify,
    parent: Mutex<Option<String>>,
}

impl Session {
    fn push(&self, message: Outbound) {
        self.outgoing.lock().unwrap().push_back(message);
        self.changed.notify_one();
    }

    async fn next(&self) -> Outbound {
        loop {
            let notified = self.changed.notified();
            if let Some(message) = self.outgoing.lock().unwrap().pop_front() {
                return message;
            }
            notified.await;
        }
    }
}

pub struct State {
    behavior: Behavior,
    plugin_contract: AtomicBool,
    pub calls: AtomicUsize,
    pub cancellations: AtomicUsize,
    pub handshakes: AtomicUsize,
    pub redirect_hits: AtomicUsize,
    pub metadata_changed: AtomicBool,
    dispatched: Notify,
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
}

impl State {
    fn new(behavior: Behavior) -> Self {
        Self {
            behavior,
            plugin_contract: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            cancellations: AtomicUsize::new(0),
            handshakes: AtomicUsize::new(0),
            redirect_hits: AtomicUsize::new(0),
            metadata_changed: AtomicBool::new(false),
            dispatched: Notify::new(),
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    pub async fn wait_for_dispatch(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = self.dispatched.notified();
                if self.calls.load(Ordering::SeqCst) != 0 {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("network fixture did not receive the call");
    }

    fn metadata(&self) -> ExtensionMetadata {
        let plugin = self.plugin_contract.load(Ordering::SeqCst);
        let input_schema = json!({
            "type":"object",
            "properties":{"target":{"type":"string"}},
            "required":["target"],
            "additionalProperties":false
        });
        ExtensionMetadata {
            protocol_version: PROTOCOL_VERSION,
            id: if plugin {
                "network-plugin"
            } else {
                "network-node"
            }
            .into(),
            kind: if plugin {
                ExtensionKind::Plugin
            } else {
                ExtensionKind::Node
            },
            contracts: if plugin {
                vec![ContractDeclaration {
                    id: "com.example.network".into(),
                    version: 1,
                    methods: ["query", "unlisted"]
                        .into_iter()
                        .map(|name| MethodDeclaration {
                            name: name.into(),
                            read_only: true,
                            input_schema: input_schema.clone(),
                            output_schema: json!({
                                "type":"object",
                                "properties":{"echo":input_schema},
                                "required":["echo"],
                                "additionalProperties":false
                            }),
                        })
                        .collect(),
                }]
            } else {
                vec![]
            },
            capabilities: if self.metadata_changed.load(Ordering::SeqCst) {
                vec!["changed".into()]
            } else {
                vec![]
            },
            workspaces: vec![],
        }
    }

    fn accept(&self, session: &Session, message: Message) {
        match message {
            Message::Call { id, params, .. } => {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.dispatched.notify_one();
                *session.parent.lock().unwrap() = Some(id.clone());
                match self.behavior {
                    Behavior::Callback => session.push(Outbound::Message(Message::Callback {
                        id: "callback-1".into(),
                        parent_id: id,
                        method: "tool".into(),
                        params: json!({"workload":"target-a"}),
                    })),
                    Behavior::WaitForCancel => {}
                    Behavior::Disconnect | Behavior::Unavailable => {
                        session.push(Outbound::Disconnect);
                    }
                    Behavior::WrongCorrelation => {
                        session.push(Outbound::Message(Message::Result {
                            id: "another-call".into(),
                            result: json!({"unexpected":true}),
                        }))
                    }
                    Behavior::Echo | Behavior::Redirect => {
                        session.push(Outbound::Message(Message::Result {
                            id,
                            result: json!({"echo":params}),
                        }));
                    }
                }
            }
            Message::Result { id, result } => {
                assert_eq!(id, "callback-1");
                let parent = session.parent.lock().unwrap().clone().unwrap();
                session.push(Outbound::Message(Message::Result {
                    id: parent,
                    result: json!({"callback":result}),
                }));
            }
            Message::Cancel { id } => {
                self.cancellations.fetch_add(1, Ordering::SeqCst);
                session.push(Outbound::Message(Message::Error {
                    id,
                    code: "cancelled".into(),
                    message: "executor outcome requires an independent result check".into(),
                    outcome: Outcome::Cancelled,
                }));
            }
            Message::Error { .. } => {}
            other => panic!("unexpected fixture message: {other:?}"),
        }
    }
}

pub struct TlsMaterial {
    pub pem: String,
    acceptor: TlsAcceptor,
}

impl TlsMaterial {
    pub fn new() -> TestResult<Self> {
        let key = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])?;
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![key.cert.der().clone()],
                PrivatePkcs8KeyDer::from(key.signing_key.serialize_der()).into(),
            )?;
        Ok(Self {
            pem: key.cert.pem(),
            acceptor: TlsAcceptor::from(Arc::new(config)),
        })
    }
}

trait FixtureIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> FixtureIo for T {}
type Stream = Box<dyn FixtureIo>;

pub struct Fixture {
    pub state: Arc<State>,
    pub url: String,
    cancellation: Cancellation,
    server: Option<JoinHandle<()>>,
}

impl Fixture {
    pub async fn start(
        scheme: &str,
        behavior: Behavior,
        tls: Option<TlsMaterial>,
    ) -> TestResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let url = format!("{scheme}://127.0.0.1:{port}/extension");
        let websocket = matches!(scheme, "ws" | "wss");
        let state = Arc::new(State::new(behavior));
        let server_state = state.clone();
        let cancellation = Cancellation::new();
        let stop = cancellation.clone();
        let acceptor = tls.map(|tls| tls.acceptor);
        let server = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        let acceptor = acceptor.clone();
                        let state = server_state.clone();
                        connections.spawn(async move {
                            let stream: Stream = if let Some(acceptor) = acceptor {
                                match tokio::time::timeout(Duration::from_secs(5), acceptor.accept(stream)).await {
                                    Ok(Ok(stream)) => Box::new(stream),
                                    // Unknown-certificate tests intentionally fail TLS negotiation.
                                    _ => return,
                                }
                            } else {
                                Box::new(stream)
                            };
                            if websocket {
                                serve_websocket(stream, state).await;
                            } else {
                                let _ = serve_http(stream, state).await;
                            }
                        });
                    }
                    completed = connections.join_next(), if !connections.is_empty() => {
                        if let Some(Err(error)) = completed {
                            panic!("network fixture connection panicked: {error}");
                        }
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Ok(Self {
            state,
            url,
            cancellation,
            server: Some(server),
        })
    }

    pub fn client(&self, ca_certificate: Option<PathBuf>) -> ExtensionClient {
        let metadata = self.state.metadata();
        ExtensionClient {
            id: metadata.id,
            kind: metadata.kind,
            endpoint: NetworkEndpoint {
                url: self.url.clone(),
                bearer_token_env: None,
                ca_certificate,
            },
        }
    }

    pub fn enable_plugin_contract(&self) {
        self.state.plugin_contract.store(true, Ordering::SeqCst);
    }

    pub async fn shutdown(mut self) {
        self.cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.server.take().unwrap())
            .await
            .expect("network fixture shutdown timed out")
            .expect("network fixture server panicked");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

async fn serve_websocket(stream: Stream, state: Arc<State>) {
    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let Some(Ok(WsMessage::Text(text))) = socket.next().await else {
        return;
    };
    assert!(matches!(
        serde_json::from_str::<Message>(&text).unwrap(),
        Message::Hello { expected_id, kind, protocol_version: PROTOCOL_VERSION }
            if expected_id == state.metadata().id && kind == state.metadata().kind
    ));
    state.handshakes.fetch_add(1, Ordering::SeqCst);
    if socket
        .send(WsMessage::Text(
            serde_json::to_string(&Message::Ready {
                metadata: state.metadata(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .is_err()
    {
        return;
    }
    let session = Session::default();
    loop {
        tokio::select! {
            incoming = socket.next() => match incoming {
                Some(Ok(WsMessage::Text(text))) => {
                    state.accept(&session, serde_json::from_str(&text).unwrap());
                }
                Some(Ok(WsMessage::Close(_))) => {
                    // Reading Close already queues Tungstenite's reply. Sending
                    // another Close would fail before that queued reply is flushed.
                    match socket.flush().await {
                        Ok(()) | Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed) => {}
                        Err(error) => panic!("network fixture close reply failed: {error}"),
                    }
                    break;
                }
                Some(Ok(WsMessage::Ping(_))) => { let _ = socket.flush().await; }
                Some(Ok(WsMessage::Pong(_))) => {}
                _ => break,
            },
            outgoing = session.next() => match outgoing {
                Outbound::Disconnect => break,
                Outbound::Message(message) => {
                    if socket.send(WsMessage::Text(serde_json::to_string(&message).unwrap().into())).await.is_err() {
                        break;
                    }
                }
            },
        }
    }
}

struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

async fn read_request(stream: &mut Stream) -> io::Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).await?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut headers = BTreeMap::new();
    let mut bytes = first.len();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Err(io::Error::other("incomplete fixture HTTP headers"));
        }
        bytes += line.len();
        if bytes > 16 * 1024 {
            return Err(io::Error::other("fixture HTTP header limit"));
        }
        if line == "\r\n" {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::other("invalid fixture HTTP header"))?;
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length: usize = headers
        .get("content-length")
        .map_or(Ok(0), |value| value.parse())
        .map_err(io::Error::other)?;
    if length > MAX_FRAME_BYTES {
        return Err(io::Error::other("fixture HTTP body limit"));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

async fn response(
    stream: &mut Stream,
    status: &str,
    extra_headers: &str,
    body: &[u8],
) -> io::Result<()> {
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{extra_headers}\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    stream.shutdown().await
}

async fn serve_http(mut stream: Stream, state: Arc<State>) -> io::Result<()> {
    let request = tokio::time::timeout(Duration::from_secs(5), read_request(&mut stream))
        .await
        .map_err(io::Error::other)??;
    if request.path == "/redirect-sink" {
        state.redirect_hits.fetch_add(1, Ordering::SeqCst);
    } else if matches!(state.behavior, Behavior::Redirect) {
        return response(
            &mut stream,
            "307 Temporary Redirect",
            "Location: /redirect-sink\r\n",
            &[],
        )
        .await;
    }
    if request.method == "POST" && !request.headers.contains_key("x-recuvora-session") {
        assert!(matches!(
            serde_json::from_slice::<Message>(&request.body).unwrap(),
            Message::Hello { expected_id, kind, protocol_version: PROTOCOL_VERSION }
                if expected_id == state.metadata().id && kind == state.metadata().kind
        ));
        let sequence = state.handshakes.fetch_add(1, Ordering::SeqCst);
        let id = format!("session-{sequence}");
        state
            .sessions
            .lock()
            .unwrap()
            .insert(id.clone(), Arc::new(Session::default()));
        return response(
            &mut stream,
            "200 OK",
            &format!("x-recuvora-session: {id}\r\n"),
            &serde_json::to_vec(&Message::Ready {
                metadata: state.metadata(),
            })?,
        )
        .await;
    }
    let id = request
        .headers
        .get("x-recuvora-session")
        .cloned()
        .unwrap_or_default();
    let session = state.sessions.lock().unwrap().get(&id).cloned();
    let Some(session) = session else {
        return response(&mut stream, "404 Not Found", "", &[]).await;
    };
    match request.method.as_str() {
        "POST" => {
            let message = serde_json::from_slice::<Message>(&request.body)?;
            let unavailable = matches!(state.behavior, Behavior::Unavailable)
                && matches!(message, Message::Call { .. });
            state.accept(&session, message);
            response(
                &mut stream,
                if unavailable {
                    "503 Service Unavailable"
                } else {
                    "202 Accepted"
                },
                "",
                &[],
            )
            .await
        }
        "GET" => match tokio::time::timeout(Duration::from_millis(100), session.next()).await {
            Ok(Outbound::Message(message)) => {
                response(&mut stream, "200 OK", "", &serde_json::to_vec(&message)?).await
            }
            Ok(Outbound::Disconnect) => Ok(()),
            Err(_) => response(&mut stream, "204 No Content", "", &[]).await,
        },
        "DELETE" => {
            state.sessions.lock().unwrap().remove(&id);
            response(&mut stream, "204 No Content", "", &[]).await
        }
        _ => response(&mut stream, "405 Method Not Allowed", "", &[]).await,
    }
}
