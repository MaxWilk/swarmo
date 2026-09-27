//! The WebSocket executor against the echo server.

use std::net::SocketAddr;

use swarmo_core::model_ws::{
    finalize_ws, merge_ws_chain, WsMessageDef, WsPayloadKind, WsRequestDef, WsWait,
};
use swarmo_core::VarScope;
use swarmo_ws::{run, Direction, ResolvedWsRequest, WsError};
use tokio_util::sync::CancellationToken;

fn session(addr: SocketAddr, path: &str, messages: Vec<WsMessageDef>) -> ResolvedWsRequest {
    let mut def = WsRequestDef::new("s");
    def.url = format!("ws://{addr}{path}");
    def.messages = messages;
    finalize_ws(&merge_ws_chain(&[], &def), &VarScope::new())
}

fn text(body: &str, wait: WsWait) -> WsMessageDef {
    WsMessageDef {
        kind: WsPayloadKind::Text,
        body: body.into(),
        wait,
        enabled: true,
    }
}

#[tokio::test]
async fn a_reply_wait_measures_send_to_first_frame() {
    let (addr, _srv) = echo_server::spawn().await;
    let req = session(
        addr,
        "/ws",
        vec![text("hello", WsWait::Reply), text("again", WsWait::Reply)],
    );
    let out = run(&req, &CancellationToken::new(), true).await.unwrap();

    assert!(out.connected);
    assert!(out.ok(), "{out:?}");
    assert!(out.connect_ms.is_some());
    assert_eq!(out.messages_sent, 2);
    assert_eq!(out.messages_received, 2);
    assert_eq!(out.exchanges.len(), 2);
    for e in &out.exchanges {
        assert!(e.latency_ms.is_some(), "{e:?}");
        assert!(!e.timed_out);
        assert_eq!(e.frames_received, 1);
    }
    // Echoed bytes are counted both ways.
    assert_eq!(out.bytes_out, 10);
    assert_eq!(out.bytes_in, 10);
    // The transcript, in order: out, in, out, in.
    let dirs: Vec<Direction> = out.frames.iter().map(|f| f.direction).collect();
    assert_eq!(
        dirs,
        [Direction::Out, Direction::In, Direction::Out, Direction::In]
    );
    assert_eq!(out.frames[1].body, "hello");
    // A clean close.
    assert_eq!(out.close_code, Some(1000));
}

#[tokio::test]
async fn a_message_without_a_wait_has_no_latency_but_still_counts() {
    let (addr, _srv) = echo_server::spawn().await;
    let req = session(
        addr,
        "/ws",
        vec![text("fire", WsWait::None), text("forget", WsWait::None)],
    );
    let out = run(&req, &CancellationToken::new(), false).await.unwrap();
    assert!(out.ok());
    assert_eq!(out.messages_sent, 2);
    assert!(out.exchanges.is_empty(), "nothing waited, nothing to time");
    assert_eq!(out.bytes_out, 10);
    assert!(
        out.frames.is_empty(),
        "frames are not kept unless asked for"
    );
}

#[tokio::test]
async fn a_count_wait_collects_a_server_push() {
    let (addr, _srv) = echo_server::spawn().await;
    // The push endpoint ignores what we send and emits n messages on its own.
    let req = session(
        addr,
        "/ws/push/5",
        vec![text("go", WsWait::Count { count: 5 })],
    );
    let out = run(&req, &CancellationToken::new(), true).await.unwrap();
    assert!(out.ok(), "{out:?}");
    assert_eq!(out.exchanges[0].frames_received, 5);
    assert_eq!(out.messages_received, 5);
    let received: Vec<&str> = out
        .frames
        .iter()
        .filter(|f| f.direction == Direction::In)
        .map(|f| f.body.as_str())
        .collect();
    assert_eq!(received, ["0", "1", "2", "3", "4"]);
}

