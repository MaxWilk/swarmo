//! gRPC execution tests against a local tonic server on an ephemeral port.

use std::net::SocketAddr;

use swarmo_core::model_grpc::{GrpcRequestDef, ProtoSource};
use swarmo_core::{finalize_grpc, merge_grpc_chain, KeyValue, VarScope};
use swarmo_grpc::call_with;
use swarmo_grpc::{call_unary, load_from_files, ChannelPool, DescriptorSource, GrpcError};

fn descriptors() -> DescriptorSource {
    load_from_files(
        &[grpc_test_server::proto_file()],
        &[grpc_test_server::proto_dir()],
    )
    .expect("the test proto should compile")
}

/// Build a resolved request for a method on the test service.
fn request(addr: SocketAddr, method: &str, message: &str) -> swarmo_core::ResolvedGrpcRequest {
    let mut def = GrpcRequestDef::new(method);
    def.address = format!("http://{addr}");
    def.service = "swarmo.testing.TestService".into();
    def.method = method.into();
    def.message = message.into();
    def.proto_source = ProtoSource::Files {
        files: vec![grpc_test_server::proto_file().display().to_string()],
        include_paths: vec![grpc_test_server::proto_dir().display().to_string()],
    };
    finalize_grpc(&merge_grpc_chain(&[], &def), &VarScope::new())
}

async fn call(
    addr: SocketAddr,
    method: &str,
    message: &str,
) -> Result<swarmo_grpc::GrpcResult, GrpcError> {
    let pool = ChannelPool::default();
    call_unary(&pool, &descriptors(), &request(addr, method, message), 0).await
}

fn json(res: &swarmo_grpc::GrpcResult) -> serde_json::Value {
    serde_json::from_str(&res.response_raw_json).expect("response should be JSON")
}

// ---------------------------------------------------------------------------
// Unary calls
// ---------------------------------------------------------------------------

#[tokio::test]
async fn echo_round_trips_scalars_nesting_and_well_known_types() {
    let (addr, _s) = grpc_test_server::spawn().await;

    let res = call(
        addr,
        "Echo",
        r#"{
            "message": "hello",
            "number": 42,
            "flag": true,
            "nested": { "label": "inner", "values": [1, 2, 3] },
            "at": "2024-03-01T12:30:00Z",
            "tags": { "env": "test" }
        }"#,
    )
    .await
    .expect("call should succeed");

    assert_eq!(res.code, 0);
    assert_eq!(res.code_name, "OK");
    assert!(res.status_message.is_empty());
    assert!(res.duration_ms > 0.0);

    let v = json(&res);
    assert_eq!(v["message"], "hello");
    assert_eq!(v["number"], 42);
    assert_eq!(v["flag"], true);
    assert_eq!(v["nested"]["label"], "inner");
    assert_eq!(v["nested"]["values"][2], 3);
    // Timestamps come back in their proto3 JSON form.
    assert_eq!(v["at"], "2024-03-01T12:30:00Z");
    assert_eq!(v["tags"]["env"], "test");
    // 64-bit integers are strings in proto3 JSON.
    assert_eq!(v["callCount"], "1");

    // The pretty form is what the UI shows; the raw form is what scripts see.
    assert!(res.response_json.contains('\n'));
    assert!(!res.response_raw_json.contains('\n'));
}

#[tokio::test]
async fn an_empty_message_is_treated_as_an_empty_object() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let res = call(addr, "Echo", "   ").await.unwrap();
    assert_eq!(res.code, 0);
    assert_eq!(json(&res)["message"], "");
}

#[tokio::test]
async fn metadata_is_sent_and_ascii_and_binary_both_work() {
    let (addr, _s) = grpc_test_server::spawn().await;

    let mut def = GrpcRequestDef::new("Echo");
    def.address = format!("http://{addr}");
    def.service = "swarmo.testing.TestService".into();
    def.method = "Echo".into();
    def.message = "{}".into();
    def.metadata = vec![
        KeyValue::new("Authorization", "Bearer tok"),
        KeyValue::new("X-Trace-Bin", "aGVsbG8="), // "hello"
    ];
    let resolved = finalize_grpc(&merge_grpc_chain(&[], &def), &VarScope::new());

    let pool = ChannelPool::default();
    let res = call_unary(&pool, &descriptors(), &resolved, 0)
        .await
        .unwrap();

    assert_eq!(res.code, 0);
    let v = json(&res);
    // The server reflects back the ascii metadata it saw.
    assert_eq!(v["metadata"]["authorization"], "Bearer tok");
}

