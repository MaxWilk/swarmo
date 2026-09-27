//! Load-engine tests for gRPC steps and scripted gRPC virtual users.
//! Everything runs against a local tonic server on an ephemeral port.

use std::net::SocketAddr;
use std::time::Duration;

use swarmo_core::model::*;
use swarmo_core::model_grpc::ProtoSource;
use swarmo_core::store::Protocol;
use swarmo_core::{EnvVariable, Environment, GrpcRequestDef, WorkspaceStore};
use swarmo_load::plan::LoadPlan;
use swarmo_load::{run, RunControl};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// A workspace with the test proto copied in, so proto paths are
/// workspace-relative exactly as a real user's would be.
fn workspace(addr: SocketAddr) -> (tempfile::TempDir, WorkspaceStore) {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = WorkspaceStore::create(tmp.path().join("ws"), "grpc test").unwrap();

    let protos = store.root().join("protos");
    std::fs::create_dir_all(&protos).unwrap();
    std::fs::write(
        protos.join("testing.proto"),
        grpc_test_server::TESTING_PROTO,
    )
    .unwrap();

    let mut env = Environment::new("test");
    env.variables.push(EnvVariable {
        key: "grpcHost".into(),
        value: format!("http://{addr}"),
        secret: false,
        enabled: true,
    });
    env.variables.push(EnvVariable {
        key: "baseUrl".into(),
        value: format!("http://{addr}"),
        secret: false,
        enabled: true,
    });
    store.save_environment(&env).unwrap();
    store.set_active_environment(Some("test".into())).unwrap();
    (tmp, store)
}

fn grpc_request(
    store: &WorkspaceStore,
    coll: &str,
    name: &str,
    method: &str,
    message: &str,
) -> String {
    let r = store.create_grpc_request(coll, name).unwrap();
    let mut def: GrpcRequestDef = store.get_grpc_request(&r).unwrap();
    def.address = "{{grpcHost}}".into();
    def.service = "swarmo.testing.TestService".into();
    def.method = method.into();
    def.message = message.into();
    def.proto_source = ProtoSource::Files {
        files: vec!["protos/testing.proto".into()],
        include_paths: vec!["protos".into()],
    };
    store.save_grpc_request(&r, &def).unwrap();
    r
}

fn step(request_ref: &str, tag: &str) -> LoadStep {
    LoadStep {
        request_ref: request_ref.to_string(),
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some(tag.to_string()),
        parallel: false,
    }
}

fn scenario(store: &WorkspaceStore, name: &str, steps: Vec<LoadStep>) -> String {
    let sref = store.create_scenario(name).unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![
        Stage {
            duration_sec: 1,
            target: 4.0,
            relative: false,
        }
        .into(),
        Stage {
            duration_sec: 2,
            target: 4.0,
            relative: false,
        }
        .into(),
    ];
    s.max_vus = 8;
    s.steps = steps;
    s.thresholds = vec![Threshold {
        metric: "http_req_failed".into(),
        stat: "rate".into(),
        op: ThresholdOp::Lt,
        value_ms: None,
        value: Some(0.01),
        abort_on_fail: false,
    }];
    store.save_scenario(&sref, &s).unwrap();
    sref
}

fn control() -> (RunControl, broadcast::Receiver<Snapshot>) {
    let (tx, rx) = broadcast::channel(256);
    (
        RunControl {
            run_id: uuid::Uuid::new_v4().to_string(),
            cancel: CancellationToken::new(),
            snapshots: tx,
        },
        rx,
    )
}

fn tag<'a>(summary: &'a RunSummary, name: &str) -> &'a TagStats {
    summary
        .per_tag
        .iter()
        .find(|t| t.tag == name)
        .unwrap_or_else(|| panic!("missing tag {name} in {:?}", summary.per_tag))
}

