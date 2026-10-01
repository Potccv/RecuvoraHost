//! A bounded, immutable snapshot of the separately built official UI.
use super::{ApiError, external};
use axum::body::Bytes;
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

const CATALOG: &[(&str, &str)] = &[
    ("index.html", "text/html; charset=utf-8"),
    ("styles.css", "text/css; charset=utf-8"),
    ("app.js", "text/javascript; charset=utf-8"),
    ("api.js", "text/javascript; charset=utf-8"),
    ("dom.js", "text/javascript; charset=utf-8"),
    ("history.js", "text/javascript; charset=utf-8"),
    ("monitoring.js", "text/javascript; charset=utf-8"),
    ("plugin-monitoring.js", "text/javascript; charset=utf-8"),
    ("project-logs.js", "text/javascript; charset=utf-8"),
    ("refresh.js", "text/javascript; charset=utf-8"),
    ("data.js", "text/javascript; charset=utf-8"),
    ("shell.js", "text/javascript; charset=utf-8"),
    ("favicon.svg", "image/svg+xml"),
    ("favicon.ico", "image/x-icon"),
];
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Assets(BTreeMap<&'static str, (&'static str, Bytes)>);

impl Assets {
    pub(super) fn load(directory: Option<&Path>) -> Result<Self, ApiError> {
        let Some(directory) = directory else {
            return Ok(Self::default());
        };
        let directory = external(directory)?;
        if !directory.is_dir() {
            return Err(ApiError::invalid("ui_dir must be a built UI directory"));
        }
        let mut assets = BTreeMap::new();
        let mut total = 0;
        for &(name, mime) in CATALOG {
            let path = external(&directory.join(name))?;
            if path.parent() != Some(directory.as_path()) || !path.is_file() {
                return Err(ApiError::invalid(
                    "UI assets must be regular files inside ui_dir",
                ));
            }
            let mut bytes = Vec::new();
            File::open(path)?
                .take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut bytes)?;
            total += bytes.len();
            if bytes.len() as u64 > MAX_FILE_BYTES || total > MAX_TOTAL_BYTES {
                return Err(ApiError::invalid("UI asset size limit exceeded"));
            }
            assets.insert(name, (mime, Bytes::from(bytes)));
        }
        Ok(Self(assets))
    }

    pub(super) fn get(&self, name: &str) -> Option<(&'static str, Bytes)> {
        self.0.get(name).cloned()
    }
}