#[tokio::test]
async fn a_non_ok_status_is_data_not_an_error() {
    let (addr, _s) = grpc_test_server::spawn().await;

    let res = call(addr, "Fail", r#"{"code": 5, "message": "no such order"}"#)
        .await
        .expect("a failing RPC must still return Ok(GrpcResult)");

    assert_eq!(res.code, 5);
    assert_eq!(res.code_name, "NOT_FOUND");
    assert_eq!(res.status_message, "no such order");
    assert!(!res.ok());
    assert!(res.response_raw_json.is_empty());
}

#[tokio::test]
async fn every_status_code_maps_to_its_name() {
    let (addr, _s) = grpc_test_server::spawn().await;
    for (code, name) in [
        (3u16, "INVALID_ARGUMENT"),
        (7, "PERMISSION_DENIED"),
        (13, "INTERNAL"),
        (14, "UNAVAILABLE"),
        (16, "UNAUTHENTICATED"),
    ] {
        let res = call(addr, "Fail", &format!(r#"{{"code": {code}}}"#))
            .await
            .unwrap();
        assert_eq!(res.code, code);
        assert_eq!(res.code_name, name);
    }
}

#[tokio::test]
async fn the_deadline_is_enforced() {
    let (addr, _s) = grpc_test_server::spawn().await;

    let mut def = GrpcRequestDef::new("Delay");
    def.address = format!("http://{addr}");
    def.service = "swarmo.testing.TestService".into();
    def.method = "Delay".into();
    def.message = r#"{"ms": "5000"}"#.into();
    def.settings.timeout_ms = 300;
    let resolved = finalize_grpc(&merge_grpc_chain(&[], &def), &VarScope::new());

    let started = std::time::Instant::now();
    let pool = ChannelPool::default();
    let outcome = call_unary(&pool, &descriptors(), &resolved, 0).await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "the deadline should have fired quickly, took {elapsed:?}"
    );
    // Either the client timeout fires or the server honours grpc-timeout and
    // returns DEADLINE_EXCEEDED. Both are correct; neither may hang.
    match outcome {
        Err(GrpcError::Timeout(ms)) => assert_eq!(ms, 300),
        Ok(res) => assert_eq!(res.code, 4, "expected DEADLINE_EXCEEDED, got {res:?}"),
        Err(other) => panic!("unexpected error: {other}"),
    }
}

#[tokio::test]
async fn streaming_methods_are_refused_before_any_call() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let err = call_unary_only(addr, "StreamNumbers", r#"{"count": 3}"#)
        .await
        .unwrap_err();
    assert!(matches!(err, GrpcError::Streaming { .. }));
    assert!(err.to_string().contains("unary calls only"));
}

#[tokio::test]
async fn a_bad_request_message_names_the_problem() {
    let (addr, _s) = grpc_test_server::spawn().await;

    let err = call(addr, "Echo", r#"{"number": "not a number"}"#)
        .await
        .unwrap_err();
    match &err {
        GrpcError::BadMessage { message_type, .. } => {
            assert_eq!(message_type, "swarmo.testing.EchoRequest")
        }
        other => panic!("expected BadMessage, got {other}"),
    }

    // An unknown field is a schema mismatch too.
    let err = call(addr, "Echo", r#"{"nope": 1}"#).await.unwrap_err();
    assert!(matches!(err, GrpcError::BadMessage { .. }), "{err}");

    // Malformed JSON.
    let err = call(addr, "Echo", "{ not json").await.unwrap_err();
    assert!(matches!(err, GrpcError::BadMessage { .. }), "{err}");
}

#[tokio::test]
async fn an_unknown_method_lists_what_exists() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let err = call(addr, "Nope", "{}").await.unwrap_err();
    assert!(err.to_string().contains("Echo"), "{err}");
    assert!(err.is_schema_stale());
}

/// The old contract, still available to callers that need exactly one reply.
async fn call_unary_only(
    addr: SocketAddr,
    method: &str,
    message: &str,
) -> Result<swarmo_grpc::GrpcResult, GrpcError> {
    let pool = ChannelPool::default();
    let d = descriptors();
    let m = d.unary_method("swarmo.testing.TestService", method)?;
    let _ = m;
    call_unary(&pool, &d, &request(addr, method, message), 0).await
}

#[tokio::test]
async fn a_server_stream_is_collected_into_one_result() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let res = call(addr, "StreamNumbers", r#"{"count": 5}"#)
        .await
        .unwrap();
    assert_eq!(res.code, 0, "{res:?}");
    assert_eq!(res.kind, "server_streaming");
    assert_eq!(res.message_count, 5);
    assert_eq!(res.messages.len(), 5);
    // The list is one JSON array, so scripts and captures can index it.
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&res.response_raw_json).unwrap();
    assert_eq!(parsed[4]["value"], 4);
    assert!(res.first_message_ms.is_some());
    assert!(res.first_message_ms.unwrap() <= res.duration_ms);
    // A stream's status travels in trailers, which are reported as such.
    assert!(
        res.trailers
            .iter()
            .any(|(k, v)| k == "grpc-status" && v == "0"),
        "{:?}",
        res.trailers
    );
}

/// A service whose server stream sends one message and then fails with a
/// status carrying metadata, as servers do to attach error details.
mod failing_stream {
    use grpc_test_server::pb::test_service_server::{TestService, TestServiceServer};
    use grpc_test_server::pb::*;
    use tonic::{Request, Response, Status};

    pub struct Svc;

    fn failure() -> Status {
        let mut md = tonic::metadata::MetadataMap::new();
        md.insert("x-error-detail", "quota".parse().unwrap());
        Status::with_metadata(tonic::Code::Aborted, "stream broke", md)
    }

    type Stream<T> = tokio_stream::wrappers::ReceiverStream<Result<T, Status>>;

    #[tonic::async_trait]
    impl TestService for Svc {
        async fn echo(&self, _: Request<EchoRequest>) -> Result<Response<EchoReply>, Status> {
            Err(failure())
        }
        async fn delay(&self, _: Request<DelayRequest>) -> Result<Response<DelayReply>, Status> {
            Err(Status::unimplemented(""))
        }
        async fn fail(&self, _: Request<FailRequest>) -> Result<Response<FailReply>, Status> {
            Err(Status::unimplemented(""))
        }
        async fn login(&self, _: Request<LoginRequest>) -> Result<Response<LoginReply>, Status> {
            Err(Status::unimplemented(""))
        }
        type StreamNumbersStream = Stream<StreamReply>;
        async fn stream_numbers(
            &self,
            _: Request<StreamRequest>,
        ) -> Result<Response<Self::StreamNumbersStream>, Status> {
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let _ = tx.send(Ok(StreamReply { value: 1 })).await;
                let _ = tx.send(Err(failure())).await;
            });
            Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
                rx,
            )))
        }
        async fn sum(
            &self,
            _: Request<tonic::Streaming<SumRequest>>,
        ) -> Result<Response<SumReply>, Status> {
            Err(Status::unimplemented(""))
        }
        type ChatStream = Stream<ChatMessage>;
        async fn chat(
            &self,
            _: Request<tonic::Streaming<ChatMessage>>,
        ) -> Result<Response<Self::ChatStream>, Status> {
            Err(Status::unimplemented(""))
        }
    }

    pub async fn spawn() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(TestServiceServer::new(Svc))
                .serve_with_incoming(incoming),
        );
        addr
    }
}

