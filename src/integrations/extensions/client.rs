//! Supervised single-call sessions, callbacks, cancellation and unknown outcomes.
use super::{
    ExtensionError, ExtensionKind, ExtensionMetadata, Message, NetworkEndpoint, Outcome,
    ProtocolSettings, call_id, valid_id,
};
use recuvora_core::operation::Cancellation;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub type CallbackFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Value, ExtensionError>> + Send + 'a>>;
pub trait CallbackHandler: Send + Sync {
    fn call<'a>(
        &'a self,
        method: String,
        params: Value,
        cancellation: Cancellation,
    ) -> CallbackFuture<'a>;
}

#[derive(Clone)]
pub struct ExtensionClient {
    pub id: String,
    pub kind: ExtensionKind,
    pub endpoint: NetworkEndpoint,
}

#[derive(Clone, Debug)]
pub struct ExtensionCall {
    pub contract: String,
    pub version: u32,
    pub method: String,
    pub params: Value,
    pub timeout: Duration,
}

impl ExtensionClient {
    pub async fn probe(&self) -> Result<ExtensionMetadata, ExtensionError> {
        self.probe_with_settings(&ProtocolSettings::default()).await
    }

    pub async fn probe_with_settings(
        &self,
        settings: &ProtocolSettings,
    ) -> Result<ExtensionMetadata, ExtensionError> {
        settings.validate()?;
        let mut session = self.open(settings).await?;
        let metadata = session.metadata.clone();
        if !session.close().await {
            return Err(ExtensionError::Protocol(
                "probe transport did not close cleanly".into(),
            ));
        }
        Ok(metadata)
    }