// ---------------------------------------------------------------------------
// Engine A: declarative gRPC steps
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_grpc_scenario_runs_and_reports_percentiles() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let echo = grpc_request(&store, &coll, "Echo", "Echo", r#"{"message":"hi"}"#);
    let sref = scenario(&store, "grpc-smoke", vec![step(&echo, "echo")]);

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert!(
        out.summary.total_requests > 10,
        "expected real traffic, got {}",
        out.summary.total_requests
    );
    assert_eq!(out.summary.total_errors, 0);
    assert_eq!(out.summary.state, RunState::Passed);
    assert!(out.summary.overall.p95 > 0.0);
    assert_eq!(tag(&out.summary, "echo").errors, 0);
    assert_eq!(out.summary.samples_dropped, 0);

    // gRPC OK is status 0, tagged as gRPC so it cannot be read as an HTTP
    // request that never got a response.
    assert_eq!(
        out.summary.status_codes,
        vec![StatusCount {
            protocol: Protocol::Grpc,
            code: 0,
            count: out.summary.total_requests,
        }]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_status_counts_as_an_error_and_fails_the_threshold() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let fail = grpc_request(
        &store,
        &coll,
        "Fail",
        "Fail",
        r#"{"code": 5, "message": "nope"}"#,
    );
    let sref = scenario(&store, "grpc-fail", vec![step(&fail, "fail")]);

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert!(out.summary.total_requests > 0);
    assert_eq!(out.summary.total_errors, out.summary.total_requests);
    assert_eq!(out.summary.state, RunState::Failed);
    assert!(!out.summary.thresholds[0].passed);
    // NOT_FOUND is 5, and it is what the status histogram records.
    assert_eq!(
        out.summary.status_codes,
        vec![StatusCount {
            protocol: Protocol::Grpc,
            code: 5,
            count: out.summary.total_requests,
        }]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn captures_chain_from_one_grpc_call_into_the_next() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let login = grpc_request(&store, &coll, "Login", "Login", r#"{"user":"demo"}"#);

    // The second step echoes the captured token back as metadata.
    let echo = store.create_grpc_request(&coll, "Authed Echo").unwrap();
    let mut def = store.get_grpc_request(&echo).unwrap();
    def.address = "{{grpcHost}}".into();
    def.service = "swarmo.testing.TestService".into();
    def.method = "Echo".into();
    def.message = r#"{"message":"{{token}}"}"#.into();
    def.metadata = vec![KeyValue::new("authorization", "Bearer {{token}}")];
    def.proto_source = ProtoSource::Files {
        files: vec!["protos/testing.proto".into()],
        include_paths: vec!["protos".into()],
    };
    store.save_grpc_request(&echo, &def).unwrap();

    let sref = store.create_scenario("grpc-capture").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![Stage {
        duration_sec: 2,
        target: 3.0,
        relative: false,
    }
    .into()];
    s.max_vus = 4;
    s.steps = vec![
        LoadStep {
            request_ref: login,
            request_id: None,
            think_time_ms: None,
            // The capture runs over the decoded response JSON, exactly as it
            // does for HTTP.
            capture: vec![Capture {
                from: CaptureSource::Body,
                json_path: Some("$.token".into()),
                name: None,
                as_var: "token".into(),
            }],
            tag: Some("login".into()),
            parallel: false,
        },
        step(&echo, "authed echo"),
    ];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_eq!(out.summary.total_errors, 0, "{:?}", out.summary.per_tag);
    let login_count = tag(&out.summary, "login").count;
    let echo_count = tag(&out.summary, "authed echo").count;
    assert!(
        login_count.abs_diff(echo_count) <= 2,
        "login={login_count} echo={echo_count}"
    );

    // Prove the token really flowed by making the same call directly.
    let merged = store.merged_grpc_request(&echo).unwrap();
    let mut scope = store.var_scope(Some("test")).unwrap();
    scope.set("token", "grpc_tok_123");
    let resolved = swarmo_core::finalize_grpc(&merged, &scope);

    let descriptors = swarmo_grpc::load_descriptors(
        &resolved.proto_source,
        &resolved.address,
        true,
        store.root(),
        &resolved.metadata,
    )
    .await
    .unwrap();
    let pool = swarmo_grpc::ChannelPool::default();
    let res = swarmo_grpc::call_unary(&pool, &descriptors, &resolved, 0)
        .await
        .unwrap();

    assert_eq!(res.code, 0);
    let v: serde_json::Value = serde_json::from_str(&res.response_raw_json).unwrap();
    assert_eq!(v["message"], "grpc_tok_123");
    assert_eq!(v["metadata"]["authorization"], "Bearer grpc_tok_123");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_and_grpc_steps_mix_in_one_scenario() {
    let (grpc_addr, _g) = grpc_test_server::spawn().await;
    let (http_addr, _h) = echo_server::spawn().await;
    let (_t, store) = workspace(grpc_addr);

    // Point the HTTP variable at the HTTP server.
    let mut env = store.get_environment_with_secrets("test").unwrap();
    for v in env.variables.iter_mut() {
        if v.key == "baseUrl" {
            v.value = format!("http://{http_addr}");
        }
    }
    store.save_environment(&env).unwrap();

    let coll = store.create_collection("C").unwrap();

    let http = store.create_request(&coll, "Get JSON").unwrap();
    let mut hdef = store.get_request(&http).unwrap();
    hdef.method = "GET".into();
    hdef.url = "{{baseUrl}}/json".into();
    store.save_request(&http, &hdef).unwrap();

    let grpc = grpc_request(&store, &coll, "Echo", "Echo", r#"{"message":"mixed"}"#);

    let sref = scenario(
        &store,
        "mixed",
        vec![step(&http, "http json"), step(&grpc, "grpc echo")],
    );

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    // Both protocols show up in the confirmation dialog's host list.
    let hosts = plan.target_hosts();
    assert!(hosts.contains(&"127.0.0.1".to_string()), "{hosts:?}");

    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_eq!(out.summary.total_errors, 0);
    assert!(tag(&out.summary, "http json").count > 0);
    assert!(tag(&out.summary, "grpc echo").count > 0);
    // Status codes from both protocols coexist: 200 for HTTP, 0 for gRPC OK.
    // Zero is the case that matters — without the protocol it is ambiguous
    // between a successful gRPC call and an HTTP request that never landed.
    let codes = &out.summary.status_codes;
    assert!(
        codes
            .iter()
            .any(|c| c.protocol == Protocol::Http && c.code == 200),
        "{codes:?}"
    );
    assert!(
        codes
            .iter()
            .any(|c| c.protocol == Protocol::Grpc && c.code == 0),
        "{codes:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_open_model_measures_grpc_from_the_scheduled_start() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let slow = grpc_request(&store, &coll, "Delay", "Delay", r#"{"ms":"100"}"#);

    let sref = store.create_scenario("grpc-open").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.mode = LoadMode::Open;
    s.max_vus = 1;
    s.stages = vec![Stage {
        duration_sec: 3,
        target: 20.0,
        relative: false,
    }
    .into()];
    s.steps = vec![step(&slow, "slow")];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert!(out.summary.total_requests > 0);
    assert!(
        out.summary.overall.p95 > 200.0,
        "p95 was {:.1}ms — queueing delay appears to have been dropped",
        out.summary.overall.p95
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_grpc_run_can_be_stopped() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let slow = grpc_request(&store, &coll, "Delay", "Delay", r#"{"ms":"50"}"#);

    let sref = store.create_scenario("grpc-long").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![
        Stage {
            duration_sec: 0,
            target: 4.0,
            relative: false,
        }
        .into(),
        Stage {
            duration_sec: 120,
            target: 4.0,
            relative: false,
        }
        .into(),
    ];
    s.steps = vec![step(&slow, "slow")];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let cancel = ctrl.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        cancel.cancel();
    });

    let t0 = std::time::Instant::now();
    let out = run(plan, ctrl).await;
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "stopping took {:?}",
        t0.elapsed()
    );
    assert_eq!(out.summary.state, RunState::Stopped);
    assert!(out.summary.total_requests > 0);
}

// ---------------------------------------------------------------------------
// Planning guardrails
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_streaming_method_is_load_tested_one_sample_per_stream() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let stream = grpc_request(&store, &coll, "Stream", "StreamNumbers", r#"{"count": 3}"#);
    let sref = scenario(&store, "streaming", vec![step(&stream, "stream")]);

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    // Each iteration opens one stream and drains its three messages; the
    // sample is the stream, not the message, so requests == streams.
    assert!(
        out.summary.total_requests > 10,
        "{}",
        out.summary.total_requests
    );
    assert_eq!(out.summary.total_errors, 0);
    assert_eq!(tag(&out.summary, "stream").errors, 0);
    // Three replies' worth of bytes per stream came back.
    assert!(out.summary.bytes_in > 0);
}

#[tokio::test]
async fn a_missing_proto_file_is_caught_at_plan_time() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let r = grpc_request(&store, &coll, "Echo", "Echo", "{}");
    let mut def = store.get_grpc_request(&r).unwrap();
    def.proto_source = ProtoSource::Files {
        files: vec!["protos/missing.proto".into()],
        include_paths: vec!["protos".into()],
    };
    store.save_grpc_request(&r, &def).unwrap();

    let sref = scenario(&store, "missing-proto", vec![step(&r, "echo")]);
    let err = LoadPlan::from_scenario(&store, &sref)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing.proto"), "{err}");
}

#[tokio::test]
async fn an_unknown_method_is_caught_at_plan_time() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let r = grpc_request(&store, &coll, "Nope", "NoSuchMethod", "{}");
    let sref = scenario(&store, "bad-method", vec![step(&r, "nope")]);

    let err = LoadPlan::from_scenario(&store, &sref)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("Echo"), "should list what exists: {err}");
}

#[tokio::test]
async fn a_scenario_can_use_reflection_instead_of_proto_files() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let r = grpc_request(&store, &coll, "Echo", "Echo", r#"{"message":"reflected"}"#);
    let mut def = store.get_grpc_request(&r).unwrap();
    def.proto_source = ProtoSource::Reflection;
    store.save_grpc_request(&r, &def).unwrap();

    let sref = scenario(&store, "reflected", vec![step(&r, "echo")]);
    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_eq!(out.summary.total_errors, 0);
    assert!(out.summary.total_requests > 0);
}

// ---------------------------------------------------------------------------
// Proto folders, auth and message reuse
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_proto_folder_is_enough_to_run_a_scenario() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let r = grpc_request(&store, &coll, "Echo", "Echo", r#"{"message":"folder"}"#);

    // Point at the folder rather than naming files and import roots.
    let mut def = store.get_grpc_request(&r).unwrap();
    def.proto_source = ProtoSource::Directory {
        root: "protos".into(),
        entry_files: vec![],
    };
    store.save_grpc_request(&r, &def).unwrap();

    let sref = scenario(&store, "folder-source", vec![step(&r, "echo")]);
    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_eq!(out.summary.total_errors, 0);
    assert!(out.summary.total_requests > 0);
}

#[tokio::test]
async fn an_empty_proto_folder_explains_which_folder_to_pick() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);
    std::fs::create_dir_all(store.root().join("empty")).unwrap();

    let coll = store.create_collection("C").unwrap();
    let r = grpc_request(&store, &coll, "Echo", "Echo", "{}");
    let mut def = store.get_grpc_request(&r).unwrap();
    def.proto_source = ProtoSource::Directory {
        root: "empty".into(),
        entry_files: vec![],
    };
    store.save_grpc_request(&r, &def).unwrap();

    let sref = scenario(&store, "empty-folder", vec![step(&r, "echo")]);
    let err = LoadPlan::from_scenario(&store, &sref)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("import paths"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auth_on_a_collection_reaches_every_grpc_call_inside_it() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    // One bearer token on the collection; the request inherits it.
    let coll = store.create_collection("Secured").unwrap();
    let mut container = store.get_container(&coll).unwrap();
    container.auth = Auth::Bearer {
        token: "{{grpcToken}}".into(),
    };
    store.save_container(&coll, &container).unwrap();

    let mut env = store.get_environment_with_secrets("test").unwrap();
    env.variables.push(EnvVariable {
        key: "grpcToken".into(),
        value: "inherited-token".into(),
        secret: false,
        enabled: true,
    });
    store.save_environment(&env).unwrap();

    let r = grpc_request(&store, &coll, "Echo", "Echo", r#"{"message":"secured"}"#);

    let sref = scenario(&store, "inherited-auth", vec![step(&r, "echo")]);
    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;
    assert_eq!(out.summary.total_errors, 0);

    // Prove the header actually went over the wire.
    let merged = store.merged_grpc_request(&r).unwrap();
    let resolved = swarmo_core::finalize_grpc(&merged, &store.var_scope(Some("test")).unwrap());
    assert_eq!(
        resolved.metadata,
        vec![(
            "authorization".to_string(),
            "Bearer inherited-token".to_string()
        )]
    );

    let descriptors = swarmo_grpc::load_descriptors(
        &resolved.proto_source,
        &resolved.address,
        true,
        store.root(),
        &resolved.metadata,
    )
    .await
    .unwrap();
    let pool = swarmo_grpc::ChannelPool::default();
    let res = swarmo_grpc::call_unary(&pool, &descriptors, &resolved, 0)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&res.response_raw_json).unwrap();
    assert_eq!(v["metadata"]["authorization"], "Bearer inherited-token");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_variable_free_message_is_parsed_once_but_a_templated_one_is_not() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();

    // No variables: prepared at plan time.
    let fixed = grpc_request(&store, &coll, "Fixed", "Echo", r#"{"message":"fixed"}"#);
    // With a variable: must be re-resolved per iteration.
    let templated = grpc_request(
        &store,
        &coll,
        "Templated",
        "Echo",
        r#"{"message":"{{grpcHost}}"}"#,
    );

    let sref = scenario(
        &store,
        "message-reuse",
        vec![step(&fixed, "fixed"), step(&templated, "templated")],
    );
    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();

    // Check the planner made the right call about each step.
    match &plan.kind {
        swarmo_load::PlanKind::Native { steps } => {
            let prepared: Vec<bool> = steps
                .iter()
                .map(|s| match &s.action {
                    swarmo_load::PreparedAction::Grpc {
                        prepared_message, ..
                    } => prepared_message.is_some(),
                    _ => panic!("expected gRPC steps"),
                })
                .collect();
            assert_eq!(
                prepared,
                vec![true, false],
                "a variable-free message should be prepared, a templated one should not"
            );
        }
        _ => panic!("expected a native plan"),
    }

    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;
    assert_eq!(out.summary.total_errors, 0);
    assert!(tag(&out.summary, "fixed").count > 0);
    assert!(tag(&out.summary, "templated").count > 0);
}

#[tokio::test]
async fn a_malformed_message_is_caught_at_plan_time_when_it_has_no_variables() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let r = grpc_request(&store, &coll, "Bad", "Echo", r#"{"number":"not a number"}"#);
    let sref = scenario(&store, "bad-message", vec![step(&r, "bad")]);

    let err = LoadPlan::from_scenario(&store, &sref)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("EchoRequest"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generators_are_evaluated_on_every_iteration() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    // Randomising the *status code* makes per-iteration evaluation directly
    // observable: if the message were resolved once, every response would carry
    // the same code and the histogram would have a single entry.
    let coll = store.create_collection("C").unwrap();
    let r = grpc_request(
        &store,
        &coll,
        "Random Fail",
        "Fail",
        r#"{"code": {{$pick(5,7,9)}}, "message": "{{$string(6)}}"}"#,
    );

    let sref = store.create_scenario("randomized").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![Stage {
        duration_sec: 2,
        target: 6.0,
        relative: false,
    }
    .into()];
    s.max_vus = 8;
    s.steps = vec![step(&r, "fail")];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();

    // A message containing generators must never be pre-parsed, or every
    // iteration would replay one frozen request.
    match &plan.kind {
        swarmo_load::PlanKind::Native { steps } => match &steps[0].action {
            swarmo_load::PreparedAction::Grpc {
                prepared_message, ..
            } => assert!(
                prepared_message.is_none(),
                "a randomised message must be resolved per iteration"
            ),
            _ => panic!("expected a gRPC step"),
        },
        _ => panic!("expected a native plan"),
    }

    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert!(out.summary.total_requests > 20);
    // The breakdown is ordered by frequency, which is random here, so compare
    // the set of codes rather than the order they happen to land in.
    let mut codes: Vec<u16> = out.summary.status_codes.iter().map(|c| c.code).collect();
    codes.sort_unstable();
    assert_eq!(
        codes,
        vec![5, 7, 9],
        "all three randomised codes should appear: {:?}",
        out.summary.status_codes
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_constant_arrival_rate_holds_without_any_stages() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let echo = grpc_request(&store, &coll, "Echo", "Echo", r#"{"message":"flat"}"#);

    let sref = store.create_scenario("flat").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.mode = LoadMode::Open;
    s.stages = vec![]; // no ramps at all
    s.arrival_rate_per_sec = Some(120.0);
    s.duration_sec = Some(3);
    s.max_vus = 64;
    s.steps = vec![step(&echo, "echo")];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    assert_eq!(plan.total_duration_sec(), 3);

    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    // 120/s held flat for 3s, with no ramp-up eating into the total.
    assert!(
        (300..=400).contains(&out.summary.total_requests),
        "expected roughly 360 requests at a flat 120/s, got {}",
        out.summary.total_requests
    );
    assert_eq!(out.summary.total_errors, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_constant_rate_run_lasts_its_duration_not_the_leftover_stages() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let echo = grpc_request(&store, &coll, "Echo", "Echo", r#"{"message":"x"}"#);

    // Exactly what the app produces: a scenario created from the default
    // template, which arrives with stages totalling 50s, then switched to a
    // constant rate with a short duration.
    let sref = store.create_scenario("leftover-stages").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    assert_eq!(
        s.flat_stages()
            .iter()
            .map(|st| st.duration_sec)
            .sum::<u64>(),
        50,
        "the default template's stages"
    );
    s.environment = Some("test".into());
    s.mode = LoadMode::Open;
    s.arrival_rate_per_sec = Some(100.0);
    s.duration_sec = Some(3);
    s.max_vus = 64;
    s.steps = vec![step(&echo, "echo")];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    // This is the number the run-confirmation dialog shows.
    assert_eq!(
        plan.total_duration_sec(),
        3,
        "the preview must show the duration that was set"
    );

    let started = std::time::Instant::now();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(10),
        "the run should last ~3s, not the stages' 50s (took {elapsed:?})"
    );
    assert!(
        (200..=400).contains(&out.summary.total_requests),
        "expected roughly 300 requests at 100/s for 3s, got {}",
        out.summary.total_requests
    );
}

#[tokio::test]
async fn a_constant_rate_in_closed_mode_is_refused() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let echo = grpc_request(&store, &coll, "Echo", "Echo", "{}");
    let sref = scenario(&store, "wrong-mode", vec![step(&echo, "echo")]);
    let mut s = store.get_scenario(&sref).unwrap();
    s.mode = LoadMode::Closed;
    s.arrival_rate_per_sec = Some(50.0);
    store.save_scenario(&sref, &s).unwrap();

    let err = LoadPlan::from_scenario(&store, &sref)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("open mode"), "{err}");
}

// ---------------------------------------------------------------------------
// Engine B: scripted gRPC virtual users
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scripted_users_can_call_grpc_and_http_together() {
    let (grpc_addr, _g) = grpc_test_server::spawn().await;
    let (http_addr, _h) = echo_server::spawn().await;
    let (_t, store) = workspace(grpc_addr);

    let mut env = store.get_environment_with_secrets("test").unwrap();
    for v in env.variables.iter_mut() {
        if v.key == "baseUrl" {
            v.value = format!("http://{http_addr}");
        }
    }
    store.save_environment(&env).unwrap();

    let src = r#"
        export const options = {
          environment: "test",
          stages: [{ durationSec: 1, target: 4 }, { durationSec: 2, target: 4 }],
          maxVus: 4,
          userMix: [
            { exec: "caller", weight: 3 },
            { exec: "browser", weight: 1 }
          ],
          grpc: {
            protoFiles: ["protos/testing.proto"],
            includePaths: ["protos"]
          }
        };

        export async function caller(ctx) {
          // ctx.vars persists across iterations, so log in once per user.
          if (!ctx.vars.token) {
            const login = await ctx.grpc.call("{{grpcHost}}", "swarmo.testing.TestService/Login", {
              message: { user: "demo" },
              tag: "grpc login"
            });
            ctx.check(login, { "login ok": r => r.code === 0 });
            ctx.vars.token = login.json().token;
          }

          const res = await ctx.grpc.call("{{grpcHost}}", "swarmo.testing.TestService/Echo", {
            message: { message: ctx.vars.token },
            metadata: { authorization: `Bearer ${ctx.vars.token}` },
            tag: "grpc echo"
          });
          ctx.check(res, {
            "echo ok": r => r.code === 0,
            "token echoed": r => r.json().message === "grpc_tok_123",
            "metadata arrived": r => r.json().metadata.authorization === "Bearer grpc_tok_123"
          });
        }

        export async function browser(ctx) {
          const res = await ctx.http.get("{{baseUrl}}/json", { tag: "http json" });
          ctx.check(res, { "http ok": r => r.status === 200 });
        }
    "#;
    let sref = store.create_user_script("mixed", src).unwrap();

    let plan = LoadPlan::from_user_script(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_eq!(out.summary.total_errors, 0, "{:?}", out.summary.per_tag);
    assert!(out.summary.total_requests > 5);

    // Both protocols produced tags.
    assert!(tag(&out.summary, "grpc echo").count > 0);
    assert!(tag(&out.summary, "http json").count > 0);

    // Every check passed, including the one proving metadata arrived.
    for c in &out.summary.checks {
        assert_eq!(c.fails, 0, "check \"{}\" failed", c.name);
    }
    let names: Vec<&str> = out.summary.checks.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"token echoed"), "{names:?}");
    assert!(names.contains(&"metadata arrived"), "{names:?}");

    // Login ran once per calling virtual user, not once per iteration.
    let logins = tag(&out.summary, "grpc login").count;
    let echoes = tag(&out.summary, "grpc echo").count;
    assert!(
        logins <= 4,
        "expected at most one login per VU, got {logins}"
    );
    assert!(echoes > logins, "echoes={echoes} logins={logins}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_script_calling_grpc_without_options_grpc_says_so() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let src = r#"
        export const options = {
          environment: "test",
          stages: [{ durationSec: 1, target: 1 }],
          maxVus: 1,
          userMix: [{ exec: "run", weight: 1 }]
        };
        export async function run(ctx) {
          await ctx.grpc.call("{{grpcHost}}", "swarmo.testing.TestService/Echo", { message: {} });
        }
    "#;
    let sref = store.create_user_script("no-grpc-options", src).unwrap();

    // Planning succeeds (the script compiles); the calls fail at runtime with
    // an actionable message rather than silently doing nothing.
    let plan = LoadPlan::from_user_script(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_eq!(
        out.summary.total_requests, 0,
        "no traffic should have been sent"
    );
    let err = out.summary.error.unwrap_or_default();
    assert!(err.contains("options.grpc"), "{err}");
}

#[tokio::test]
async fn a_broken_options_grpc_block_fails_at_plan_time() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let src = r#"
        export const options = {
          stages: [{ durationSec: 1, target: 1 }],
          grpc: { protoFiles: ["protos/does-not-exist.proto"] }
        };
        export default async function (ctx) {}
    "#;
    let sref = store.create_user_script("bad-grpc-options", src).unwrap();

    let err = LoadPlan::from_user_script(&store, &sref)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("does-not-exist.proto"), "{err}");
}

#[tokio::test]
async fn reflection_needs_an_address_in_options_grpc() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let src = r#"
        export const options = {
          stages: [{ durationSec: 1, target: 1 }],
          grpc: { reflection: true }
        };
        export default async function (ctx) {}
    "#;
    let sref = store.create_user_script("reflect-no-addr", src).unwrap();

    let err = LoadPlan::from_user_script(&store, &sref)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("address"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scripted_grpc_can_use_reflection() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let src = r#"
        export const options = {
          environment: "test",
          stages: [{ durationSec: 2, target: 2 }],
          maxVus: 2,
          userMix: [{ exec: "run", weight: 1 }],
          grpc: { reflection: true, address: "{{grpcHost}}" }
        };
        export async function run(ctx) {
          const res = await ctx.grpc.call("{{grpcHost}}", "swarmo.testing.TestService/Echo", {
            message: { message: "reflected" },
            tag: "echo"
          });
          ctx.check(res, { "ok": r => r.code === 0 });
        }
    "#;
    let sref = store.create_user_script("reflected-script", src).unwrap();

    let plan = LoadPlan::from_user_script(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_eq!(out.summary.total_errors, 0);
    assert!(out.summary.total_requests > 0);
    assert_eq!(out.summary.checks.iter().map(|c| c.fails).sum::<u64>(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_grpc_status_is_visible_to_a_script() {
    let (addr, _s) = grpc_test_server::spawn().await;
    let (_t, store) = workspace(addr);

    let src = r#"
        export const options = {
          environment: "test",
          stages: [{ durationSec: 2, target: 2 }],
          maxVus: 2,
          userMix: [{ exec: "run", weight: 1 }],
          grpc: { protoFiles: ["protos/testing.proto"], includePaths: ["protos"] }
        };
        export async function run(ctx) {
          const res = await ctx.grpc.call("{{grpcHost}}", "swarmo.testing.TestService/Fail", {
            message: { code: 7, message: "denied" },
            tag: "fail"
          });
          ctx.check(res, {
            "is permission denied": r => r.code === 7,
            "names the code": r => r.codeName === "PERMISSION_DENIED",
            "carries the message": r => r.statusMessage === "denied",
            "not ok": r => r.ok === false
          });
        }
    "#;
    let sref = store.create_user_script("failing", src).unwrap();

    let plan = LoadPlan::from_user_script(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    // The calls happened and were counted as errors...
    assert!(out.summary.total_requests > 0);
    assert_eq!(out.summary.total_errors, out.summary.total_requests);
    // ...but the script saw the status as data, so every check passed.
    for c in &out.summary.checks {
        assert_eq!(c.fails, 0, "check \"{}\" failed", c.name);
    }
}
