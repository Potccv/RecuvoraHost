//! Browser URLs resolved only against trusted deployment bindings.
use reqwest::Url;

fn clean(value: &str) -> bool {
    !value
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
}

fn decode(value: &str) -> Result<Vec<u8>, &'static str> {
    let mut decoded = Vec::new();
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hi = bytes.next().and_then(|b| (b as char).to_digit(16));
            let lo = bytes.next().and_then(|b| (b as char).to_digit(16));
            let (Some(hi), Some(lo)) = (hi, lo) else {
                return Err("invalid URL escape");
            };
            decoded.push((hi * 16 + lo) as u8);
        } else {
            decoded.push(byte);
        }
    }
    Ok(decoded)
}

fn path(value: &str) -> Result<(), &'static str> {
    for segment in value.split('/') {
        let decoded = decode(segment)?;
        if matches!(decoded.as_slice(), b"." | b"..")
            || decoded
                .iter()
                .any(|b| b.is_ascii_control() || matches!(b, b'/' | b'\\' | b'%'))
        {
            return Err("unsafe page path");
        }
    }
    Ok(())
}

pub(super) fn base_url(value: &str) -> Result<Url, &'static str> {
    if value.len() > 4096 || !clean(value) {
        return Err("invalid page base URL");
    }
    let url = Url::parse(value).map_err(|_| "invalid page base URL")?;
    let rest = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .ok_or("page base URL requires http:// or https://")?;
    let (authority, raw_path) = rest
        .split_once('/')
        .ok_or("page base URL requires a directory path")?;
    if authority.is_empty()
        || authority.contains('@')
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
        || !value.ends_with('/')
    {
        return Err("invalid page base URL");
    }
    path(raw_path)?;
    Ok(url)
}

pub(super) fn relative_url(value: &str) -> Result<(), &'static str> {
    if value.len() > 2048 || !clean(value) || value.starts_with('/') {
        return Err("invalid relative page URL");
    }
    decode(value)?;
    let raw_path = value.split(['?', '#']).next().unwrap_or_default();
    if raw_path.split('/').next().unwrap_or_default().contains(':') {
        return Err("absolute page URL is not allowed");
    }
    path(raw_path)
}

pub(super) fn resolve(base: &str, relative: &str) -> Result<String, &'static str> {
    let base = base_url(base)?;
    relative_url(relative)?;
    let resolved = base.join(relative).map_err(|_| "invalid page URL")?;
    if resolved.origin() != base.origin()
        || !resolved.path().starts_with(base.path())
        || resolved.as_str().len() > 4096
    {
        return Err("page URL is outside its binding");
    }
    Ok(resolved.into())
}
