//! Run one WebSocket session and measure it.
//!
//! The session is the [`ResolvedWsRequest`]: connect, then for each message
//! send it and honour its wait, then close. The same function serves the app
//! (which wants every frame, to show) and the load engine (which wants the
//! numbers and nothing else), so `keep_frames` decides whether the transcript
//! is retained.
//!
//! What "latency" means here is fixed by the model rather than the transport:
//! for a message that waits, the time from sending it to the first frame that
//! arrives. Nothing else on a WebSocket has a defensible latency.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use futures::{FutureExt, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Message};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

pub use swarmo_core::model_ws::{ResolvedWsMessage, ResolvedWsRequest, WsPayloadKind, WsWait};

#[derive(Debug, thiserror::Error)]
pub enum WsError {
    #[error("{0}")]
    Invalid(String),
    #[error("could not connect to {url}: {detail}")]
    Connect { url: String, detail: String },
    #[error("the handshake did not complete within {0}ms")]
    ConnectTimeout(u64),
}

pub type Result<T> = std::result::Result<T, WsError>;

/// Which way a frame went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    Out,
    In,
}

/// One frame of the transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsFrame {
    pub direction: Direction,
    pub kind: WsPayloadKind,
    /// Text, or base64 for binary.
    pub body: String,
    pub bytes: u64,
    /// Milliseconds since the session started.
    pub at_ms: f64,
}

/// A message that waited, and what it got.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsExchange {
    /// Index into the resolved message list.
    pub message_index: usize,
    /// Send-to-first-frame. `None` when nothing arrived in time.
    pub latency_ms: Option<f64>,
    pub frames_received: u64,
    pub bytes_out: u64,
    pub bytes_in: u64,
    pub timed_out: bool,
}

/// The measured session.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WsResult {
    pub connected: bool,
    /// Handshake time, when it connected.
    pub connect_ms: Option<f64>,
    /// The whole session, connect to close.
    pub duration_ms: f64,
    pub messages_sent: u64,
    pub messages_received: u64,
    pub bytes_out: u64,
    pub bytes_in: u64,
    /// Only the messages that waited have an entry here.
    pub exchanges: Vec<WsExchange>,
    /// The close code the server sent, or that we sent, if the session closed
    /// cleanly. 1006 is the reserved "connection dropped" code.
    pub close_code: Option<u16>,
    pub close_reason: String,
    /// Kept only when asked for; the load engine never wants it.
    pub frames: Vec<WsFrame>,
    /// A failure after connecting — a wait that timed out, an aborted
    /// connection. `None` on a clean session.
    pub error: Option<String>,
    /// The subprotocol the server agreed to, if any.
    pub subprotocol: Option<String>,
}

impl WsResult {
    /// Whether the session did what it was asked: connected, every wait was
    /// answered, and nothing broke.
    pub fn ok(&self) -> bool {
        self.connected && self.error.is_none() && self.exchanges.iter().all(|e| !e.timed_out)
    }
}

