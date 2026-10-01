//! Bounded network sessions. Closing a connection never proves executor termination.
use super::{
    ExtensionError, MAX_FRAME_BYTES, Message, NetworkEndpoint, ProtocolSettings, valid_id,
};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use std::io::Read;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{
        Message as WsMessage, client::IntoClientRequest, protocol::WebSocketConfig,
        protocol::frame::coding::CloseCode,
    },
};

const SESSION_HEADER: &str = "x-recuvora-session";
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Writer = Arc<Mutex<SplitSink<Socket, WsMessage>>>;
type Incoming = mpsc::Receiver<Result<Message, ExtensionError>>;

enum Transport {
    WebSocket(Writer),
    Http(HttpSession),
}

struct HttpSession {
    client: reqwest::Client,
    url: reqwest::Url,
    session: Option<String>,
    sender: Option<mpsc::Sender<Result<Message, ExtensionError>>>,
}

pub(super) struct NetworkSession {
    transport: Transport,
    incoming: Incoming,
    reader: Option<JoinHandle<bool>>,
    settings: ProtocolSettings,
    closed: bool,
}

impl NetworkSession {
    pub(super) async fn connect(
        endpoint: &NetworkEndpoint,
        settings: &ProtocolSettings,
    ) -> Result<Self, ExtensionError> {
        endpoint.validate()?;
        settings.validate()?;
        let url = reqwest::Url::parse(&endpoint.url)
            .map_err(|_| configuration("invalid network endpoint"))?;
        let authorization = authorization(endpoint)?;
        let tls = tls_config(endpoint)?;
        let (sender, incoming) = mpsc::channel(settings.incoming_queue_capacity);
        match url.scheme() {
            "ws" | "wss" => {
                let mut request = endpoint
                    .url
                    .as_str()
                    .into_client_request()
                    .map_err(|_| configuration("invalid WebSocket endpoint"))?;
                if let Some(value) = authorization {
                    request.headers_mut().insert(AUTHORIZATION, value);
                }
                let limits = WebSocketConfig::default()
                    .read_buffer_size(16 * 1024)
                    .write_buffer_size(0)
                    .max_write_buffer_size(MAX_FRAME_BYTES + 1024)
                    .max_message_size(Some(MAX_FRAME_BYTES))
                    .max_frame_size(Some(MAX_FRAME_BYTES));
                // This connector performs one direct handshake, without proxies,
                // redirects or retries. Rustls validates the server name and chain.
                let (socket, _) = tokio::time::timeout(
                    settings.connect_timeout(),
                    connect_async_tls_with_config(
                        request,
                        Some(limits),
                        true,
                        Some(Connector::Rustls(Arc::new(tls))),
                    ),
                )
                .await
                .map_err(|_| unavailable("WebSocket connection deadline elapsed"))?
                .map_err(|_| unavailable("WebSocket connection or TLS handshake failed"))?;
                let (writer, stream) = socket.split();
                let writer = Arc::new(Mutex::new(writer));
                let reader_writer = writer.clone();
                let reader = tokio::spawn(websocket_reader(
                    stream,
                    reader_writer,
                    sender,
                    settings.clone(),
                ));
                Ok(Self {
                    transport: Transport::WebSocket(writer),
                    incoming,
                    reader: Some(reader),
                    settings: settings.clone(),
                    closed: false,
                })
            }
            "http" | "https" => {
                let mut headers = reqwest::header::HeaderMap::new();
                if let Some(value) = authorization {
                    headers.insert(AUTHORIZATION, value);
                }
                let client = reqwest::Client::builder()
                    .default_headers(headers)
                    .redirect(reqwest::redirect::Policy::none())
                    .retry(reqwest::retry::never())
                    .no_proxy()
                    .no_gzip()
                    .no_brotli()
                    .no_deflate()
                    .no_zstd()
                    .connect_timeout(settings.connect_timeout())
                    .timeout(settings.http_poll_timeout())
                    .read_timeout(settings.http_poll_timeout())
                    .tls_backend_preconfigured(tls)
                    .build()
                    .map_err(|_| configuration("cannot build network TLS client"))?;
                Ok(Self {
                    transport: Transport::Http(HttpSession {
                        client,
                        url,
                        session: None,
                        sender: Some(sender),
                    }),
                    incoming,
                    reader: None,
                    settings: settings.clone(),
                    closed: false,
                })
            }
            _ => Err(configuration("unsupported network endpoint scheme")),
        }
    }

