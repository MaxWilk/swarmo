//! Executing a `ResolvedRequest` and packaging the response for the UI.

use std::path::PathBuf;
use std::time::Instant;

use base64::Engine as _;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use swarmo_core::resolve::{ResolvedBody, ResolvedRequest};
use swarmo_core::MultipartKind;

use crate::client::{ClientPool, ClientSettings};
use crate::{MAX_INLINE_BODY, MAX_PRETTY_BODY};

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("request failed: {0}")]
    Transport(String),
    #[error("cancelled")]
    Cancelled,
    #[error("io error: {0}")]
    Io(String),
}

impl ExecError {
    /// A short, user-facing category shown next to the error message.
    pub fn kind(&self) -> &'static str {
        match self {
            ExecError::Invalid(_) => "invalid",
            ExecError::Transport(_) => "transport",
            ExecError::Cancelled => "cancelled",
            ExecError::Io(_) => "io",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timings {
    /// Time until response headers were available.
    pub ttfb_ms: f64,
    /// Time spent reading the body after headers.
    pub download_ms: f64,
    pub total_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseHeader {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BodyPreview {
    /// Decoded text (possibly pretty-printed JSON).
    #[serde(rename_all = "camelCase")]
    Text {
        text: String,
        /// The original, un-prettified text.
        raw: String,
        /// "json" | "xml" | "html" | "text"
        language: String,
    },
    #[serde(rename_all = "camelCase")]
    Image {
        data_url: String,
    },
    /// Too large for memory; written to a temp file.
    #[serde(rename_all = "camelCase")]
    File {
        path: String,
        size: u64,
    },
    #[serde(rename_all = "camelCase")]
    Binary {
        size: u64,
        base64_head: String,
    },
    Empty,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieRecord {
    pub domain: String,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult {
    pub status: u16,
    pub status_text: String,
    pub http_version: String,
    pub headers: Vec<ResponseHeader>,
    pub body: BodyPreview,
    pub body_size: u64,
    pub timings: Timings,
    pub final_url: String,
    pub set_cookies: Vec<CookieRecord>,
}

#[derive(Debug, Clone)]
pub struct ExecOpts {
    pub proxy: Option<String>,
    /// Where oversized bodies are written.
    pub temp_dir: PathBuf,
}

impl Default for ExecOpts {
    fn default() -> Self {
        Self {
            proxy: None,
            temp_dir: std::env::temp_dir().join("swarmo"),
        }
    }
}

/// Send one request and read the whole response.
pub async fn execute(
    pool: &ClientPool,
    req: &ResolvedRequest,
    opts: &ExecOpts,
) -> Result<ExecResult, ExecError> {
    if req.url.trim().is_empty() {
        return Err(ExecError::Invalid("URL is empty".into()));
    }

    let url = normalize_url(&req.url)?;

    let settings = ClientSettings {
        follow_redirects: req.settings.follow_redirects,
        verify_tls: req.settings.verify_tls,
        timeout_ms: req.settings.timeout_ms,
        proxy: opts.proxy.clone(),
    };
    let client = pool
        .get(&settings)
        .map_err(|e| ExecError::Transport(e.to_string()))?;

    let builder = build_request(&client, req, &url).await?;

    let started = Instant::now();
    let resp = builder
        .send()
        .await
        .map_err(|e| ExecError::Transport(friendly_transport_error(&e)))?;
    let ttfb = started.elapsed();

    let status = resp.status();
    let http_version = format!("{:?}", resp.version());
    let final_url = resp.url().to_string();

    let mut headers = Vec::new();
    let mut set_cookies = Vec::new();
    let host = resp.url().host_str().unwrap_or_default().to_string();
    for (name, value) in resp.headers().iter() {
        let v = value.to_str().unwrap_or("<non-utf8 header>").to_string();
        if name.as_str().eq_ignore_ascii_case("set-cookie") {
            set_cookies.push(CookieRecord {
                domain: host.clone(),
                raw: v.clone(),
            });
        }
        headers.push(ResponseHeader {
            key: name.to_string(),
            value: v,
        });
    }

    let content_type = headers
        .iter()
        .find(|h| h.key.eq_ignore_ascii_case("content-type"))
        .map(|h| h.value.clone())
        .unwrap_or_default();

    let (body, body_size) = read_body(resp, &content_type, opts).await?;
    let total = started.elapsed();

    Ok(ExecResult {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("").to_string(),
        http_version,
        headers,
        body,
        body_size,
        timings: Timings {
            ttfb_ms: ttfb.as_secs_f64() * 1000.0,
            download_ms: (total.saturating_sub(ttfb)).as_secs_f64() * 1000.0,
            total_ms: total.as_secs_f64() * 1000.0,
        },
        final_url,
        set_cookies,
    })
}

/// Accept "example.com/x" by defaulting the scheme to https.
pub fn normalize_url(raw: &str) -> Result<String, ExecError> {
    let t = raw.trim();
    // Schemes are case-insensitive: `HTTPS://host` is a valid URL.
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Ok(t.to_string())
    } else if t.contains("://") {
        Err(ExecError::Invalid(format!("unsupported URL scheme in {t}")))
    } else {
        Ok(format!("https://{t}"))
    }
}

/// Build a `RequestBuilder` from a resolved request. Shared with the load
/// engine so both paths construct requests identically.
pub async fn build_request(
    client: &reqwest::Client,
    req: &ResolvedRequest,
    url: &str,
) -> Result<reqwest::RequestBuilder, ExecError> {
    let method = reqwest::Method::from_bytes(req.method.as_bytes())
        .map_err(|_| ExecError::Invalid(format!("invalid HTTP method: {}", req.method)))?;

    let is_content_type = |k: &str| k.trim().eq_ignore_ascii_case("content-type");
    let multipart = matches!(req.body, ResolvedBody::Multipart { .. });

    let mut builder = client.request(method, url);
    for (k, v) in &req.headers {
        if k.trim().is_empty() {
            continue;
        }
        // `RequestBuilder::header` appends, and `multipart()` adds its own
        // Content-Type carrying the boundary. A second one from the headers
        // (a Postman-style bare `multipart/form-data`, or an inherited
        // `application/json`) would shadow it on servers that read the first.
        if multipart && is_content_type(k) {
            continue;
        }
        builder = builder.header(k, v);
    }
    let has_content_type = req.headers.iter().any(|(k, _)| is_content_type(k));
    apply_body(builder, &req.body, has_content_type).await
}

async fn apply_body(
    builder: reqwest::RequestBuilder,
    body: &ResolvedBody,
    has_content_type: bool,
) -> Result<reqwest::RequestBuilder, ExecError> {
    Ok(match body {
        ResolvedBody::None => builder,
        ResolvedBody::Bytes { text, .. } => builder.body(text.clone()),
        ResolvedBody::Form { fields } => builder.form(fields),
        ResolvedBody::File { path } => {
            let p = PathBuf::from(path);
            let bytes = tokio::fs::read(&p).await.map_err(|e| {
                ExecError::Io(format!("cannot read body file {}: {e}", p.display()))
            })?;
            let b = builder.body(bytes);
            // Guess only when the user set nothing; `header` appends, so a
            // guess would otherwise go out as a second Content-Type.
            match mime_guess::from_path(&p).first_raw() {
                Some(ct) if !has_content_type => b.header(reqwest::header::CONTENT_TYPE, ct),
                _ => b,
            }
        }
        ResolvedBody::Multipart { parts } => {
            let mut form = reqwest::multipart::Form::new();
            for p in parts {
                match p.kind {
                    MultipartKind::Text => {
                        let mut part = reqwest::multipart::Part::text(p.value.clone());
                        if let Some(ct) = &p.content_type {
                            part = part
                                .mime_str(ct)
                                .map_err(|e| ExecError::Invalid(e.to_string()))?;
                        }
                        form = form.part(p.key.clone(), part);
                    }
                    MultipartKind::File => {
                        let path = PathBuf::from(&p.value);
                        let bytes = tokio::fs::read(&path).await.map_err(|e| {
                            ExecError::Io(format!(
                                "cannot read multipart file {}: {e}",
                                path.display()
                            ))
                        })?;
                        let filename = path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| "file".into());
                        let ct = p
                            .content_type
                            .clone()
                            .or_else(|| mime_guess::from_path(&path).first_raw().map(String::from))
                            .unwrap_or_else(|| "application/octet-stream".into());
                        let part = reqwest::multipart::Part::bytes(bytes)
                            .file_name(filename)
                            .mime_str(&ct)
                            .map_err(|e| ExecError::Invalid(e.to_string()))?;
                        form = form.part(p.key.clone(), part);
                    }
                }
            }
            builder.multipart(form)
        }
    })
}

async fn read_body(
    resp: reqwest::Response,
    content_type: &str,
    opts: &ExecOpts,
) -> Result<(BodyPreview, u64), ExecError> {
    // If the server advertises something huge, stream straight to disk.
    let declared = resp.content_length().unwrap_or(0);
    if declared as usize > MAX_INLINE_BODY {
        return stream_to_file(resp, opts).await;
    }

    let mut buf: Vec<u8> = Vec::with_capacity(declared.min(1 << 20) as usize);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ExecError::Transport(e.to_string()))?;
        buf.extend_from_slice(&chunk);
        if buf.len() > MAX_INLINE_BODY {
            // Overshot an undeclared length: spill what we have plus the rest.
            return spill_to_file(buf, stream, opts).await;
        }
    }

    let size = buf.len() as u64;
    Ok((preview_from_bytes(buf, content_type), size))
}

fn preview_from_bytes(buf: Vec<u8>, content_type: &str) -> BodyPreview {
    if buf.is_empty() {
        return BodyPreview::Empty;
    }
    let ct = content_type.to_ascii_lowercase();

    if ct.starts_with("image/") {
        let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
        let mime = ct.split(';').next().unwrap_or("image/png").trim();
        return BodyPreview::Image {
            data_url: format!("data:{mime};base64,{b64}"),
        };
    }

    let looks_texty = ct.is_empty()
        || ct.contains("json")
        || ct.contains("text")
        || ct.contains("xml")
        || ct.contains("javascript")
        || ct.contains("html")
        || ct.contains("csv")
        || ct.contains("urlencoded");

    if !looks_texty && buf.iter().take(1024).any(|b| *b == 0) {
        let head = base64::engine::general_purpose::STANDARD.encode(&buf[..buf.len().min(4096)]);
        return BodyPreview::Binary {
            size: buf.len() as u64,
            base64_head: head,
        };
    }

    let raw = String::from_utf8_lossy(&buf).into_owned();
    let language = if ct.contains("json") {
        "json"
    } else if ct.contains("html") {
        "html"
    } else if ct.contains("xml") {
        "xml"
    } else if ct.contains("javascript") {
        "javascript"
    } else if raw.trim_start().starts_with('{') || raw.trim_start().starts_with('[') {
        "json"
    } else {
        "text"
    };

    let text = if language == "json" && raw.len() <= MAX_PRETTY_BODY {
        match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.clone()),
            Err(_) => raw.clone(),
        }
    } else {
        raw.clone()
    };