#[tokio::test]
async fn a_failed_stream_keeps_its_trailing_metadata() {
    let addr = failing_stream::spawn().await;
    let res = call(addr, "StreamNumbers", "{}").await.unwrap();
    assert_eq!(res.code_name, "ABORTED", "{res:?}");
    // What arrived before the failure is kept...
    assert_eq!(res.message_count, 1);
    // ...and so is the metadata the failing status carried.
    assert!(
        res.trailers
            .iter()
            .any(|(k, v)| k == "x-error-detail" && v == "quota"),
        "trailers: {:?}, headers: {:?}",
        res.trailers,
        res.headers
    );
}

#[tokio::test]
async fn a_server_stream_stops_at_the_configured_cap() {
    // An unbounded feed would otherwise run every call to its deadline.
    let (addr, _s) = grpc_test_server::spawn().await;
    let mut req = request(addr, "StreamNumbers", r#"{"count": 1000}"#);
    req.settings.stream_max_messages = Some(7);
    let pool = ChannelPool::default();
    let res = call_with(&pool, &descriptors(), &req, 0, None)
        .await
        .unwrap();
    assert_eq!(res.code, 0);
    assert_eq!(res.message_count, 7);
}

#[tokio::test]
async fn a_client_stream_sends_a_json_array_of_messages() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let res = call(
        addr,
        "Sum",
        r#"[{"value": 1}, {"value": 2}, {"value": 39}]"#,
    )
    .await
    .unwrap();
    assert_eq!(res.code, 0, "{res:?}");
    assert_eq!(res.kind, "client_streaming");
    let reply: serde_json::Value = serde_json::from_str(&res.response_raw_json).unwrap();
    assert_eq!(reply["total"], "42", "int64 is stringified per proto3 JSON");
    assert_eq!(reply["count"], 3);
    // Three messages went out; bytes reflect all of them.
    assert!(res.request_bytes >= 3);
}