    pub(super) async fn send(&mut self, message: &Message) -> Result<(), ExtensionError> {
        if self.closed {
            return Err(unavailable("network session is closed"));
        }
        let bytes =
            serde_json::to_vec(message).map_err(|_| protocol("cannot encode network message"))?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(ExtensionError::Rejected(
                "outgoing frame exceeds 1 MiB".into(),
            ));
        }
        match &mut self.transport {
            Transport::WebSocket(writer) => {
                let text = String::from_utf8(bytes)
                    .map_err(|_| protocol("outgoing message is not UTF-8"))?;
                tokio::time::timeout(self.settings.io_timeout(), async {
                    writer.lock().await.send(WsMessage::Text(text.into())).await
                })
                .await
                .map_err(|_| unavailable("WebSocket write deadline elapsed"))?
                .map_err(|_| unavailable("WebSocket write failed"))
            }
            Transport::Http(http) => {
                if http.session.is_none() {
                    if !matches!(message, Message::Hello { .. }) {
                        return Err(protocol("HTTP session must begin with Hello"));
                    }
                    let response = http
                        .client
                        .post(http.url.clone())
                        .header(CONTENT_TYPE, "application/json")
                        .timeout(self.settings.io_timeout())
                        .body(bytes)
                        .send()
                        .await
                        .map_err(|_| unavailable("HTTP handshake failed"))?;
                    if response.status() != reqwest::StatusCode::OK {
                        return Err(protocol("HTTP handshake requires status 200"));
                    }
                    if response.headers().get_all(SESSION_HEADER).iter().count() != 1 {
                        return Err(protocol("HTTP handshake requires one session identity"));
                    }
                    let session = response
                        .headers()
                        .get(SESSION_HEADER)
                        .and_then(|value| value.to_str().ok())
                        .filter(|value| valid_id(value))
                        .ok_or_else(|| protocol("invalid HTTP session identity"))?
                        .to_owned();
                    // Retain cleanup ownership before any response-body await.
                    http.session = Some(session.clone());
                    let ready = decode_message(&bounded_body(response).await?)?;
                    if !matches!(ready, Message::Ready { .. }) {
                        return Err(protocol("HTTP handshake requires Ready"));
                    }
                    let sender = http
                        .sender
                        .take()
                        .ok_or_else(|| protocol("HTTP handshake already completed"))?;
                    sender
                        .send(Ok(ready))
                        .await
                        .map_err(|_| unavailable("HTTP receiver closed"))?;
                    self.reader = Some(tokio::spawn(http_reader(
                        http.client.clone(),
                        http.url.clone(),
                        session,
                        sender,
                        self.settings.clone(),
                    )));
                    Ok(())
                } else {
                    if matches!(message, Message::Hello { .. }) {
                        return Err(protocol("HTTP session is already initialized"));
                    }
                    let response = http
                        .client
                        .post(http.url.clone())
                        .header(SESSION_HEADER, http.session.as_deref().unwrap_or_default())
                        .header(CONTENT_TYPE, "application/json")
                        .timeout(self.settings.io_timeout())
                        .body(bytes)
                        .send()
                        .await
                        .map_err(|_| unavailable("HTTP message submission failed"))?;
                    if !matches!(response.status().as_u16(), 202 | 204) {
                        return Err(protocol(
                            "HTTP message submission requires status 202 or 204",
                        ));
                    }
                    if !bounded_body(response).await?.is_empty() {
                        return Err(protocol("HTTP submission acknowledgement must be empty"));
                    }
                    Ok(())
                }
            }
        }
    }

    /// The reader owns network reads, so cancelling this wait cannot lose a
    /// response which a remote HTTP queue has already removed.
    pub(super) async fn recv(&mut self) -> Option<Result<Message, ExtensionError>> {
        self.incoming.recv().await
    }

    pub(super) async fn close(&mut self) -> bool {
        self.closed = true;
        let clean = match &mut self.transport {
            Transport::WebSocket(writer) => {
                let sent = tokio::time::timeout(self.settings.io_timeout(), async {
                    writer.lock().await.close().await
                })
                .await;
                let sent = matches!(sent, Ok(Ok(())))
                    || matches!(
                        sent,
                        Ok(Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed
                            | tokio_tungstenite::tungstenite::Error::AlreadyClosed))
                    );
                let completed_pending = match self.reader.as_mut() {
                    Some(reader) => {
                        match tokio::time::timeout(self.settings.close_timeout(), reader).await {
                            Ok(result) => {
                                self.reader = None;
                                matches!(result, Ok(true))
                            }
                            Err(_) => false,
                        }
                    }
                    None => true,
                };
                sent && completed_pending
            }
            Transport::Http(http) => {
                if let Some(reader) = self.reader.as_mut() {
                    reader.abort();
                    let _ = reader.await;
                }
                self.reader = None;
                http.sender = None;
                match http.session.as_ref() {
                    Some(session) => {
                        let result = http
                            .client
                            .delete(http.url.clone())
                            .header(SESSION_HEADER, session)
                            .timeout(self.settings.io_timeout())
                            .send()
                            .await;
                        match result {
                            Ok(response) if matches!(response.status().as_u16(), 200 | 204) => {
                                let clean = bounded_body(response).await.is_ok();
                                if clean {
                                    http.session = None;
                                }
                                clean
                            }
                            _ => false,
                        }
                    }
                    None => true,
                }
            }
        };
        if let Some(reader) = self.reader.take() {
            reader.abort();
            let _ = reader.await;
        }
        let mut empty = true;
        while self.incoming.try_recv().is_ok() {
            empty = false;
        }
        clean && empty
    }
}