/// The most frames kept in a transcript, so a subscription that pushes
/// forever cannot grow memory without bound.
pub const MAX_KEPT_FRAMES: usize = 5_000;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Run the session to completion, or until cancelled.
///
/// Cancellation is checked at every await. A cancelled session returns what
/// it had, with `error` set, rather than an `Err` — the frames and timings
/// already collected are still worth showing.
pub async fn run(
    req: &ResolvedWsRequest,
    cancel: &CancellationToken,
    keep_frames: bool,
) -> Result<WsResult> {
    if req.url.trim().is_empty() {
        return Err(WsError::Invalid(
            "Enter a ws:// or wss:// URL first.".into(),
        ));
    }
    if !(req.url.starts_with("ws://") || req.url.starts_with("wss://")) {
        return Err(WsError::Invalid(format!(
            "\"{}\" is not a WebSocket URL; it should start with ws:// or wss://",
            req.url
        )));
    }

    // Binary bodies are decoded before connecting, so a malformed one is
    // refused up front rather than discarding a session already under way.
    let mut outgoing: Vec<(Message, u64)> = Vec::with_capacity(req.messages.len());
    for (i, m) in req.messages.iter().enumerate() {
        outgoing.push(match m.kind {
            WsPayloadKind::Text => (Message::Text(m.body.clone()), m.body.len() as u64),
            WsPayloadKind::Binary => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(m.body.trim())
                    .map_err(|e| {
                        WsError::Invalid(format!("message {} is not valid base64: {e}", i + 1))
                    })?;
                let n = bytes.len() as u64;
                (Message::Binary(bytes), n)
            }
        });
    }

    let started = Instant::now();
    let at = |t: Instant| t.duration_since(started).as_secs_f64() * 1000.0;
    let mut out = WsResult::default();

    // -- connect --------------------------------------------------------------
    let mut request = req
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| WsError::Invalid(format!("bad URL: {e}")))?;
    for (k, v) in &req.headers {
        let name = http::header::HeaderName::from_bytes(k.trim().as_bytes())
            .map_err(|e| WsError::Invalid(format!("bad header name \"{k}\": {e}")))?;
        let value = http::header::HeaderValue::from_str(v)
            .map_err(|e| WsError::Invalid(format!("bad value for header \"{k}\": {e}")))?;
        request.headers_mut().insert(name, value);
    }
    if !req.subprotocols.is_empty() {
        let joined = req.subprotocols.join(", ");
        if let Ok(v) = http::header::HeaderValue::from_str(&joined) {
            request.headers_mut().insert("Sec-WebSocket-Protocol", v);
        }
    }

    let connector = if req.settings.verify_tls {
        None
    } else {
        Some(Connector::Rustls(Arc::new(insecure_tls_config())))
    };

    let connect = tokio_tungstenite::connect_async_tls_with_config(request, None, false, connector);
    let connect_timeout = Duration::from_millis(req.settings.connect_timeout_ms.max(1));
    let (mut socket, response): (Socket, _) = tokio::select! {
        r = tokio::time::timeout(connect_timeout, connect) => match r {
            Err(_) => return Err(WsError::ConnectTimeout(req.settings.connect_timeout_ms)),
            Ok(Err(e)) => {
                return Err(WsError::Connect {
                    url: req.url.clone(),
                    detail: e.to_string(),
                })
            }
            Ok(Ok(pair)) => pair,
        },
        _ = cancel.cancelled() => {
            out.error = Some("cancelled".into());
            out.duration_ms = at(Instant::now());
            return Ok(out);
        }
    };
    out.connected = true;
    out.connect_ms = Some(at(Instant::now()));
    out.subprotocol = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let wait_timeout = Duration::from_millis(req.settings.timeout_ms.max(1));

    // -- the session ------------------------------------------------------------
    let total = req.messages.len();
    'messages: for (i, (m, (frame, sent_bytes))) in req.messages.iter().zip(outgoing).enumerate() {
        // Whatever is already buffered arrived before this message went out,
        // so it cannot be its reply: the answer to an earlier message that did
        // not wait, or frames past what a Count asked for. Left in place it
        // would satisfy this message's wait with a near-zero latency. It still
        // counts in the totals and the transcript. Before the first message
        // there is nothing to be left over from, so a greeting or a feed that
        // starts on connect still counts toward the first wait.
        if i > 0 {
            while let Some(next) = socket.next().now_or_never() {
                let now = Instant::now();
                match next {
                    Some(Ok(Message::Close(frame))) => {
                        accept_close(&mut socket, &mut out, frame).await;
                        out.error = Some(cut_short(i, total));
                        break 'messages;
                    }
                    Some(Ok(msg)) => note_inbound(&mut out, None, &msg, now, at(now), keep_frames),
                    Some(Err(e)) => {
                        out.error = Some(format!("read failed: {e}"));
                        out.close_code = Some(1006);
                        break 'messages;
                    }
                    None => {
                        out.error = Some("the connection closed unexpectedly".into());
                        out.close_code = Some(1006);
                        break 'messages;
                    }
                }
            }
        }

        // A server that stops reading lets the send block for good once the
        // socket buffers fill, so it gets the wait budget and honours cancel.
        let sent_at = Instant::now();
        let sent = tokio::select! {
            r = tokio::time::timeout(wait_timeout, socket.send(frame)) => r,
            _ = cancel.cancelled() => {
                out.error = Some("cancelled".into());
                break;
            }
        };
        match sent {
            Err(_) => {
                out.error = Some(format!(
                    "send timed out on message {} after {}ms",
                    i + 1,
                    wait_timeout.as_millis()
                ));
                break;
            }
            Ok(Err(e)) => {
                out.error = Some(format!("send failed on message {}: {e}", i + 1));
                break;
            }
            Ok(Ok(())) => {}
        }
        out.messages_sent += 1;
        out.bytes_out += sent_bytes;
        if keep_frames && out.frames.len() < MAX_KEPT_FRAMES {
            out.frames.push(WsFrame {
                direction: Direction::Out,
                kind: m.kind,
                body: m.body.clone(),
                bytes: sent_bytes,
                at_ms: at(sent_at),
            });
        }

        // The wait, if any.
        let (target_frames, window): (Option<u64>, Option<Duration>) = match &m.wait {
            WsWait::None => continue,
            WsWait::Reply => (Some(1), None),
            WsWait::Count { count } => (Some(u64::from(*count).max(1)), None),
            WsWait::Millis { ms } => (None, Some(Duration::from_millis(*ms))),
        };
        let mut exchange = WsExchange {
            message_index: i,
            latency_ms: None,
            frames_received: 0,
            bytes_out: sent_bytes,
            bytes_in: 0,
            timed_out: false,
        };
        let deadline = sent_at + window.unwrap_or(wait_timeout);

        loop {
            if target_frames.is_some_and(|n| exchange.frames_received >= n) {
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // A timed window simply ends; a wait for frames that never
                // came is a failure of that message — and it cost the whole
                // wait, which is what its latency records.
                if window.is_none() {
                    exchange.timed_out = true;
                    exchange
                        .latency_ms
                        .get_or_insert(sent_at.elapsed().as_secs_f64() * 1000.0);
                }
                break;
            }
            let next = tokio::select! {
                r = tokio::time::timeout(left, socket.next()) => r,
                _ = cancel.cancelled() => {
                    out.error = Some("cancelled".into());
                    out.exchanges.push(exchange);
                    break 'messages;
                }
            };
            match next {
                Err(_) => {
                    if window.is_none() {
                        exchange.timed_out = true;
                        exchange
                            .latency_ms
                            .get_or_insert(sent_at.elapsed().as_secs_f64() * 1000.0);
                    }
                    break;
                }
                Ok(None) => {
                    out.error = Some("the connection closed unexpectedly".into());
                    out.close_code = Some(1006);
                    exchange.timed_out = window.is_none();
                    out.exchanges.push(exchange);
                    break 'messages;
                }
                Ok(Some(Err(e))) => {
                    out.error = Some(format!("read failed: {e}"));
                    out.close_code = Some(1006);
                    exchange.timed_out = window.is_none();
                    out.exchanges.push(exchange);
                    break 'messages;
                }
                Ok(Some(Ok(Message::Close(frame)))) => {
                    accept_close(&mut socket, &mut out, frame).await;
                    // Closed with frames still owed: the wait was not met. A
                    // timed window is simply cut short.
                    exchange.timed_out = window.is_none()
                        && target_frames.is_some_and(|n| exchange.frames_received < n);
                    // Messages the script never got to send are a failure of
                    // the session, even when this one's wait was satisfied.
                    if i + 1 < total {
                        out.error = Some(cut_short(i + 1, total));
                    }
                    out.exchanges.push(exchange);
                    break 'messages;
                }
                // Pings are answered by the library on the next flush;
                // neither they nor pongs are messages, and note_inbound
                // ignores them.
                Ok(Some(Ok(msg))) => {
                    let now = Instant::now();
                    note_inbound(
                        &mut out,
                        Some((&mut exchange, sent_at)),
                        &msg,
                        now,
                        at(now),
                        keep_frames,
                    );
                }
            }
        }
        out.exchanges.push(exchange);
    }

    // -- close -------------------------------------------------------------------
    if out.close_code.is_none() && out.error.is_none() && req.settings.close_after {
        // Send and acknowledgement share one budget: a server that has
        // stopped reading must not hang the session here either.
        let ack = tokio::time::timeout(Duration::from_millis(500), async {
            socket.send(Message::Close(None)).await.ok()?;
            while let Some(Ok(msg)) = socket.next().await {
                if let Message::Close(frame) = msg {
                    return frame.map(|f| (u16::from(f.code), f.reason.to_string()));
                }
            }
            None
        })
        .await;
        match ack {
            Ok(Some((code, reason))) => {
                out.close_code = Some(code);
                out.close_reason = reason;
            }
            _ => out.close_code = Some(1000),
        }
    }

    out.duration_ms = at(Instant::now());
    Ok(out)
}