#[tokio::test]
async fn a_single_object_stands_in_for_a_list_of_one() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let res = call(addr, "Sum", r#"{"value": 9}"#).await.unwrap();
    let reply: serde_json::Value = serde_json::from_str(&res.response_raw_json).unwrap();
    assert_eq!(reply["count"], 1);
}

#[tokio::test]
async fn a_bidirectional_stream_pairs_every_message_with_a_reply() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let res = call(addr, "Chat", r#"[{"text": "hi"}, {"text": "there"}]"#)
        .await
        .unwrap();
    assert_eq!(res.code, 0, "{res:?}");
    assert_eq!(res.kind, "bidi");
    assert_eq!(res.message_count, 2);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&res.response_raw_json).unwrap();
    assert_eq!(parsed[0]["text"], "HI");
    assert_eq!(parsed[1]["text"], "THERE");
}

#[tokio::test]
async fn a_malformed_message_list_is_refused_before_anything_is_sent() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let err = call(addr, "Sum", r#"[{"value": "not a number"}]"#)
        .await
        .unwrap_err();
    assert!(matches!(err, GrpcError::BadMessage { .. }), "{err}");
}

#[tokio::test]
async fn an_unreachable_server_reports_unavailable_rather_than_erroring() {
    // Bind then drop so nothing is listening.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = listener.local_addr().unwrap();
    drop(listener);

    let mut req = request(dead, "Echo", "{}");
    req.settings.timeout_ms = 1500;

    let pool = ChannelPool::default();
    let outcome = call_unary(&pool, &descriptors(), &req, 0).await;

    match outcome {
        // Transport failure surfaces as a gRPC status so it lands in the
        // status histogram during load tests — never as an Err.
        //
        // Which status depends on when the refusal is seen. Channels connect
        // lazily and reconnect with backoff, so a host with nothing listening
        // usually runs out the deadline rather than failing fast; that is
        // DEADLINE_EXCEEDED (4), the same code a slow method gets, because
        // the client genuinely cannot tell them apart. A refusal that does
        // surface before the deadline is UNAVAILABLE (14).
        Ok(res) => {
            assert!(
                res.code == 14 || res.code == 4,
                "expected UNAVAILABLE or DEADLINE_EXCEEDED, got {res:?}"
            );
            assert!(!res.ok());
            assert!(
                res.status_message.contains(&dead.to_string())
                    || res.status_message.contains("deadline"),
                "{res:?}"
            );
        }
        Err(other) => panic!("unexpected error: {other}"),
    }
}

#[tokio::test]
async fn missing_service_or_method_is_caught_before_connecting() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let mut req = request(addr, "Echo", "{}");
    req.service = String::new();
    let pool = ChannelPool::default();
    let err = call_unary(&pool, &descriptors(), &req, 0)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Pick a service"), "{err}");
}

#[tokio::test]
async fn separate_calls_really_reach_the_server() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let pool = ChannelPool::default();
    let d = descriptors();

    let mut counts = Vec::new();
    for _ in 0..3 {
        let res = call_unary(&pool, &d, &request(addr, "Echo", "{}"), 0)
            .await
            .unwrap();
        counts.push(json(&res)["callCount"].as_str().unwrap().to_string());
    }
    assert_eq!(counts, vec!["1", "2", "3"]);
}

// ---------------------------------------------------------------------------
// Reflection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reflection_finds_the_same_schema_as_the_proto_file() {
    let (addr, _s) = grpc_test_server::spawn().await;

    let via_reflection = swarmo_grpc::load_via_reflection(&format!("http://{addr}"), false, &[])
        .await
        .expect("reflection should work against the test server");

    let from_files = descriptors();
    assert_eq!(via_reflection.service_names(), from_files.service_names());

    let m = via_reflection
        .unary_method("swarmo.testing.TestService", "Echo")
        .expect("Echo should be reachable via reflection");
    assert_eq!(m.input().full_name(), "swarmo.testing.EchoRequest");

    // And a call driven purely by a reflected schema works.
    let pool = ChannelPool::default();
    let res = call_unary(
        &pool,
        &via_reflection,
        &request(addr, "Echo", r#"{"message": "via reflection"}"#),
        0,
    )
    .await
    .unwrap();
    assert_eq!(res.code, 0);
    assert_eq!(json(&res)["message"], "via reflection");
}

