//! Request-execution integration tests against a local echo server.

use std::net::SocketAddr;

use swarmo_core::model::*;
use swarmo_core::{finalize, merge_chain, VarScope};
use swarmo_http::{BodyPreview, ClientPool, ExecOpts};

fn scope(addr: SocketAddr) -> VarScope {
    let mut s = VarScope::new();
    s.push_layer(
        [("baseUrl".to_string(), format!("http://{addr}"))]
            .into_iter()
            .collect(),
    );
    s
}

async fn send(addr: SocketAddr, def: RequestDef) -> swarmo_http::ExecResult {
    let merged = merge_chain(&[], &def);
    let resolved = finalize(&merged, &scope(addr));
    let pool = ClientPool::new();
    swarmo_http::execute(&pool, &resolved, &ExecOpts::default())
        .await
        .expect("request failed")
}

fn req(method: &str, url: &str) -> RequestDef {
    let mut d = RequestDef::new("test");
    d.method = method.into();
    d.url = url.into();
    d
}

fn echoed(res: &swarmo_http::ExecResult) -> serde_json::Value {
    match &res.body {
        BodyPreview::Text { raw, .. } => serde_json::from_str(raw).expect("echo body was not JSON"),
        other => panic!("expected a text body, got {other:?}"),
    }
}

#[tokio::test]
async fn get_with_query_params_and_headers() {
    let (addr, _s) = echo_server::spawn().await;
    let mut d = req("GET", "{{baseUrl}}/echo");
    d.params = vec![KeyValue::new("q", "hello world"), KeyValue::new("n", "2")];
    d.headers = vec![KeyValue::new("X-Custom", "abc")];

    let res = send(addr, d).await;
    assert_eq!(res.status, 200);
    let v = echoed(&res);
    assert_eq!(v["method"], "GET");
    assert_eq!(v["query"]["q"], "hello world");
    assert_eq!(v["headers"]["x-custom"], "abc");
    assert!(res.timings.total_ms > 0.0);
}

#[tokio::test]
async fn post_json_body_sets_content_type() {
    let (addr, _s) = echo_server::spawn().await;
    let mut d = req("POST", "{{baseUrl}}/echo");
    d.body = Body::Json {
        text: r#"{"sku":"A-1"}"#.into(),
    };

    let v = echoed(&send(addr, d).await);
    assert_eq!(v["method"], "POST");
    assert_eq!(v["headers"]["content-type"], "application/json");
    assert_eq!(v["json"]["sku"], "A-1");
}

#[tokio::test]
async fn form_body() {
    let (addr, _s) = echo_server::spawn().await;
    let mut d = req("POST", "{{baseUrl}}/echo");
    d.body = Body::Form {
        fields: vec![KeyValue::new("a", "1"), KeyValue::new("b", "two words")],
    };

    let v = echoed(&send(addr, d).await);
    assert_eq!(
        v["headers"]["content-type"],
        "application/x-www-form-urlencoded"
    );
    assert_eq!(v["body"], "a=1&b=two+words");
}

#[tokio::test]
async fn multipart_body() {
    let (addr, _s) = echo_server::spawn().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.txt");
    std::fs::write(&file, b"file contents here").unwrap();

    let mut d = req("POST", "{{baseUrl}}/echo");
    d.body = Body::Multipart {
        parts: vec![
            MultipartPart {
                key: "field".into(),
                kind: MultipartKind::Text,
                value: "value".into(),
                enabled: true,
                content_type: None,
            },
            MultipartPart {
                key: "upload".into(),
                kind: MultipartKind::File,
                value: file.to_string_lossy().to_string(),
                enabled: true,
                content_type: None,
            },
        ],
    };

    let v = echoed(&send(addr, d).await);
    let ct = v["headers"]["content-type"].as_str().unwrap();
    assert!(ct.starts_with("multipart/form-data"), "got {ct}");
    let body = v["body"].as_str().unwrap();
    assert!(body.contains("file contents here"));
    assert!(body.contains("name=\"upload\""));
    assert!(body.contains("filename=\"note.txt\""));
}