#[tokio::test]
async fn a_timed_window_ends_on_time_and_is_not_a_failure() {
    let (addr, _srv) = echo_server::spawn().await;
    // Echo answers once; the window keeps listening for 300ms regardless.
    let req = session(addr, "/ws", vec![text("x", WsWait::Millis { ms: 300 })]);
    let started = std::time::Instant::now();
    let out = run(&req, &CancellationToken::new(), true).await.unwrap();
    assert!(out.ok(), "{out:?}");
    assert!(started.elapsed() >= std::time::Duration::from_millis(280));
    assert_eq!(out.exchanges[0].frames_received, 1);
    assert!(!out.exchanges[0].timed_out, "a window simply ends");
}

#[tokio::test]
async fn a_reply_that_never_comes_fails_that_message_not_the_session() {
    let (addr, _srv) = echo_server::spawn().await;
    // The push endpoint sends nothing in response to a message after its
    // burst; with 0 messages it sends nothing at all and then closes... so
    // use a count larger than the push to make the wait outlive the feed.
    let mut req = session(
        addr,
        "/ws/push/1",
        vec![text("x", WsWait::Count { count: 3 })],
    );
    req.settings.timeout_ms = 400;
    let out = run(&req, &CancellationToken::new(), true).await.unwrap();
    assert!(out.connected);
    // The feed closed after one message; the wait for three could not be met.
    assert!(!out.ok(), "{out:?}");
    assert_eq!(out.exchanges.len(), 1);
    assert_eq!(out.exchanges[0].frames_received, 1);
}

#[tokio::test]
async fn an_unreachable_host_is_a_connect_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = listener.local_addr().unwrap();
    drop(listener);
    let mut req = session(dead, "/ws", vec![text("x", WsWait::Reply)]);
    req.settings.connect_timeout_ms = 2000;
    let err = run(&req, &CancellationToken::new(), false)
        .await
        .unwrap_err();
    // A refusal arrives as Connect; on some stacks a closed loopback port
    // instead swallows the SYN until the deadline. Both are connect failures.
    assert!(
        matches!(err, WsError::Connect { .. } | WsError::ConnectTimeout(_)),
        "{err}"
    );
}

#[tokio::test]
async fn a_non_socket_url_is_refused_before_connecting() {
    let mut req = session("127.0.0.1:1".parse().unwrap(), "/ws", vec![]);
    req.url = "ftp://nope".into();
    let err = run(&req, &CancellationToken::new(), false)
        .await
        .unwrap_err();
    assert!(matches!(err, WsError::Invalid(_)), "{err}");
}

#[tokio::test]
async fn cancellation_returns_what_was_collected() {
    let (addr, _srv) = echo_server::spawn().await;
    let mut req = session(
        addr,
        "/ws/push/1",
        vec![text("x", WsWait::Count { count: 100 })],
    );
    req.settings.timeout_ms = 30_000;
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        c2.cancel();
    });
    let started = std::time::Instant::now();
    let out = run(&req, &cancel, true).await.unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "cancel was not honoured"
    );
    assert!(out.connected);
    // Either the feed closed first (1 frame, then a close) or the cancel
    // landed mid-wait; both leave the session marked, never a silent success.
    assert!(!out.ok() || out.close_code.is_some(), "{out:?}");
}

// ---------------------------------------------------------------------------
// Scripted servers, for behaviour the echo server cannot provoke
// ---------------------------------------------------------------------------

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

/// Accept one WebSocket connection and hand it to `script`.
async fn scripted<F, Fut, T>(script: F) -> (SocketAddr, tokio::task::JoinHandle<T>)
where
    F: FnOnce(tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T> + Send,
    T: Send + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        script(ws).await
    });
    (addr, handle)
}