impl Drop for NetworkSession {
    fn drop(&mut self) {
        if let Some(reader) = &self.reader {
            reader.abort();
        }
    }
}

async fn websocket_reader(
    mut stream: futures_util::stream::SplitStream<Socket>,
    writer: Writer,
    sender: mpsc::Sender<Result<Message, ExtensionError>>,
    settings: ProtocolSettings,
) -> bool {
    loop {
        // Calls have an 1800-second maximum; the supervisor normally cancels
        // sooner. The reader also has its own finite idle boundary.
        let next = tokio::time::timeout(settings.websocket_idle_timeout(), stream.next()).await;
        let item = match next {
            Ok(Some(Ok(WsMessage::Text(text)))) => decode_message(text.as_bytes()),
            Ok(Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_)))) => {
                // Tungstenite queues protocol pong replies while reading.
                if !matches!(
                    tokio::time::timeout(settings.io_timeout(), async {
                        writer.lock().await.flush().await
                    })
                    .await,
                    Ok(Ok(()))
                ) {
                    let _ = sender
                        .send(Err(unavailable("WebSocket control write failed")))
                        .await;
                    return false;
                }
                continue;
            }
            Ok(Some(Ok(WsMessage::Close(frame)))) => {
                let normal = frame.is_none_or(|frame| frame.code == CloseCode::Normal);
                let _ = tokio::time::timeout(settings.io_timeout(), async {
                    writer.lock().await.flush().await
                })
                .await;
                return normal;
            }
            Ok(Some(Ok(_))) => Err(protocol("WebSocket requires JSON text messages")),
            Ok(Some(Err(_))) | Ok(None) => {
                Err(unavailable("WebSocket receive failed or connection closed"))
            }
            Err(_) => Err(unavailable("WebSocket read deadline elapsed")),
        };
        let failed = item.is_err();
        if sender.send(item).await.is_err() || failed {
            return false;
        }
    }
}