#[tokio::test]
async fn binary_body_from_a_file() {
    let (addr, _s) = echo_server::spawn().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("payload.json");
    std::fs::write(&file, br#"{"from":"file"}"#).unwrap();

    let mut d = req("POST", "{{baseUrl}}/echo");
    d.body = Body::Binary {
        path: file.to_string_lossy().to_string(),
    };

    let v = echoed(&send(addr, d).await);
    assert_eq!(v["json"]["from"], "file");
    assert_eq!(v["headers"]["content-type"], "application/json");
}

#[tokio::test]
async fn graphql_body_is_sent_as_json() {
    let (addr, _s) = echo_server::spawn().await;
    let mut d = req("POST", "{{baseUrl}}/echo");
    d.body = Body::Graphql {
        query: "query Me { me { id } }".into(),
        variables: r#"{"limit":5}"#.into(),
    };

    let v = echoed(&send(addr, d).await);
    assert_eq!(v["json"]["query"], "query Me { me { id } }");
    assert_eq!(v["json"]["variables"]["limit"], 5);
}

#[tokio::test]
async fn auth_headers_are_generated() {
    let (addr, _s) = echo_server::spawn().await;

    let mut d = req("GET", "{{baseUrl}}/echo");
    d.auth = Auth::Basic {
        username: "u".into(),
        password: "p".into(),
    };
    assert_eq!(
        echoed(&send(addr, d).await)["headers"]["authorization"],
        "Basic dTpw"
    );

    let mut d = req("GET", "{{baseUrl}}/echo");
    d.auth = Auth::Bearer {
        token: "tok".into(),
    };
    assert_eq!(
        echoed(&send(addr, d).await)["headers"]["authorization"],
        "Bearer tok"
    );

    let mut d = req("GET", "{{baseUrl}}/echo");
    d.auth = Auth::ApiKeyHeader {
        header_name: "X-Api-Key".into(),
        value: "k1".into(),
    };
    assert_eq!(echoed(&send(addr, d).await)["headers"]["x-api-key"], "k1");
}

#[tokio::test]
async fn json_response_is_pretty_printed_and_raw_is_kept() {
    let (addr, _s) = echo_server::spawn().await;
    let res = send(addr, req("GET", "{{baseUrl}}/json")).await;
    match res.body {
        BodyPreview::Text {
            text,
            raw,
            ref language,
        } => {
            assert_eq!(language, "json");
            assert!(text.contains('\n'), "pretty output should be multi-line");
            assert!(!raw.contains('\n'), "raw should be untouched");
        }
        other => panic!("expected text, got {other:?}"),
    }
}

#[tokio::test]
async fn image_responses_become_data_urls() {
    let (addr, _s) = echo_server::spawn().await;
    let res = send(addr, req("GET", "{{baseUrl}}/image")).await;
    match res.body {
        BodyPreview::Image { data_url } => {
            assert!(data_url.starts_with("data:image/png;base64,"))
        }
        other => panic!("expected an image, got {other:?}"),
    }
}

#[tokio::test]
async fn error_statuses_are_returned_not_thrown() {
    let (addr, _s) = echo_server::spawn().await;
    let res = send(addr, req("GET", "{{baseUrl}}/status/418")).await;
    assert_eq!(res.status, 418);
    assert_eq!(res.status_text.to_lowercase(), "i'm a teapot");
}

#[tokio::test]
async fn set_cookie_headers_are_captured() {
    let (addr, _s) = echo_server::spawn().await;
    let res = send(addr, req("GET", "{{baseUrl}}/cookie")).await;
    assert_eq!(res.set_cookies.len(), 1);
    assert!(res.set_cookies[0].raw.contains("session=abc123"));
}

#[tokio::test]
async fn cookies_are_sent_back_on_the_next_request() {
    let (addr, _s) = echo_server::spawn().await;
    let pool = ClientPool::new();
    let scope = scope(addr);

    let first = finalize(&merge_chain(&[], &req("GET", "{{baseUrl}}/cookie")), &scope);
    swarmo_http::execute(&pool, &first, &ExecOpts::default())
        .await
        .unwrap();

    let second = finalize(&merge_chain(&[], &req("GET", "{{baseUrl}}/echo")), &scope);
    let res = swarmo_http::execute(&pool, &second, &ExecOpts::default())
        .await
        .unwrap();

    let v = echoed(&res);
    assert_eq!(v["headers"]["cookie"], "session=abc123");
}

#[tokio::test]
async fn oversized_bodies_spill_to_a_file() {
    let (addr, _s) = echo_server::spawn().await;
    let dir = tempfile::tempdir().unwrap();

    // 21 MB, just over the 20 MB in-memory cap.
    let merged = merge_chain(&[], &req("GET", "{{baseUrl}}/large/21504"));
    let resolved = finalize(&merged, &scope(addr));
    let pool = ClientPool::new();
    let res = swarmo_http::execute(
        &pool,
        &resolved,
        &ExecOpts {
            proxy: None,
            temp_dir: dir.path().to_path_buf(),
        },
    )
    .await
    .unwrap();

    match res.body {
        BodyPreview::File { path, size } => {
            assert!(size > 20 * 1024 * 1024, "size was {size}");
            assert!(std::path::Path::new(&path).is_file());
        }
        other => panic!("expected the body to spill to a file, got {other:?}"),
    }
}

#[tokio::test]
async fn timeouts_surface_a_clear_message() {
    let (addr, _s) = echo_server::spawn().await;
    let mut d = req("GET", "{{baseUrl}}/delay/2000");
    d.settings.timeout_ms = 200;

    let merged = merge_chain(&[], &d);
    let resolved = finalize(&merged, &scope(addr));
    let pool = ClientPool::new();
    let err = swarmo_http::execute(&pool, &resolved, &ExecOpts::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("timed out"), "got: {err}");
}

#[tokio::test]
async fn an_unreachable_host_fails_fast_with_a_readable_message() {
    // Bind then drop so nothing is listening on the port. Depending on the
    // host's firewall this either refuses the connection or black-holes it;
    // what matters is that we surface an error promptly either way.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = listener.local_addr().unwrap();
    drop(listener);

    let mut d = req("GET", &format!("http://{dead}/nothing"));
    d.settings.timeout_ms = 1500;
    let merged = merge_chain(&[], &d);
    let resolved = finalize(&merged, &VarScope::new());
    let pool = ClientPool::new();

    let started = std::time::Instant::now();
    let err = swarmo_http::execute(&pool, &resolved, &ExecOpts::default())
        .await
        .unwrap_err();
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "took {elapsed:?}; the timeout should have bounded this"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("connect") || msg.contains("timed out"),
        "message was not actionable: {msg}"
    );
}