#[tokio::test]
async fn reflection_falls_back_to_v1alpha() {
    let (addr, _s) = grpc_test_server::spawn_with(grpc_test_server::Reflection::V1AlphaOnly).await;

    let source = swarmo_grpc::load_via_reflection(&format!("http://{addr}"), false, &[])
        .await
        .expect("the v1alpha fallback should be used");
    assert_eq!(
        source.service_names(),
        vec!["swarmo.testing.TestService".to_string()]
    );
}

#[tokio::test]
async fn reflection_sends_the_requests_metadata() {
    // A server that requires auth rejects reflection just like any other RPC,
    // so the schema fetch has to carry the request's own metadata.
    let (addr, _s) = grpc_test_server::spawn_requiring_auth("secret-token").await;
    let address = format!("http://{addr}");

    let err = swarmo_grpc::load_via_reflection(&address, false, &[])
        .await
        .expect_err("without auth the fetch must fail");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("authenticat") || msg.contains("credential") || msg.contains("permission"),
        "the failure should be recognisably an auth problem: {err}"
    );

    let source = swarmo_grpc::load_via_reflection(
        &address,
        false,
        &[("authorization".into(), "Bearer secret-token".into())],
    )
    .await
    .expect("with auth the schema should come back");

    assert_eq!(
        source.service_names(),
        vec!["swarmo.testing.TestService".to_string()]
    );
}

#[tokio::test]
async fn an_authenticated_schema_and_call_work_end_to_end() {
    let (addr, _s) = grpc_test_server::spawn_requiring_auth("secret-token").await;

    // Auth on the request produces the metadata, which reaches both the schema
    // fetch and the call itself.
    let mut def = GrpcRequestDef::new("Echo");
    def.address = format!("http://{addr}");
    def.service = "swarmo.testing.TestService".into();
    def.method = "Echo".into();
    def.message = r#"{"message":"authed"}"#.into();
    def.proto_source = ProtoSource::Reflection;
    def.auth = swarmo_core::Auth::Bearer {
        token: "secret-token".into(),
    };

    let resolved = finalize_grpc(&merge_grpc_chain(&[], &def), &VarScope::new());
    assert_eq!(
        resolved.metadata,
        vec![(
            "authorization".to_string(),
            "Bearer secret-token".to_string()
        )]
    );

    let descriptors = swarmo_grpc::load_descriptors(
        &resolved.proto_source,
        &resolved.address,
        false,
        std::path::Path::new("."),
        &resolved.metadata,
    )
    .await
    .expect("reflection should succeed with the request's auth");

    let pool = ChannelPool::default();
    let res = call_unary(&pool, &descriptors, &resolved, 0).await.unwrap();
    assert_eq!(res.code, 0, "{res:?}");
    let v: serde_json::Value = serde_json::from_str(&res.response_raw_json).unwrap();
    assert_eq!(v["message"], "authed");
}

#[tokio::test]
async fn a_server_without_reflection_says_what_to_do() {
    let (addr, _s) = grpc_test_server::spawn_with(grpc_test_server::Reflection::Disabled).await;

    let err = swarmo_grpc::load_via_reflection(&format!("http://{addr}"), false, &[])
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("reflection"), "{msg}");
    assert!(msg.contains(".proto"), "{msg}");
}

#[tokio::test]
async fn message_templates_come_from_a_reflected_schema_too() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let source = swarmo_grpc::load_via_reflection(&format!("http://{addr}"), false, &[])
        .await
        .unwrap();

    let text = source
        .message_template("swarmo.testing.TestService", "Login")
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["user"], "");
}

// ---------------------------------------------------------------------------
// Descriptor caching
// ---------------------------------------------------------------------------

#[test]
fn cache_keys_change_when_a_proto_is_edited() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.proto");
    std::fs::write(&p, "syntax = \"proto3\";").unwrap();

    let source = ProtoSource::Files {
        files: vec![p.display().to_string()],
        include_paths: vec![],
    };
    let before = swarmo_grpc::cache_key(&source, "", dir.path());

    // Modification time has second-or-better resolution; force a change.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&p, "syntax = \"proto3\"; // edited").unwrap();
    let after = swarmo_grpc::cache_key(&source, "", dir.path());

    assert_ne!(before, after, "editing a .proto must invalidate the cache");
}

#[test]
fn reflection_cache_keys_are_per_address() {
    let dir = tempfile::tempdir().unwrap();
    let a = swarmo_grpc::cache_key(&ProtoSource::Reflection, "http://a:1", dir.path());
    let b = swarmo_grpc::cache_key(&ProtoSource::Reflection, "http://b:1", dir.path());
    assert_ne!(a, b);
}
