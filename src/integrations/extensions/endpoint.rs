//! Explicit network destinations; credentials are resolved only when connecting.
use super::ExtensionError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkEndpoint {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_certificate: Option<PathBuf>,
}

impl NetworkEndpoint {
    pub fn validate(&self) -> Result<(), ExtensionError> {
        let invalid = |message: &str| ExtensionError::Configuration(message.into());
        if self.url.len() > 4096 || self.url.chars().any(char::is_whitespace) {
            return Err(invalid("invalid network endpoint URL"));
        }
        let url =
            reqwest::Url::parse(&self.url).map_err(|_| invalid("invalid network endpoint URL"))?;
        if !matches!(url.scheme(), "http" | "https" | "ws" | "wss")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.port() == Some(0)
        {
            return Err(invalid(
                "endpoint requires http/https/ws/wss and no URL credentials, query or fragment",
            ));
        }
        if let Some(name) = &self.bearer_token_env
            && (name.is_empty()
                || name.len() > 128
                || !name.bytes().enumerate().all(|(index, byte)| {
                    byte == b'_'
                        || byte.is_ascii_alphabetic()
                        || (index > 0 && byte.is_ascii_digit())
                }))
        {
            return Err(invalid("invalid bearer token environment variable name"));
        }
        if let Some(path) = &self.ca_certificate
            && (!matches!(url.scheme(), "https" | "wss") || !path.is_absolute() || !path.is_file())
        {
            return Err(invalid(
                "CA certificate requires TLS and an existing absolute file",
            ));
        }
        Ok(())
    }
}