    /// A supervisor outlives a dropped caller, cancels work, awaits any trusted
    /// callback, and releases transport resources. No call is automatically replayed.
    pub async fn call(
        &self,
        call: ExtensionCall,
        expected: ExtensionMetadata,
        cancellation: Cancellation,
        handler: Option<Arc<dyn CallbackHandler>>,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<Value, ExtensionError> {
        self.call_with_settings(
            call,
            expected,
            cancellation,
            handler,
            permit,
            &ProtocolSettings::default(),
        )
        .await
    }

    /// Calls with explicit runtime tuning while retaining the protocol's fixed
    /// frame, message, callback and 1800-second call limits.
    pub async fn call_with_settings(
        &self,
        call: ExtensionCall,
        expected: ExtensionMetadata,
        cancellation: Cancellation,
        handler: Option<Arc<dyn CallbackHandler>>,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
        settings: &ProtocolSettings,
    ) -> Result<Value, ExtensionError> {
        settings.validate()?;
        if call.timeout.is_zero() || call.timeout > Duration::from_secs(1800) {
            return Err(ExtensionError::Rejected(
                "deadline must be 1ns..1800s".into(),
            ));
        }
        let mut guard = CancelOnDrop(Some(cancellation.clone()));
        let client = self.clone();
        let settings = settings.clone();
        let task = tokio::spawn(async move {
            let _permit = permit;
            client
                .call_inner(call, expected, cancellation, handler, settings)
                .await
        });
        let result = task
            .await
            .map_err(|e| ExtensionError::Unavailable(format!("supervisor failed: {e}")))?;
        guard.0 = None;
        result
    }

    async fn call_inner(
        &self,
        call: ExtensionCall,
        expected: ExtensionMetadata,
        cancellation: Cancellation,
        handler: Option<Arc<dyn CallbackHandler>>,
        settings: ProtocolSettings,
    ) -> Result<Value, ExtensionError> {
        if cancellation.is_cancelled() {
            return Err(ExtensionError::Cancelled);
        }
        let mut session = self.open(&settings).await?;
        if session.metadata != expected {
            session.close().await;
            return Err(ExtensionError::Protocol(
                "metadata changed since registration; re-register explicitly".into(),
            ));
        }
        if cancellation.is_cancelled() {
            session.close().await;
            return Err(ExtensionError::Cancelled);
        }
        let id = call_id();
        let message = Message::Call {
            id: id.clone(),
            contract: call.contract,
            version: call.version,
            method: call.method,
            params: call.params,
            timeout_ms: u64::try_from(call.timeout.as_millis())
                .unwrap_or(1800000)
                .max(1),
        };
        let result = async {
            session.send(&message).await?;
            let deadline = tokio::time::sleep(call.timeout);
            tokio::pin!(deadline);
            let mut callback: Option<tokio::task::JoinHandle<(String, Result<Value, ExtensionError>)>> = None;
            let mut seen = std::collections::BTreeSet::new();
            let mut count = 0usize;
            let mut cancelled = false;
            let cancel_grace = tokio::time::sleep(settings.cancel_grace());
            tokio::pin!(cancel_grace);
            let outcome = loop {
                tokio::select! {
                    _ = cancellation.cancelled(), if !cancelled => {
                        cancelled = true;
                        cancel_grace.as_mut().reset(tokio::time::Instant::now() + settings.cancel_grace());
                        if let Err(e) = session.send(&Message::Cancel {id:id.clone()}).await { break Err(e); }
                    }
                    _ = &mut deadline, if !cancelled => {
                        cancellation.cancel();
                    }
                    _ = &mut cancel_grace, if cancelled => {
                        break Err(ExtensionError::Unknown {call_id:id.clone(), message:"cancel acknowledgement deadline elapsed".into()});
                    }
                    done = async { match &mut callback { Some(task) => task.await, None => std::future::pending().await } } => {
                        callback = None;
                        let (callback_id, result) = match done { Ok(v) => v, Err(e) => break Err(ExtensionError::Protocol(format!("trusted callback failed: {e}"))) };
                        let reply = match result { Ok(result) => Message::Result {id:callback_id,result}, Err(e) => Message::Error {id:callback_id,code:"tool_rejected".into(),message:e.to_string(),outcome:Outcome::Rejected} };
                        if let Err(e) = session.send(&reply).await { break Err(e); }
                    }
                    message = session.recv() => {
                        count += 1;
                        if count > 130 { break Err(ExtensionError::Protocol("message count exceeded".into())); }
                        let message = match message { Some(Ok(m)) => m, Some(Err(e)) => break Err(e), None => break Err(ExtensionError::Unavailable("transport closed".into())) };
                        match message {
                            Message::Result {id:result_id,result} if result_id == id && callback.is_none() => { break Ok(result); }
                            Message::Error {id:result_id,code,message,outcome} if result_id == id && callback.is_none() => {
                                break Err(match outcome { Outcome::Rejected if seen.is_empty() => ExtensionError::Rejected(format!("{code}: {message}")), _ => ExtensionError::Unknown {call_id:id.clone(),message:format!("{code}: {message}")} });
                            }
                            Message::Callback {id:callback_id,parent_id,method,params} if parent_id == id && !cancelled && callback.is_none() && valid_id(&callback_id) && callback_id != id && seen.len() < 64 && seen.insert(callback_id.clone()) => {
                                let Some(handler) = handler.clone() else { break Err(ExtensionError::Protocol("unsolicited callback".into())); };
                                let token = cancellation.clone();
                                callback = Some(tokio::spawn(async move { (callback_id,handler.call(method,params,token).await) }));
                            }
                            _ => break Err(ExtensionError::Protocol("unexpected, duplicate or uncorrelated message".into())),
                        }
                    }
                }
            };
            if callback.is_some() { cancellation.cancel(); }
            if let Some(task) = callback { let _ = task.await; }
            outcome
        }.await;
        let exited = session.close().await;
        let result = if exited {
            result
        } else {
            Err(ExtensionError::Unknown {
                call_id: id.clone(),
                message: "transport cleanup incomplete; execution outcome requires a result check"
                    .into(),
            })
        };
        result.map_err(|error| match error {
            ExtensionError::Rejected(_) | ExtensionError::Unknown { .. } => error,
            other => ExtensionError::Unknown {
                call_id: id,
                message: other.to_string(),
            },
        })
    }
}

struct CancelOnDrop(Option<Cancellation>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(token) = &self.0 {
            token.cancel();
        }
    }
}
