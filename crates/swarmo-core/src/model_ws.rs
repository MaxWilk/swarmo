//! WebSocket requests.
//!
//! A WebSocket "request" is not one exchange but a short **session**: connect,
//! send some messages, wait for what comes back, close. That is the artifact
//! both the client and the load engine execute — the same file drives a
//! debugging session in the app and a thousand concurrent sessions in a run —
//! which is the whole point of having it be one thing.
//!
//! Latency on a WebSocket has no natural definition, so it is defined here:
//! for a message that waits for a reply, the time from sending it to the first
//! frame that arrives. A message sent without waiting has no latency; it still
//! counts in bytes.

use serde::{Deserialize, Serialize};

use crate::interp::{interpolate, VarScope};
use crate::model::{
    default_true, default_version, new_uuid, Auth, ContainerDef, KeyValue, FORMAT_VERSION,
};

/// What a frame carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum WsPayloadKind {
    #[default]
    Text,
    /// `body` is base64 of the bytes.
    Binary,
}

/// What to do after sending a message, before sending the next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WsWait {
    /// Send and move straight on. No latency is recorded for this message.
    #[default]
    None,
    /// Wait for the next frame from the server. The request/response idiom:
    /// latency is send-to-first-frame.
    Reply,
    /// Wait for this many frames.
    Count { count: u32 },
    /// Collect whatever arrives for this long.
    Millis { ms: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsMessageDef {
    #[serde(default)]
    pub kind: WsPayloadKind,
    /// Text, or base64 for a binary frame. Interpolated like any body.
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub wait: WsWait,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsSettings {
    /// How long the handshake may take.
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    /// How long a single wait may take. A wait that times out fails the
    /// message rather than the session, so a slow reply is a slow reply and
    /// not a broken run.
    #[serde(default = "default_wait_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub verify_tls: bool,
    /// Close the connection cleanly once every message has been handled.
    /// Off, the session ends when the server closes it or the wait budget
    /// runs out — for a subscription that pushes indefinitely, pair this with
    /// a final `Millis` wait.
    #[serde(default = "default_true")]
    pub close_after: bool,
}

fn default_connect_timeout_ms() -> u64 {
    10_000
}

fn default_wait_timeout_ms() -> u64 {
    10_000
}

impl Default for WsSettings {
    fn default() -> Self {
        Self {
            connect_timeout_ms: default_connect_timeout_ms(),
            timeout_ms: default_wait_timeout_ms(),
            verify_tls: true,
            close_after: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsRequestDef {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "new_uuid")]
    pub id: String,
    pub name: String,
    /// `ws://` or `wss://`. `http(s)://` is accepted and mapped.
    #[serde(default)]
    pub url: String,
    /// Handshake headers. Auth becomes one of these at resolve time.
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    /// Offered as `Sec-WebSocket-Protocol`, in order.
    #[serde(default)]
    pub subprotocols: Vec<String>,
    #[serde(default)]
    pub auth: Auth,
    /// The session, in order.
    #[serde(default)]
    pub messages: Vec<WsMessageDef>,
    #[serde(default)]
    pub settings: WsSettings,
}

impl WsRequestDef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: FORMAT_VERSION,
            id: new_uuid(),
            name: name.into(),
            url: String::new(),
            headers: Vec::new(),
            subprotocols: Vec::new(),
            auth: Auth::Inherit,
            messages: vec![WsMessageDef {
                kind: WsPayloadKind::Text,
                body: "{}".into(),
                wait: WsWait::Reply,
                enabled: true,
            }],
            settings: WsSettings::default(),
        }
    }
}

/// Post-inheritance, pre-interpolation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedWsRequest {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Ancestors' headers first, so the request's own win on collision.
    pub headers: Vec<KeyValue>,
    pub subprotocols: Vec<String>,
    pub auth: Auth,
    pub messages: Vec<WsMessageDef>,
    pub settings: WsSettings,
}

/// Fold collection and folder settings into the request.
///
/// Headers accumulate outermost-first; auth is the nearest explicit setting.
/// Scripts are deliberately not carried: a WebSocket session has no single
/// "response" for a post-response script to run against.
pub fn merge_ws_chain(ancestors: &[ContainerDef], req: &WsRequestDef) -> MergedWsRequest {
    let auth = if !matches!(req.auth, Auth::Inherit) {
        req.auth.clone()
    } else {
        ancestors
            .iter()
            .rev()
            .find(|a| !matches!(a.auth, Auth::Inherit))
            .map(|a| a.auth.clone())
            .unwrap_or(Auth::None)
    };

    let mut headers: Vec<KeyValue> = Vec::new();
    for a in ancestors {
        headers.extend(a.headers.iter().cloned());
    }
    headers.extend(req.headers.iter().cloned());

    MergedWsRequest {
        id: req.id.clone(),
        name: req.name.clone(),
        url: req.url.clone(),
        headers,
        subprotocols: req.subprotocols.clone(),
        auth,
        messages: req.messages.clone(),
        settings: req.settings.clone(),
    }
}

/// One message, ready to send.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedWsMessage {
    pub kind: WsPayloadKind,
    pub body: String,
    pub wait: WsWait,
}