async fn http_reader(
    client: reqwest::Client,
    url: reqwest::Url,
    session: String,
    sender: mpsc::Sender<Result<Message, ExtensionError>>,
    settings: ProtocolSettings,
) -> bool {
    loop {
        let next = async {
            let response = client
                .get(url.clone())
                .header(SESSION_HEADER, &session)
                .header("prefer", format!("wait={}", settings.http_server_wait_secs))
                .timeout(settings.http_poll_timeout())
                .send()
                .await
                .map_err(|_| unavailable("HTTP message polling failed"))?;
            let status = response.status();
            if !matches!(status.as_u16(), 200 | 204) {
                return Err(protocol("HTTP polling requires status 200 or 204"));
            }
            let bytes = bounded_body(response).await?;
            if status == reqwest::StatusCode::NO_CONTENT {
                if !bytes.is_empty() {
                    return Err(protocol("HTTP empty poll must have no body"));
                }
                Ok(None)
            } else {
                decode_message(&bytes).map(Some)
            }
        }
        .await;
        match next {
            Ok(Some(message)) => {
                if sender.send(Ok(message)).await.is_err() {
                    return false;
                }
            }
            Ok(None) => tokio::time::sleep(settings.http_empty_backoff()).await,
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                return false;
            }
        }
    }
}

async fn bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>, ExtensionError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_FRAME_BYTES as u64)
    {
        return Err(protocol("HTTP frame exceeds 1 MiB"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| unavailable("HTTP response body failed"))?
    {
        if chunk.len() > MAX_FRAME_BYTES - bytes.len() {
            return Err(protocol("HTTP frame exceeds 1 MiB"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn decode_message(bytes: &[u8]) -> Result<Message, ExtensionError> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(protocol("network frame exceeds 1 MiB"));
    }
    serde_json::from_slice(bytes).map_err(|_| protocol("invalid network JSON message"))
}

fn authorization(endpoint: &NetworkEndpoint) -> Result<Option<HeaderValue>, ExtensionError> {
    let Some(name) = &endpoint.bearer_token_env else {
        return Ok(None);
    };
    let token = std::env::var(name)
        .map_err(|_| configuration("network bearer token environment variable is unavailable"))?;
    if token.is_empty() || token.len() > 8192 || token.bytes().any(|byte| !byte.is_ascii_graphic())
    {
        return Err(configuration("invalid network bearer token"));
    }
    let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| configuration("invalid network authorization header"))?;
    value.set_sensitive(true);
    Ok(Some(value))
}

fn tls_config(endpoint: &NetworkEndpoint) -> Result<rustls::ClientConfig, ExtensionError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = &endpoint.ca_certificate {
        let file = std::fs::File::open(path)
            .map_err(|_| configuration("cannot read network CA certificate"))?;
        let mut bytes = Vec::new();
        file.take((MAX_FRAME_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| configuration("cannot read network CA certificate"))?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(configuration("network CA certificate exceeds 1 MiB"));
        }
        let certificates = rustls_pemfile::certs(&mut bytes.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| configuration("invalid network CA certificate PEM"))?;
        if certificates.is_empty() || certificates.len() > 64 {
            return Err(configuration(
                "network CA file must contain 1..64 certificates",
            ));
        }
        for certificate in certificates {
            roots
                .add(certificate)
                .map_err(|_| configuration("invalid network CA certificate"))?;
        }
    }
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| configuration("cannot configure network TLS versions"))
        .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
}

fn unavailable(message: &str) -> ExtensionError {
    ExtensionError::Unavailable(message.into())
}
fn protocol(message: &str) -> ExtensionError {
    ExtensionError::Protocol(message.into())
}
fn configuration(message: &str) -> ExtensionError {
    ExtensionError::Configuration(message.into())
}
