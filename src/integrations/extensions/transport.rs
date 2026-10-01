//! Shared network identity handshake and per-call session ownership.
use super::network::NetworkSession;
use super::{
    ExtensionClient, ExtensionError, ExtensionMetadata, Message, PROTOCOL_VERSION, ProtocolSettings,
};

impl ExtensionClient {
    pub(super) async fn open(
        &self,
        settings: &ProtocolSettings,
    ) -> Result<Session, ExtensionError> {
        let transport = tokio::time::timeout(
            settings.connect_timeout(),
            NetworkSession::connect(&self.endpoint, settings),
        )
        .await
        .map_err(|_| ExtensionError::Unavailable("network connection deadline elapsed".into()))??;
        let mut session = Session {
            transport,
            metadata: ExtensionMetadata {
                protocol_version: 0,
                id: String::new(),
                kind: self.kind,
                contracts: vec![],
                capabilities: vec![],
                workspaces: vec![],
            },
        };
        let handshake = tokio::time::timeout(settings.handshake_timeout(), async {
            session
                .send(&Message::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    expected_id: self.id.clone(),
                    kind: self.kind,
                })
                .await?;
            match session.recv().await {
                Some(Ok(Message::Ready { metadata }))
                    if metadata.protocol_version == PROTOCOL_VERSION
                        && metadata.id == self.id
                        && metadata.kind == self.kind =>
                {
                    Ok(metadata)
                }
                Some(Err(e)) => Err(e),
                _ => Err(ExtensionError::Protocol(
                    "identity, kind or protocol version mismatch".into(),
                )),
            }
        })
        .await;
        match handshake {
            Ok(Ok(metadata)) => {
                session.metadata = metadata;
                Ok(session)
            }
            error => {
                session.close().await;
                Err(match error {
                    Ok(Err(e)) => e,
                    _ => ExtensionError::Unavailable("handshake deadline elapsed".into()),
                })
            }
        }
    }
}

pub(super) struct Session {
    transport: NetworkSession,
    pub(super) metadata: ExtensionMetadata,
}

impl Session {
    pub(super) async fn send(&mut self, message: &Message) -> Result<(), ExtensionError> {
        self.transport.send(message).await
    }

    pub(super) async fn recv(&mut self) -> Option<Result<Message, ExtensionError>> {
        self.transport.recv().await
    }

    pub(super) async fn close(&mut self) -> bool {
        self.transport.close().await
    }
}