#[tokio::test]
async fn frames_left_over_from_an_earlier_message_are_not_its_successors_reply() {
    // "one" is answered with three frames at once; "two" with one, late.
    let (addr, _srv) = scripted(|mut ws| async move {
        while let Some(Ok(msg)) = ws.next().await {
            match msg.to_text().unwrap_or_default() {
                "one" => {
                    for r in ["r1", "r2", "r3"] {
                        ws.feed(Message::Text(r.into())).await.unwrap();
                    }
                    ws.flush().await.unwrap();
                }
                "two" => {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    ws.send(Message::Text("late".into())).await.unwrap();
                }
                _ => {}
            }
        }
    })
    .await;
    let req = session(
        addr,
        "/",
        vec![text("one", WsWait::Reply), text("two", WsWait::Reply)],
    );
    let out = run(&req, &CancellationToken::new(), true).await.unwrap();
    assert!(out.ok(), "{out:?}");

    // The second reply is the late one, and its latency says so.
    let second = &out.exchanges[1];
    assert_eq!(second.frames_received, 1);
    assert!(
        second.latency_ms.unwrap() >= 100.0,
        "a buffered frame was taken as the reply: {second:?}"
    );
    // The leftovers are not lost: they count and appear in the transcript.
    assert_eq!(out.messages_received, 4);
    let received: Vec<&str> = out
        .frames
        .iter()
        .filter(|f| f.direction == Direction::In)
        .map(|f| f.body.as_str())
        .collect();
    assert_eq!(received, ["r1", "r2", "r3", "late"]);
}

#[tokio::test]
async fn a_close_from_the_server_is_acknowledged_and_ends_the_script_as_a_failure() {
    let (addr, srv) = scripted(|mut ws| async move {
        let _first = ws.next().await;
        ws.send(Message::Close(None)).await.unwrap();
        // What the client answers with, if anything.
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await {
                Ok(Some(Ok(Message::Close(_)))) => return true,
                Ok(Some(Ok(_))) => continue,
                _ => return false,
            }
        }
    })
    .await;
    let req = session(
        addr,
        "/",
        vec![
            text("x", WsWait::Millis { ms: 300 }),
            text("y", WsWait::Reply),
        ],
    );
    let out = run(&req, &CancellationToken::new(), false).await.unwrap();

    assert!(srv.await.unwrap(), "the server's close was never answered");
    assert_eq!(out.messages_sent, 1);
    // The window itself was fine, but the script did not finish.
    assert!(!out.exchanges[0].timed_out);
    assert!(!out.ok(), "{out:?}");
    assert!(out.error.as_deref().unwrap().contains("1 of 2"), "{out:?}");
}

/// A server that completes the handshake and then never reads again, so the
/// client's socket buffers fill and its sends stop completing.
async fn stops_reading() -> SocketAddr {
    let (addr, _srv) = scripted(|ws| async move {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        drop(ws);
    })
    .await;
    addr
}

fn big_messages(n: usize) -> Vec<WsMessageDef> {
    let body = "x".repeat(16 * 1024 * 1024);
    (0..n).map(|_| text(&body, WsWait::None)).collect()
}

#[tokio::test]
async fn a_blocked_send_honours_cancellation() {
    let addr = stops_reading().await;
    let mut req = session(addr, "/", big_messages(4));
    req.settings.timeout_ms = 60_000;
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        c2.cancel();
    });
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run(&req, &cancel, false),
    )
    .await
    .expect("cancel was not honoured while sending")
    .unwrap();
    assert_eq!(out.error.as_deref(), Some("cancelled"));
}

#[tokio::test]
async fn a_blocked_send_times_out() {
    let addr = stops_reading().await;
    let mut req = session(addr, "/", big_messages(4));
    req.settings.timeout_ms = 400;
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run(&req, &CancellationToken::new(), false),
    )
    .await
    .expect("the send had no timeout")
    .unwrap();
    assert!(!out.ok());
    assert!(
        out.error.as_deref().unwrap().contains("timed out"),
        "{out:?}"
    );
}

#[tokio::test]
async fn bad_base64_is_refused_before_connecting() {
    // Nothing listens here: reaching the network would be a Connect error.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = listener.local_addr().unwrap();
    drop(listener);
    let mut req = session(dead, "/", vec![text("fine", WsWait::None)]);
    req.messages.push(swarmo_ws::ResolvedWsMessage {
        kind: WsPayloadKind::Binary,
        body: "not base64!".into(),
        wait: WsWait::None,
    });
    let err = run(&req, &CancellationToken::new(), false)
        .await
        .unwrap_err();
    assert!(matches!(err, WsError::Invalid(_)), "{err}");
}