#[tokio::test]
async fn empty_url_is_rejected_before_sending() {
    let pool = ClientPool::new();
    let resolved = finalize(
        &merge_chain(&[], &RequestDef::new("blank")),
        &VarScope::new(),
    );
    let err = swarmo_http::execute(&pool, &resolved, &ExecOpts::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("URL is empty"), "got: {err}");
}

#[tokio::test]
async fn redirects_can_be_disabled() {
    let (addr, _s) = echo_server::spawn().await;
    // 302 with no Location still exercises the redirect policy switch.
    let mut d = req("GET", "{{baseUrl}}/status/302");
    d.settings.follow_redirects = false;
    let res = send(addr, d).await;
    assert_eq!(res.status, 302);
}

/// Every Content-Type the built request would send. The echo server collapses
/// repeated headers, so duplicates are checked on the request itself.
async fn content_types(d: RequestDef) -> Vec<String> {
    let resolved = finalize(&merge_chain(&[], &d), &VarScope::new());
    let client = reqwest::Client::new();
    let built = swarmo_http::build_request(&client, &resolved, "http://127.0.0.1:9/")
        .await
        .unwrap()
        .build()
        .unwrap();
    built
        .headers()
        .get_all(reqwest::header::CONTENT_TYPE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn multipart_replaces_a_content_type_set_in_the_headers() {
    let mut d = req("POST", "http://x/upload");
    d.headers = vec![KeyValue::new("Content-Type", "multipart/form-data")];
    d.body = Body::Multipart {
        parts: vec![MultipartPart {
            key: "field".into(),
            kind: MultipartKind::Text,
            value: "value".into(),
            enabled: true,
            content_type: None,
        }],
    };

    let cts = content_types(d).await;
    assert_eq!(cts.len(), 1, "got {cts:?}");
    assert!(cts[0].contains("boundary="), "got {cts:?}");
}

#[tokio::test]
async fn a_binary_body_keeps_an_explicit_content_type() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("payload.json");
    std::fs::write(&file, b"{}").unwrap();

    let mut d = req("POST", "http://x/upload");
    d.headers = vec![KeyValue::new("Content-Type", "application/octet-stream")];
    d.body = Body::Binary {
        path: file.to_string_lossy().to_string(),
    };

    assert_eq!(content_types(d).await, vec!["application/octet-stream"]);
}