    BodyPreview::Text {
        text,
        raw,
        language: language.to_string(),
    }
}

async fn stream_to_file(
    resp: reqwest::Response,
    opts: &ExecOpts,
) -> Result<(BodyPreview, u64), ExecError> {
    spill_to_file(Vec::new(), resp.bytes_stream(), opts).await
}

async fn spill_to_file<S>(
    head: Vec<u8>,
    mut stream: S,
    opts: &ExecOpts,
) -> Result<(BodyPreview, u64), ExecError>
where
    S: futures::Stream<Item = reqwest::Result<bytes_alias::Bytes>> + Unpin,
{
    use tokio::io::AsyncWriteExt;

    tokio::fs::create_dir_all(&opts.temp_dir)
        .await
        .map_err(|e| ExecError::Io(e.to_string()))?;
    let path = opts
        .temp_dir
        .join(format!("response-{}.bin", uuid::Uuid::new_v4().simple()));

    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|e| ExecError::Io(format!("cannot create {}: {e}", path.display())))?;

    let mut size = head.len() as u64;
    file.write_all(&head)
        .await
        .map_err(|e| ExecError::Io(e.to_string()))?;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ExecError::Transport(e.to_string()))?;
        size += chunk.len() as u64;
        file.write_all(&chunk)
            .await
            .map_err(|e| ExecError::Io(e.to_string()))?;
    }
    file.flush()
        .await
        .map_err(|e| ExecError::Io(e.to_string()))?;

    Ok((
        BodyPreview::File {
            path: path.to_string_lossy().to_string(),
            size,
        },
        size,
    ))
}

