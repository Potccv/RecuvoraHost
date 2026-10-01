//! Isolated loopback WebSocket peers for synchronous business-contract fixtures.
//! TLS and certificate behavior are covered separately by network_transport.
use recuvora_host::integrations::extensions::{Message, NetworkEndpoint};
use std::collections::BTreeMap;
use std::error::Error;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{self, Message as Frame, WebSocket};

pub type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

pub struct Session {
    socket: WebSocket<TcpStream>,
    env: BTreeMap<String, String>,
    closed: bool,
}

impl Session {
    #[allow(dead_code)] // Some shared fixtures have no scenario environment.
    pub fn env(&self, name: &str) -> Option<&str> {
        self.env.get(name).map(String::as_str)
    }

    pub fn read(&mut self) -> TestResult<Option<Message>> {
        loop {
            match self.socket.read() {
                Ok(Frame::Text(text)) => return Ok(Some(serde_json::from_str(&text)?)),
                Ok(Frame::Close(_)) => {
                    match self.socket.flush() {
                        Ok(()) | Err(tungstenite::Error::ConnectionClosed) => {}
                        Err(error) => return Err(error.into()),
                    }
                    self.closed = true;
                    return Ok(None);
                }
                Ok(Frame::Ping(_)) => self.socket.flush()?,
                Ok(Frame::Pong(_)) => {}
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    self.closed = true;
                    return Ok(None);
                }
                Ok(_) => return Err("fixture accepts only text protocol messages".into()),
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub fn write(&mut self, message: Message) -> TestResult {
        self.socket
            .send(Frame::Text(serde_json::to_string(&message)?.into()))?;
        Ok(())
    }

    #[allow(dead_code)] // Only the extension bounds test sends malformed raw frames.
    pub fn write_text(&mut self, text: String) -> TestResult {
        self.socket.send(Frame::Text(text.into()))?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.closed {
            // Early fixture returns still finish the WebSocket close exchange;
            // absence of a business result remains Unknown in the client.
            let _ = self
                .socket
                .get_mut()
                .set_read_timeout(Some(Duration::from_secs(2)));
            let _ = self.socket.close(None);
            let _ = self.socket.flush();
            while let Ok(frame) = self.socket.read() {
                if matches!(frame, Frame::Close(_)) {
                    let _ = self.socket.flush();
                    break;
                }
            }
        }
    }
}

pub struct PeerServer {
    endpoint: NetworkEndpoint,
    stopped: Arc<AtomicBool>,
    sockets: Arc<Mutex<Vec<TcpStream>>>,
    listener: Option<JoinHandle<TestResult>>,
}

impl PeerServer {
    pub fn start(
        handler: fn(Session) -> TestResult,
        env: BTreeMap<String, String>,
    ) -> TestResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let sockets = Arc::new(Mutex::new(Vec::<TcpStream>::new()));
        let tracked = sockets.clone();
        let thread = thread::spawn(move || {
            let mut workers = Vec::new();
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // Windows accepted sockets inherit the listener's mode;
                        // synchronous fixture reads require a blocking stream.
                        stream.set_nonblocking(false)?;
                        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
                        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                        tracked.lock().unwrap().push(stream.try_clone()?);
                        let env = env.clone();
                        workers.push(thread::spawn(move || {
                            if let Ok(socket) = tungstenite::accept(stream) {
                                // Rejection and abrupt-close scenarios deliberately
                                // make fixture I/O fail. Assertions still panic.
                                let _ = handler(Session {
                                    socket,
                                    env,
                                    closed: false,
                                });
                            }
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            for socket in tracked.lock().unwrap().iter() {
                let _ = socket.shutdown(Shutdown::Both);
            }
            for worker in workers {
                worker
                    .join()
                    .map_err(|_| "WebSocket fixture handler panicked")?;
            }
            Ok(())
        });
        Ok(Self {
            endpoint: NetworkEndpoint {
                url: format!("ws://{address}/extension"),
                bearer_token_env: None,
                ca_certificate: None,
            },
            stopped,
            sockets,
            listener: Some(thread),
        })
    }

    pub fn endpoint(&self) -> NetworkEndpoint {
        self.endpoint.clone()
    }

    pub fn shutdown(mut self) -> TestResult {
        self.close()
    }

    fn close(&mut self) -> TestResult {
        self.stopped.store(true, Ordering::Release);
        for socket in self.sockets.lock().unwrap().iter() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(listener) = self.listener.take() {
            listener
                .join()
                .map_err(|_| "WebSocket fixture listener panicked")??;
        }
        Ok(())
    }
}

impl Drop for PeerServer {
    fn drop(&mut self) {
        let result = self.close();
        if !thread::panicking() {
            result.expect("WebSocket fixture cleanup failed");
        }
    }
}
