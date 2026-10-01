//! Bounded runtime tuning for protocol sessions.
use super::ExtensionError;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const MAX_SHORT_TIMEOUT_MS: u64 = 60_000;
const MAX_HTTP_POLL_TIMEOUT_MS: u64 = 120_000;
const MAX_HTTP_SERVER_WAIT_SECS: u64 = 60;
const MAX_HTTP_EMPTY_BACKOFF_MS: u64 = 10_000;
const MAX_WEBSOCKET_IDLE_TIMEOUT_MS: u64 = 1_860_000;
const MAX_INCOMING_QUEUE_CAPACITY: usize = 1_024;
const MAX_CALL_TIMEOUT_MS: u64 = 1_800_000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProtocolSettings {
    pub connect_timeout_ms: u64,
    pub handshake_timeout_ms: u64,
    pub io_timeout_ms: u64,
    pub close_timeout_ms: u64,
    pub cancel_grace_ms: u64,
    pub http_poll_timeout_ms: u64,
    pub http_server_wait_secs: u64,
    pub http_empty_backoff_ms: u64,
    pub websocket_idle_timeout_ms: u64,
    pub incoming_queue_capacity: usize,
}

impl Default for ProtocolSettings {
    fn default() -> Self {
        Self {
            connect_timeout_ms: 5_000,
            handshake_timeout_ms: 5_000,
            io_timeout_ms: 5_000,
            close_timeout_ms: 10_000,
            cancel_grace_ms: 10_000,
            http_poll_timeout_ms: 35_000,
            http_server_wait_secs: 30,
            http_empty_backoff_ms: 100,
            websocket_idle_timeout_ms: 1_810_000,
            incoming_queue_capacity: 8,
        }
    }
}

impl ProtocolSettings {
    pub fn validate(&self) -> Result<(), ExtensionError> {
        if !(1..=MAX_SHORT_TIMEOUT_MS).contains(&self.connect_timeout_ms)
            || !(1..=MAX_SHORT_TIMEOUT_MS).contains(&self.handshake_timeout_ms)
            || !(1..=MAX_SHORT_TIMEOUT_MS).contains(&self.io_timeout_ms)
            || !(1..=MAX_SHORT_TIMEOUT_MS).contains(&self.close_timeout_ms)
            || !(1..=MAX_SHORT_TIMEOUT_MS).contains(&self.cancel_grace_ms)
        {
            return Err(configuration(
                "protocol connection, handshake, I/O, close and cancellation timeouts must be 1..60000ms",
            ));
        }
        if self.io_timeout_ms < self.connect_timeout_ms
            || self.handshake_timeout_ms < self.io_timeout_ms
            || self.close_timeout_ms < self.io_timeout_ms
            || self.cancel_grace_ms < self.io_timeout_ms
        {
            return Err(configuration(
                "protocol I/O must cover connection timeout, and handshake, close and cancellation timeouts must cover I/O",
            ));
        }
        if !(1..=MAX_HTTP_POLL_TIMEOUT_MS).contains(&self.http_poll_timeout_ms)
            || !(1..=MAX_HTTP_SERVER_WAIT_SECS).contains(&self.http_server_wait_secs)
            || !(1..=MAX_HTTP_EMPTY_BACKOFF_MS).contains(&self.http_empty_backoff_ms)
        {
            return Err(configuration(
                "HTTP poll timeout, server wait and empty-poll backoff exceed their fixed limits",
            ));
        }
        let server_wait_ms = self
            .http_server_wait_secs
            .checked_mul(1_000)
            .ok_or_else(|| configuration("HTTP server wait overflows milliseconds"))?;
        if self.http_poll_timeout_ms <= server_wait_ms {
            return Err(configuration(
                "HTTP poll timeout must be longer than the advertised server wait",
            ));
        }
        let minimum_idle = MAX_CALL_TIMEOUT_MS
            .checked_add(self.cancel_grace_ms)
            .ok_or_else(|| configuration("WebSocket idle timeout relation overflowed"))?;
        if !(minimum_idle..=MAX_WEBSOCKET_IDLE_TIMEOUT_MS).contains(&self.websocket_idle_timeout_ms)
        {
            return Err(configuration(
                "WebSocket idle timeout must cover the fixed 1800s call limit and cancellation grace without exceeding 1860s",
            ));
        }
        if !(1..=MAX_INCOMING_QUEUE_CAPACITY).contains(&self.incoming_queue_capacity) {
            return Err(configuration(
                "incoming protocol queue capacity must be 1..1024",
            ));
        }
        Ok(())
    }

    pub(super) fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.connect_timeout_ms)
    }

    pub(super) fn handshake_timeout(&self) -> Duration {
        Duration::from_millis(self.handshake_timeout_ms)
    }

    pub(super) fn io_timeout(&self) -> Duration {
        Duration::from_millis(self.io_timeout_ms)
    }

    pub(super) fn close_timeout(&self) -> Duration {
        Duration::from_millis(self.close_timeout_ms)
    }

    pub(super) fn cancel_grace(&self) -> Duration {
        Duration::from_millis(self.cancel_grace_ms)
    }

    pub(super) fn http_poll_timeout(&self) -> Duration {
        Duration::from_millis(self.http_poll_timeout_ms)
    }

    pub(super) fn http_empty_backoff(&self) -> Duration {
        Duration::from_millis(self.http_empty_backoff_ms)
    }

    pub(super) fn websocket_idle_timeout(&self) -> Duration {
        Duration::from_millis(self.websocket_idle_timeout_ms)
    }
}

fn configuration(message: &str) -> ExtensionError {
    ExtensionError::Configuration(message.into())
}