/// `reqwest` re-exports `bytes`, but not under a stable path we can name in a
/// generic bound, so alias it here.
mod bytes_alias {
    pub type Bytes = ::bytes::Bytes;
}

fn friendly_transport_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "request timed out".to_string();
    }
    if e.is_connect() {
        return format!("could not connect: {e}");
    }
    if e.is_redirect() {
        return format!("too many redirects: {e}");
    }
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_scheme_less_urls() {
        assert_eq!(normalize_url("example.com").unwrap(), "https://example.com");
        assert_eq!(
            normalize_url("http://example.com").unwrap(),
            "http://example.com"
        );
        assert!(normalize_url("ftp://x").is_err());
        assert_eq!(
            normalize_url("HTTPS://example.com").unwrap(),
            "HTTPS://example.com"
        );
    }

    #[test]
    fn pretty_prints_json_and_keeps_raw() {
        let p = preview_from_bytes(br#"{"a":1}"#.to_vec(), "application/json");
        match p {
            BodyPreview::Text {
                text,
                raw,
                language,
            } => {
                assert_eq!(language, "json");
                assert!(text.contains("\n"));
                assert_eq!(raw, r#"{"a":1}"#);
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn detects_json_without_content_type() {
        let p = preview_from_bytes(b"[1,2]".to_vec(), "");
        match p {
            BodyPreview::Text { language, .. } => assert_eq!(language, "json"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn images_become_data_urls() {
        let p = preview_from_bytes(vec![1, 2, 3], "image/png");
        match p {
            BodyPreview::Image { data_url } => {
                assert!(data_url.starts_with("data:image/png;base64,"))
            }
            _ => panic!("expected image"),
        }
    }

    #[test]
    fn binary_bodies_are_not_stringified() {
        let mut data = vec![0u8; 64];
        data[10] = 0;
        let p = preview_from_bytes(data, "application/octet-stream");
        assert!(matches!(p, BodyPreview::Binary { .. }));
    }

    #[test]
    fn empty_body() {
        assert!(matches!(
            preview_from_bytes(Vec::new(), "text/plain"),
            BodyPreview::Empty
        ));
    }
}