/// The error for a session the server closed before the script was done.
fn cut_short(sent: usize, total: usize) -> String {
    format!("the server closed the connection after {sent} of {total} messages")
}

/// Record a close the server started, and answer it.
///
/// tungstenite only queues the reply, to go out on the next read or write.
/// The session reads nothing more after a close, so without this flush the
/// socket is dropped first and the server sees a reset rather than a
/// completed closing handshake.
async fn accept_close(socket: &mut Socket, out: &mut WsResult, frame: Option<CloseFrame<'static>>) {
    out.close_code = Some(frame.as_ref().map(|f| u16::from(f.code)).unwrap_or(1005));
    out.close_reason = frame.map(|f| f.reason.to_string()).unwrap_or_default();
    let _ = tokio::time::timeout(Duration::from_millis(500), socket.flush()).await;
}

/// Count an inbound data frame, and attribute it to the exchange waiting for
/// it, if there is one. Control frames are not messages and are ignored.
fn note_inbound(
    out: &mut WsResult,
    waiting: Option<(&mut WsExchange, Instant)>,
    msg: &Message,
    now: Instant,
    at_ms: f64,
    keep_frames: bool,
) {
    let (kind, bytes) = match msg {
        Message::Text(t) => (WsPayloadKind::Text, t.len() as u64),
        Message::Binary(b) => (WsPayloadKind::Binary, b.len() as u64),
        _ => return,
    };
    if let Some((exchange, sent_at)) = waiting {
        if exchange.latency_ms.is_none() {
            exchange.latency_ms = Some(now.duration_since(sent_at).as_secs_f64() * 1000.0);
        }
        exchange.frames_received += 1;
        exchange.bytes_in += bytes;
    }
    out.messages_received += 1;
    out.bytes_in += bytes;
    if keep_frames && out.frames.len() < MAX_KEPT_FRAMES {
        let body = match msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => base64::engine::general_purpose::STANDARD.encode(b),
            _ => String::new(),
        };
        out.frames.push(WsFrame {
            direction: Direction::In,
            kind,
            body,
            bytes,
            at_ms,
        });
    }
}

/// A TLS config that accepts any certificate, for `verifyTls: false`. Same
/// escape hatch the HTTP and gRPC paths offer, for the same local-dev reason.
fn insecure_tls_config() -> rustls::ClientConfig {
    #[derive(Debug)]
    struct AcceptAll;
    impl rustls::client::danger::ServerCertVerifier for AcceptAll {
        fn verify_server_cert(
            &self,
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &[rustls::pki_types::CertificateDer<'_>],
            _: &rustls::pki_types::ServerName<'_>,
            _: &[u8],
            _: rustls::pki_types::UnixTime,
        ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error>
        {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
        {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
        {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }
    rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAll))
        .with_no_client_auth()
}
