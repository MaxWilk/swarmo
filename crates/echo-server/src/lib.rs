//! A tiny local HTTP server used by integration tests and manual QA.
//! It never touches the network beyond localhost.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use serde_json::json;

#[derive(Default)]
pub struct AppState {
    pub counter: AtomicU64,
    expiring_auth_seen: std::sync::Mutex<Option<String>>,
}

pub fn router() -> Router {
    let state = Arc::new(AppState::default());
    Router::new()
        .route("/", get(|| async { "swarmo echo server" }))
        .route("/echo", any(echo))
        .route("/echo/*rest", any(echo))
        .route("/json", get(json_handler))
        .route("/status/:code", any(status_handler))
        .route("/delay/:ms", any(delay_handler))
        .route("/image", get(image_handler))
        .route("/large/:kb", get(large_handler))
        .route("/count", any(count_handler))
        .route("/ws", get(ws_echo))
        .route("/ws/push/:n", get(ws_push))
        .route("/token", any(token_handler))
        .route("/cookie", get(cookie_handler))
        // Payload benchmarks send multi-megabyte bodies; axum's 2MB default
        // would fail them at the front door.
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024 * 1024))
        .route("/expiring-auth", any(expiring_auth_handler))
        .route("/breaks-above/:rps", any(breaks_above_handler))
        .route("/expiring-auth/reset", any(expiring_auth_reset))
        .with_state(state)
}

/// Bind on an ephemeral port and serve until the returned handle is dropped.
pub async fn spawn() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router()).await;
    });
    (addr, handle)
}

async fn echo(
    method: axum::http::Method,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let hdrs: serde_json::Map<String, serde_json::Value> = headers
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                json!(v.to_str().unwrap_or("<non-utf8>").to_string()),
            )
        })
        .collect();

    let body_text = String::from_utf8_lossy(&body).to_string();
    let body_json: Option<serde_json::Value> = serde_json::from_str(&body_text).ok();

    Json(json!({
        "method": method.as_str(),
        "query": params,
        "headers": hdrs,
        "body": body_text,
        "json": body_json,
        "bodyLen": body.len(),
    }))
}

async fn json_handler() -> impl IntoResponse {
    Json(json!({ "ok": true, "items": [1, 2, 3], "nested": { "deep": "value" } }))
}

async fn status_handler(Path(code): Path<u16>) -> impl IntoResponse {
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(json!({ "status": code })))
}

/// Echoes every text or binary frame straight back — the request/response
/// idiom over a socket, which is what a `Reply` wait measures.
async fn ws_echo(ws: axum::extract::ws::WebSocketUpgrade) -> impl IntoResponse {
    use axum::extract::ws::Message;
    ws.on_upgrade(|mut socket| async move {
        while let Some(Ok(msg)) = socket.recv().await {
            let reply = match msg {
                Message::Text(t) => Message::Text(t),
                Message::Binary(b) => Message::Binary(b),
                Message::Close(_) => break,
                _ => continue,
            };
            if socket.send(reply).await.is_err() {
                break;
            }
        }
    })
}

/// Pushes `n` numbered messages unprompted, then closes — a server-driven
/// feed, for `Count` and `Millis` waits.
async fn ws_push(ws: axum::extract::ws::WebSocketUpgrade, Path(n): Path<u32>) -> impl IntoResponse {
    use axum::extract::ws::Message;
    ws.on_upgrade(move |mut socket| async move {
        for i in 0..n.min(10_000) {
            if socket.send(Message::Text(i.to_string())).await.is_err() {
                return;
            }
        }
        let _ = socket.send(Message::Close(None)).await;
    })
}

/// Serves normally up to `rps`, then starts failing — a stand-in for a service
/// with a real capacity limit, so a breaking-point run has something to find.
async fn breaks_above_handler(Path(rps): Path<u64>) -> impl IntoResponse {
    use std::sync::atomic::{AtomicU64, Ordering};
    static WINDOW: AtomicU64 = AtomicU64::new(0);
    static COUNT: AtomicU64 = AtomicU64::new(0);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if WINDOW.swap(now, Ordering::Relaxed) != now {
        COUNT.store(0, Ordering::Relaxed);
    }
    let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;

    if n > rps {
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "over capacity" })),
        )
    } else {
        (axum::http::StatusCode::OK, Json(json!({ "ok": true })))
    }
}

/// Forget the credential already seen, so each test starts clean.
async fn expiring_auth_reset(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    *state.expiring_auth_seen.lock().unwrap() = None;
    Json(json!({ "reset": true }))
}

/// Rejects the first token it is shown, then accepts anything different.
///
/// Models a credential that expired between one request and the next, which is
/// what the command-token refresh exists for.
async fn expiring_auth_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let mut guard = state.expiring_auth_seen.lock().unwrap();
    match guard.as_ref() {
        // The first credential ever presented is the "expired" one.
        None => {
            *guard = Some(presented);
            (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "token expired" })),
            )
        }
        Some(expired) if *expired == presented => (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "token expired" })),
        ),
        Some(_) => (
            axum::http::StatusCode::OK,
            Json(json!({ "ok": true, "authorization": presented })),
        ),
    }
}

async fn delay_handler(Path(ms): Path<u64>) -> impl IntoResponse {
    tokio::time::sleep(std::time::Duration::from_millis(ms.min(60_000))).await;
    Json(json!({ "delayedMs": ms }))
}

async fn image_handler() -> impl IntoResponse {
    // A 1x1 transparent PNG.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    Response::builder()
        .header("content-type", "image/png")
        .body(axum::body::Body::from(PNG))
        .unwrap()
}

async fn large_handler(Path(kb): Path<usize>) -> impl IntoResponse {
    let n = kb.min(64 * 1024) * 1024;
    let data = vec![b'x'; n];
    Response::builder()
        .header("content-type", "text/plain")
        .body(axum::body::Body::from(data))
        .unwrap()
}

async fn count_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let n = state.counter.fetch_add(1, Ordering::Relaxed) + 1;
    Json(json!({ "count": n }))
}

/// Returns a token, so capture/auth flows can be exercised end to end.
async fn token_handler() -> impl IntoResponse {
    Json(json!({
        "token": "tok_abc123",
        "user": { "id": 42, "name": "test" },
        "expiresIn": 3600
    }))
}

async fn cookie_handler() -> impl IntoResponse {
    Response::builder()
        .header("set-cookie", "session=abc123; Path=/")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(r#"{"set":true}"#))
        .unwrap()
}