/// Post-interpolation, ready to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedWsRequest {
    pub name: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub subprotocols: Vec<String>,
    pub messages: Vec<ResolvedWsMessage>,
    pub settings: WsSettings,
    pub unresolved: Vec<String>,
}

/// Interpolate everything and produce a directly runnable session.
pub fn finalize_ws(merged: &MergedWsRequest, scope: &VarScope) -> ResolvedWsRequest {
    let mut unresolved: Vec<String> = Vec::new();
    let mut interp = |s: &str| -> String {
        let (out, un) = interpolate(s, scope);
        for u in un {
            if !unresolved.contains(&u.name) {
                unresolved.push(u.name);
            }
        }
        out
    };

    // `http(s)://` is what people paste; the socket scheme is what the
    // handshake needs.
    let mut url = interp(&merged.url).trim().to_string();
    if let Some(rest) = url.strip_prefix("https://") {
        url = format!("wss://{rest}");
    } else if let Some(rest) = url.strip_prefix("http://") {
        url = format!("ws://{rest}");
    }

    let mut headers: Vec<(String, String)> = merged
        .headers
        .iter()
        .filter(|h| h.enabled && !h.key.trim().is_empty())
        .map(|h| (interp(&h.key), interp(&h.value)))
        .collect();

    // Auth -> handshake header, the same mapping HTTP uses.
    match &merged.auth {
        Auth::Inherit | Auth::None => {}
        Auth::Basic { username, password } => {
            use base64::Engine as _;
            let raw = format!("{}:{}", interp(username), interp(password));
            let b64 = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
            set_header(&mut headers, "Authorization", format!("Basic {b64}"));
        }
        Auth::Bearer { token } => {
            let t = interp(token);
            set_header(&mut headers, "Authorization", format!("Bearer {t}"));
        }
        Auth::ApiKeyHeader { header_name, value } => {
            let n = interp(header_name);
            let v = interp(value);
            if !n.trim().is_empty() {
                set_header(&mut headers, &n, v);
            }
        }
        // As for HTTP: the caller resolves the token and hands over an
        // ApiKeyHeader; this crate cannot run a process.
        Auth::CommandToken { .. } => {}
    }

    let messages = merged
        .messages
        .iter()
        .filter(|m| m.enabled)
        .map(|m| ResolvedWsMessage {
            kind: m.kind,
            body: interp(&m.body),
            wait: m.wait.clone(),
        })
        .collect();

    ResolvedWsRequest {
        name: merged.name.clone(),
        url,
        headers,
        subprotocols: merged.subprotocols.iter().map(|s| interp(s)).collect(),
        messages,
        settings: merged.settings.clone(),
        unresolved,
    }
}

fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    match headers
        .iter_mut()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
    {
        Some(h) => h.1 = value,
        None => headers.push((name.to_string(), value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(pairs: &[(&str, &str)]) -> VarScope {
        let mut s = VarScope::new();
        for (k, v) in pairs {
            s.set(*k, *v);
        }
        s
    }

    #[test]
    fn http_schemes_are_mapped_to_socket_schemes() {
        let mut def = WsRequestDef::new("x");
        def.url = "https://{{host}}/live".into();
        let r = finalize_ws(&merge_ws_chain(&[], &def), &scope(&[("host", "ex.test")]));
        assert_eq!(r.url, "wss://ex.test/live");
        assert!(r.unresolved.is_empty());
    }

    #[test]
    fn auth_becomes_a_handshake_header_and_ancestors_headers_fold_in() {
        let mut coll = ContainerDef::new("c");
        coll.headers.push(KeyValue::new("X-Team", "ml"));
        coll.auth = Auth::Bearer {
            token: "{{tok}}".into(),
        };
        let def = WsRequestDef::new("x");
        let r = finalize_ws(&merge_ws_chain(&[coll], &def), &scope(&[("tok", "abc")]));
        assert!(r.headers.iter().any(|(k, v)| k == "X-Team" && v == "ml"));
        assert!(r
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer abc"));
    }

    #[test]
    fn disabled_messages_are_left_out_and_bodies_are_interpolated() {
        let mut def = WsRequestDef::new("x");
        def.messages = vec![
            WsMessageDef {
                kind: WsPayloadKind::Text,
                body: r#"{"id":"{{id}}"}"#.into(),
                wait: WsWait::Reply,
                enabled: true,
            },
            WsMessageDef {
                kind: WsPayloadKind::Text,
                body: "skip".into(),
                wait: WsWait::None,
                enabled: false,
            },
        ];
        let r = finalize_ws(&merge_ws_chain(&[], &def), &scope(&[("id", "7")]));
        assert_eq!(r.messages.len(), 1);
        assert_eq!(r.messages[0].body, r#"{"id":"7"}"#);
    }

    #[test]
    fn a_wait_round_trips_through_json_by_tag() {
        let w: WsWait = serde_json::from_str(r#"{"kind":"count","count":3}"#).unwrap();
        assert_eq!(w, WsWait::Count { count: 3 });
        let w: WsWait = serde_json::from_str(r#"{"kind":"reply"}"#).unwrap();
        assert_eq!(w, WsWait::Reply);
    }
}
