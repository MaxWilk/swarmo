//! Load-engine integration tests. Every request goes to a local echo server
//! bound on an ephemeral port; nothing here touches the network.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use swarmo_core::model::*;
use swarmo_core::store::Protocol;
use swarmo_core::{EnvVariable, Environment, WorkspaceStore};
use swarmo_load::plan::{LoadPlan, PlanKind};
use swarmo_load::{run, RunControl};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

fn workspace(addr: SocketAddr) -> (tempfile::TempDir, WorkspaceStore) {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = WorkspaceStore::create(tmp.path().join("ws"), "test").unwrap();

    let mut env = Environment::new("test");
    env.variables.push(EnvVariable {
        key: "baseUrl".into(),
        value: format!("http://{addr}"),
        secret: false,
        enabled: true,
    });
    store.save_environment(&env).unwrap();
    store
        .set_active_environment(Some("test".to_string()))
        .unwrap();
    (tmp, store)
}

/// Create a request and return its ref.
fn request(store: &WorkspaceStore, coll: &str, name: &str, method: &str, url: &str) -> String {
    let r = store.create_request(coll, name).unwrap();
    let mut def = store.get_request(&r).unwrap();
    def.method = method.to_string();
    def.url = url.to_string();
    store.save_request(&r, &def).unwrap();
    r
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

// ---------------------------------------------------------------------------
// Parallel steps and fixed-iteration runs
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_steps_overlap_instead_of_queueing() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    let coll = store.create_collection("C").unwrap();

    // Three steps, each taking ~300ms. Run together an iteration is ~300ms;
    // run in sequence it is ~900ms — far enough apart that timing noise
    // cannot blur the verdict.
    let mk = |name: &str| request(&store, &coll, name, "GET", "{{baseUrl}}/delay/300");
    let (a, b, c) = (mk("A"), mk("B"), mk("C"));

    let sref = store.create_scenario("fanout").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.iterations = Some(4);
    s.concurrency = Some(1);
    s.stages = vec![];
    s.max_vus = 4;
    let step = |r: &str, par: bool| LoadStep {
        request_ref: r.to_string(),
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: None,
        parallel: par,
    };
    s.steps = vec![step(&a, false), step(&b, true), step(&c, true)];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let started = Instant::now();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;
    let took = started.elapsed();

    assert_eq!(out.summary.total_requests, 12, "4 iterations x 3 steps");
    assert_eq!(out.summary.total_errors, 0);
    // 4 iterations of a parallel group ≈ 1.2s; sequential would be ≈ 3.6s.
    assert!(
        took < Duration::from_millis(2400),
        "took {took:?} — the steps appear to have run in sequence"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fixed_iteration_run_sends_exactly_that_many() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Fast", "GET", "{{baseUrl}}/json");

    let sref = store.create_scenario("batch").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    // The batch-benchmark shape: N requests, C at a time, no duration at all.
    s.iterations = Some(500);
    s.concurrency = Some(32);
    s.max_vus = 64;
    s.stages = vec![];
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("batch".into()),
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    // Exactly the budget: a shared counter is claimed before each iteration,
    // so worker interleaving cannot over- or under-shoot.
    assert_eq!(out.summary.total_requests, 500);
    assert_eq!(out.summary.total_errors, 0);
    assert!(out.summary.duration_sec > 0.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_rounds_send_every_batch_and_wait_between_them() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Fast", "GET", "{{baseUrl}}/json");

    let sref = store.create_scenario("blasts").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![];
    s.max_vus = 16;
    // "200 requests, wait a second, again" — three times over.
    s.blasts = vec![swarmo_core::BlastItem::Repeat(swarmo_core::BlastRepeat {
        times: 3,
        blasts: vec![swarmo_core::Blast {
            iterations: 200,
            concurrency: 8,
            gap_sec: 1,
            relative: false,
        }],
        iteration_scale: None,
    })];
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("blast".into()),
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    assert_eq!(plan.rounds.len(), 3, "the block unrolls into three rounds");

    let started = Instant::now();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;
    let took = started.elapsed();

    // Every round's budget, exactly.
    assert_eq!(out.summary.total_requests, 600);
    assert_eq!(out.summary.total_errors, 0);

    // Two gaps, not three: the pause sits between rounds, so the run does not
    // end with an idle second padding its wall time.
    assert!(
        took >= Duration::from_millis(1900),
        "took {took:?} — the gaps between rounds were not waited out"
    );
    assert!(
        took < Duration::from_millis(2800),
        "took {took:?} — looks like a gap ran after the final round too"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_round_is_measured_on_its_own() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    let coll = store.create_collection("C").unwrap();
    // A deliberate 200ms per request, so a round's wall time is predictable
    // and the rounds are told apart by their size rather than by noise.
    let req = request(&store, &coll, "Slow", "GET", "{{baseUrl}}/delay/200");

    let sref = store.create_scenario("measured").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![];
    s.max_vus = 4;
    // Round 1: 4 requests, 4 at a time  -> one 200ms wave.
    // Round 2: 8 requests, 4 at a time  -> two waves, so about twice as long.
    s.blasts = vec![
        swarmo_core::Blast {
            iterations: 4,
            concurrency: 4,
            gap_sec: 1,
            relative: false,
        }
        .into(),
        swarmo_core::Blast {
            iterations: 8,
            concurrency: 4,
            gap_sec: 0,
            relative: false,
        }
        .into(),
    ];
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("m".into()),
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    let rounds = &out.summary.rounds;
    assert_eq!(rounds.len(), 2, "one entry per round");

    assert_eq!(rounds[0].index, 1);
    assert_eq!(rounds[1].index, 2);
    assert_eq!(rounds[0].planned, 4);
    assert_eq!(rounds[1].planned, 8);
    assert_eq!(rounds[0].gap_sec, 1);

    // Each round counts only its own requests, not the run's total.
    assert_eq!(rounds[0].stats.count, 4);
    assert_eq!(rounds[1].stats.count, 8);
    assert_eq!(out.summary.total_requests, 12);

    // The second round runs two waves, so it takes about twice as long — the
    // whole point of measuring rounds separately.
    assert!(
        rounds[1].wall_sec > rounds[0].wall_sec * 1.5,
        "round walls {:?} and {:?} should differ by about 2x",
        rounds[0].wall_sec,
        rounds[1].wall_sec
    );

    // A round's wall time is the round alone: the one-second pause after the
    // first round must not be inside its measurement.
    assert!(
        rounds[0].wall_sec < 0.9,
        "round 1 took {:.3}s — the gap after it looks to be included",
        rounds[0].wall_sec
    );

    // The rate is measured within the round, so it reflects the burst rather
    // than being dragged down by the pause that followed.
    assert!(
        rounds[0].rps > 5.0,
        "round 1 rps {:.1} looks gap-diluted",
        rounds[0].rps
    );

    // Latency is per round too, and each request really did take ~200ms.
    for r in rounds {
        assert!(
            r.stats.p50 >= 150.0,
            "round {} p50 {:.0}ms",
            r.index,
            r.stats.p50
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_round_may_have_its_own_size_and_concurrency() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Fast", "GET", "{{baseUrl}}/json");

    let sref = store.create_scenario("mixed").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![];
    s.max_vus = 4; // Deliberately low: a fixed run answers to concurrency only.
    s.blasts = vec![
        swarmo_core::Blast {
            iterations: 50,
            concurrency: 4,
            gap_sec: 0,
            relative: false,
        }
        .into(),
        swarmo_core::Blast {
            iterations: 150,
            concurrency: 16,
            gap_sec: 0,
            relative: false,
        }
        .into(),
    ];
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("mixed".into()),
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_eq!(out.summary.total_requests, 200, "50 + 150");
    assert_eq!(out.summary.total_errors, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_abort_on_fail_threshold_reports_the_run_as_failed() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Broken", "GET", "{{baseUrl}}/status/500");

    let sref = store.create_scenario("abort").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.mode = swarmo_core::LoadMode::Closed;
    s.stages = vec![swarmo_core::Stage {
        duration_sec: 20,
        target: 2.0,
        relative: false,
    }
    .into()];
    s.max_vus = 4;
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: None,
        parallel: false,
    }];
    // Every request fails, so this trips on the first snapshot.
    s.thresholds = vec![swarmo_core::Threshold {
        metric: "http_req_failed".into(),
        stat: "rate".into(),
        op: swarmo_core::ThresholdOp::Lt,
        value_ms: None,
        value: Some(0.5),
        abort_on_fail: true,
    }];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let started = Instant::now();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    // Ended early — well before the 20 s stage — and as a *failure*: an
    // abort pulled by the run's own threshold used to read as "Stopped",
    // indistinguishable from the user pressing the button.
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(
        out.summary.state,
        swarmo_core::RunState::Failed,
        "{:?}",
        out.summary.state
    );
    assert!(
        out.summary.stopped_because.is_none(),
        "an abort is not a stop condition"
    );
}

// ---------------------------------------------------------------------------
// WebSocket steps
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_websocket_step_measures_the_connect_and_each_answered_message() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    let coll = store.create_collection("C").unwrap();

    let ws = store.create_ws_request(&coll, "echo").unwrap();
    let mut def = store.get_ws_request(&ws).unwrap();
    def.url = format!("ws://{addr}/ws");
    def.messages = vec![
        swarmo_core::WsMessageDef {
            kind: swarmo_core::WsPayloadKind::Text,
            body: "ping".into(),
            wait: swarmo_core::WsWait::Reply,
            enabled: true,
        },
        swarmo_core::WsMessageDef {
            kind: swarmo_core::WsPayloadKind::Text,
            body: "pong".into(),
            wait: swarmo_core::WsWait::Reply,
            enabled: true,
        },
    ];
    store.save_ws_request(&ws, &def).unwrap();

    let sref = store.create_scenario("ws").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![];
    s.max_vus = 16;
    // 50 sessions, 8 at a time: exact counts make the sample arithmetic checkable.
    s.iterations = Some(50);
    s.concurrency = Some(8);
    s.steps = vec![LoadStep {
        request_ref: ws,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("echo".into()),
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_eq!(
        out.summary.total_errors, 0,
        "{:?}",
        out.summary.errors_by_message
    );
    // Per session: one connect sample and one per answered message.
    let connect = out
        .summary
        .per_tag
        .iter()
        .find(|t| t.tag == "echo connect")
        .expect("a connect tag");
    let messages = out
        .summary
        .per_tag
        .iter()
        .find(|t| t.tag == "echo")
        .expect("a message tag");
    assert_eq!(connect.count, 50);
    assert_eq!(messages.count, 100, "two answered messages per session");
    assert_eq!(out.summary.total_requests, 150);
    // Message latency is a real round trip, and the bytes are the echo's.
    assert!(messages.p95 > 0.0);
    assert_eq!(messages.bytes_out, 50 * 8);
    assert_eq!(messages.bytes_in, 50 * 8);
    // The status histogram reads in the socket's own terms.
    let names: Vec<String> = out
        .summary
        .status_codes
        .iter()
        .map(|c| c.describe())
        .collect();
    assert!(names.iter().any(|n| n == "WS connected"), "{names:?}");
    assert!(names.iter().any(|n| n == "WS message"), "{names:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_websocket_wait_that_goes_unanswered_is_an_error_not_a_hang() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    let coll = store.create_collection("C").unwrap();

    let ws = store.create_ws_request(&coll, "push").unwrap();
    let mut def = store.get_ws_request(&ws).unwrap();
    // The push feed sends one frame then closes; asking for three cannot be met.
    def.url = format!("ws://{addr}/ws/push/1");
    def.settings.timeout_ms = 500;
    def.messages = vec![swarmo_core::WsMessageDef {
        kind: swarmo_core::WsPayloadKind::Text,
        body: "go".into(),
        wait: swarmo_core::WsWait::Count { count: 3 },
        enabled: true,
    }];
    store.save_ws_request(&ws, &def).unwrap();

    let sref = store.create_scenario("ws-unmet").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![];
    s.max_vus = 4;
    s.iterations = Some(5);
    s.concurrency = Some(2);
    s.steps = vec![LoadStep {
        request_ref: ws,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("push".into()),
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let started = Instant::now();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
    let messages = out
        .summary
        .per_tag
        .iter()
        .find(|t| t.tag == "push")
        .expect("tag");
    assert_eq!(messages.count, 5);
    assert_eq!(messages.errors, 5, "every wait went unmet");
    assert!(
        out.summary
            .errors_by_message
            .iter()
            .any(|e| e.message.contains("No reply")),
        "{:?}",
        out.summary.errors_by_message
    );
}

// ---------------------------------------------------------------------------
// Stopping at a breaking point
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_climbing_ramp_stops_when_the_service_starts_failing() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    // The server serves 200/s and rejects everything above it.
    let req = request(
        &store,
        &coll,
        "Capped",
        "GET",
        "{{baseUrl}}/breaks-above/200",
    );

    let sref = store.create_scenario("climb").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.mode = LoadMode::Open;
    // Climb 150/s at a time, with a generous ceiling: the stop condition is
    // what should end this, not the stage list running out.
    s.stages = vec![StageItem::Repeat(RepeatBlock {
        times: 20,
        stages: vec![Stage {
            duration_sec: 2,
            target: 150.0,
            relative: true,
        }],
        duration_scale: None,
    })];
    s.max_vus = 400;
    s.stop_when = Some(StopCondition {
        metric: "errorRate".into(),
        above: Some(0.10),
        below: None,
        for_intervals: 2,
    });
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("capped".into()),
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let full_ramp = plan.total_duration_sec();
    assert_eq!(
        full_ramp, 40,
        "the ramp should be long enough to be cut short"
    );

    let started = Instant::now();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;
    let took = started.elapsed().as_secs();

    // It found the limit and said so.
    let reason = out
        .summary
        .stopped_because
        .as_deref()
        .expect("the run should have stopped on its condition");
    assert!(reason.contains("errorRate"), "{reason}");

    // And it stopped rather than running the whole ramp out.
    assert!(
        took < full_ramp,
        "ran the full {full_ramp}s instead of stopping early"
    );

    // Reaching the limit is the goal, so this is not a cancelled run.
    assert_ne!(out.summary.state, RunState::Stopped);
    assert!(out.summary.total_requests > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_healthy_run_is_not_stopped_early() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Fine", "GET", "{{baseUrl}}/json");

    let sref = store.create_scenario("gentle").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.mode = LoadMode::Closed;
    s.stages = vec![Stage {
        duration_sec: 3,
        target: 4.0,
        relative: false,
    }
    .into()];
    s.max_vus = 4;
    // A condition that a working service will never meet.
    s.stop_when = Some(StopCondition {
        metric: "errorRate".into(),
        above: Some(0.5),
        below: None,
        for_intervals: 2,
    });
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("fine".into()),
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert!(
        out.summary.stopped_because.is_none(),
        "a healthy run was cut short: {:?}",
        out.summary.stopped_because
    );
    assert_eq!(out.summary.total_errors, 0);
}

// ---------------------------------------------------------------------------
// Auth tokens that expire mid-run
// ---------------------------------------------------------------------------

/// A command whose output differs every time it runs, standing in for a
/// credential that has been reissued.
fn reissuing_command() -> String {
    if cfg!(windows) {
        "echo %TIME%".to_string()
    } else {
        "date +%s%N".to_string()
    }
}

/// A one-step scenario against `/expiring-auth`, whose token comes from a
/// command. The server rejects the first credential it is shown.
async fn expiring_auth_run(
    addr: std::net::SocketAddr,
    store: &WorkspaceStore,
) -> (String, swarmo_load::plan::LoadPlan) {
    let coll = store.create_collection("C").unwrap();
    let req = request(store, &coll, "Guarded", "GET", "{{baseUrl}}/expiring-auth");

    let mut def = store.get_request(&req).unwrap();
    def.auth = Auth::CommandToken {
        command: reissuing_command(),
        header_name: "Authorization".into(),
        prefix: "Bearer ".into(),
    };
    store.save_request(&req, &def).unwrap();

    let sref = store.create_scenario("guarded").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.mode = LoadMode::Closed;
    s.stages = vec![Stage {
        duration_sec: 2,
        target: 4.0,
        relative: false,
    }
    .into()];
    s.max_vus = 4;
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("guarded".into()),
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let _ = addr;
    let plan = LoadPlan::from_scenario(store, &sref).await.unwrap();
    (sref, plan)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_token_is_refreshed_once_mid_run() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    reqwest::get(format!("http://{addr}/expiring-auth/reset"))
        .await
        .unwrap();

    let (_sref, mut plan) = expiring_auth_run(addr, &store).await;

    // The run starts with a token the server will reject, exactly as it would
    // if the credential had expired between fetching it and using it.
    let cache = std::sync::Arc::new(swarmo_load::auth::TokenCache::new());
    let command = reissuing_command();
    let first = cache.get(&command).await.unwrap();
    plan.install_auth_token(&command, cache.clone(), first);

    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    // The refresh happened, and only once: every virtual user that hit a 401
    // shared the one replacement rather than each spawning a process.
    assert_eq!(
        out.summary.token_refreshes, 1,
        "expected exactly one refresh, got {}",
        out.summary.token_refreshes
    );

    // And the run recovered: after the retry everything succeeded.
    assert!(out.summary.total_requests > 4);
    assert_eq!(
        out.summary.total_errors, 0,
        "the run should have recovered: {:?}",
        out.summary.status_codes
    );
    assert_eq!(
        out.summary.status_codes,
        vec![StatusCount {
            protocol: Protocol::Http,
            code: 200,
            count: out.summary.total_requests,
        }],
        "a retried request must be recorded once, as its final outcome"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retry_does_not_inflate_the_request_count() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    reqwest::get(format!("http://{addr}/expiring-auth/reset"))
        .await
        .unwrap();

    let (_sref, mut plan) = expiring_auth_run(addr, &store).await;
    let cache = std::sync::Arc::new(swarmo_load::auth::TokenCache::new());
    let command = reissuing_command();
    let first = cache.get(&command).await.unwrap();
    plan.install_auth_token(&command, cache.clone(), first);

    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    // Every sample is one iteration's *final* outcome. The rejected attempt
    // must leave no trace at all: if it were recorded too, the total would
    // exceed the number of successful responses by exactly one.
    let successes: u64 = out
        .summary
        .status_codes
        .iter()
        .filter(|c| c.code == 200)
        .map(|c| c.count)
        .sum();
    assert_eq!(
        successes, out.summary.total_requests,
        "a retried attempt was counted as well as its retry: {:?}",
        out.summary.status_codes
    );
    assert_eq!(out.summary.per_tag[0].count, out.summary.total_requests);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_without_command_auth_records_no_refreshes() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Plain", "GET", "{{baseUrl}}/json");
    let sref = store.create_scenario("plain").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.mode = LoadMode::Closed;
    s.stages = vec![Stage {
        duration_sec: 1,
        target: 2.0,
        relative: false,
    }
    .into()];
    s.max_vus = 2;
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("plain".into()),
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    assert!(plan.auth_commands().is_empty());

    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;
    assert_eq!(out.summary.token_refreshes, 0);
    assert_eq!(out.summary.total_errors, 0);
}

// ---------------------------------------------------------------------------
// Failure classification
//
// `classify_error` matches on the text of client errors, so these tests drive
// real failures rather than synthetic strings. If a reqwest upgrade rewords
// them, the bucket silently becomes "(other)" — these fail loudly instead.
// ---------------------------------------------------------------------------

/// Run a one-step scenario against `url` and return the failure buckets.
async fn failure_buckets(url: &str, timeout_ms: u64) -> Vec<(String, u64)> {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Doomed", "GET", url);

    let sref = store.create_scenario("failing").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.mode = LoadMode::Closed;
    s.stages = vec![Stage {
        duration_sec: 1,
        target: 2.0,
        relative: false,
    }
    .into()];
    s.max_vus = 2;
    s.timeout_ms = timeout_ms;
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("doomed".into()),
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;
    out.summary
        .errors_by_message
        .into_iter()
        .map(|e| (e.message, e.count))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_connection_is_named_as_one() {
    // Bind then drop, so the port is almost certainly still free and nothing
    // is listening on it.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let buckets = failure_buckets(&format!("http://{dead}/"), 5_000).await;
    assert!(
        buckets.iter().any(|(m, _)| m == "Connection refused"),
        "expected a refused-connection bucket, got {buckets:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_timeout_is_named_as_one() {
    // The server takes 5 s; the client gives up after 200 ms.
    let buckets = failure_buckets("{{baseUrl}}/delay/5000", 200).await;
    assert!(
        buckets.iter().any(|(m, _)| m == "Timed out"),
        "expected a timeout bucket, got {buckets:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_name_that_cannot_resolve_is_named_as_one() {
    // .invalid is reserved by RFC 2606 and must never resolve.
    let buckets = failure_buckets("http://swarmo-does-not-exist.invalid/", 5_000).await;
    // Which bucket depends on the resolver: one that answers gets a DNS
    // error, one that hijacks the name gets a refused connection, and one
    // that simply hangs gets a timeout. All three are correctly classified —
    // what must not happen is landing in the unnamed fallback.
    assert!(
        buckets.iter().any(|(m, _)| matches!(
            m.as_str(),
            "DNS resolution failed" | "Connection refused" | "Timed out"
        )),
        "the failure was not classified at all: {buckets:?}"
    );
}

// ---------------------------------------------------------------------------
// Engine A: declarative scenarios
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closed_model_runs_and_reports_percentiles() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Get JSON", "GET", "{{baseUrl}}/json");

    let sref = store.create_scenario("smoke").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.mode = LoadMode::Closed;
    s.stages = vec![Stage {
        duration_sec: 2,
        target: 8.0,
        relative: false,
    }
    .into()];
    s.max_vus = 16;
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("json".into()),
        parallel: false,
    }];
    s.thresholds = vec![Threshold {
        metric: "http_req_failed".into(),
        stat: "rate".into(),
        op: ThresholdOp::Lt,
        value_ms: None,
        value: Some(0.01),
        abort_on_fail: false,
    }];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, mut rx) = control();
    let collector = tokio::spawn(async move {
        let mut snaps = Vec::new();
        while let Ok(s) = rx.recv().await {
            snaps.push(s);
        }
        snaps
    });

    let out = run(plan, ctrl).await;
    let snaps = collector.await.unwrap();

    assert!(
        out.summary.total_requests > 20,
        "expected real traffic, got {}",
        out.summary.total_requests
    );
    assert_eq!(out.summary.total_errors, 0);
    assert_eq!(out.summary.state, RunState::Passed);
    assert!(out.summary.overall.p95 > 0.0);
    assert_eq!(out.summary.per_tag.len(), 1);
    assert_eq!(out.summary.per_tag[0].tag, "json");
    assert_eq!(out.summary.samples_dropped, 0);

    // Live snapshots were emitted roughly once per second.
    assert!(!snaps.is_empty(), "no snapshots were broadcast");
    assert!(snaps.iter().any(|s| s.active_vus > 0), "VUs never ramped");
    assert_eq!(out.timeline.len(), snaps.len());

    // The status-code histogram is populated.
    assert_eq!(
        out.summary.status_codes,
        vec![StatusCount {
            protocol: Protocol::Http,
            code: 200,
            count: out.summary.total_requests,
        }]
    );
    // Response bytes are accounted for, not just counted requests.
    assert!(out.summary.bytes_in > 0, "no throughput recorded");
    assert!(out.summary.bytes_per_sec > 0.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn errors_are_counted_and_fail_the_threshold() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Boom", "GET", "{{baseUrl}}/status/500");

    let sref = store.create_scenario("errs").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![Stage {
        duration_sec: 1,
        target: 4.0,
        relative: false,
    }
    .into()];
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: None,
        parallel: false,
    }];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert!(out.summary.total_requests > 0);
    assert_eq!(out.summary.total_errors, out.summary.total_requests);
    assert!((out.summary.error_rate - 1.0).abs() < 1e-9);
    assert_eq!(out.summary.state, RunState::Failed);
    assert!(!out.summary.thresholds[0].passed);
    // The tag falls back to the request's name.
    assert_eq!(out.summary.per_tag[0].tag, "Boom");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn captures_feed_later_steps() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let login = request(&store, &coll, "Login", "POST", "{{baseUrl}}/token");

    // The second step echoes the captured token back in a header.
    let use_ref = store.create_request(&coll, "Use Token").unwrap();
    let mut def = store.get_request(&use_ref).unwrap();
    def.method = "GET".into();
    def.url = "{{baseUrl}}/echo".into();
    def.headers = vec![KeyValue::new("X-Token", "{{token}}")];
    store.save_request(&use_ref, &def).unwrap();

    let sref = store.create_scenario("capture").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![Stage {
        duration_sec: 1,
        target: 2.0,
        relative: false,
    }
    .into()];
    s.steps = vec![
        LoadStep {
            request_ref: login,
            request_id: None,
            think_time_ms: None,
            capture: vec![Capture {
                from: CaptureSource::Body,
                json_path: Some("$.token".into()),
                name: None,
                as_var: "token".into(),
            }],
            tag: Some("login".into()),
            parallel: false,
        },
        LoadStep {
            request_ref: use_ref.clone(),
            request_id: None,
            think_time_ms: None,
            capture: vec![],
            tag: Some("use".into()),
            parallel: false,
        },
    ];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_eq!(out.summary.total_errors, 0);
    assert_eq!(out.summary.per_tag.len(), 2);
    // Both steps ran the same number of times (within one in-flight iteration).
    let login_count = out
        .summary
        .per_tag
        .iter()
        .find(|t| t.tag == "login")
        .unwrap()
        .count;
    let use_count = out
        .summary
        .per_tag
        .iter()
        .find(|t| t.tag == "use")
        .unwrap()
        .count;
    assert!(
        login_count.abs_diff(use_count) <= 2,
        "login={login_count} use={use_count}"
    );

    // Verify the token actually arrived by sending the same request once more.
    let merged = store.merged_request(&use_ref).unwrap();
    let mut scope = store.var_scope(Some("test")).unwrap();
    scope.set("token", "tok_abc123");
    let resolved = swarmo_core::finalize(&merged, &scope);
    let pool = swarmo_http::ClientPool::new();
    let res = swarmo_http::execute(&pool, &resolved, &swarmo_http::ExecOpts::default())
        .await
        .unwrap();
    match res.body {
        swarmo_http::BodyPreview::Text { raw, .. } => {
            assert!(raw.contains("tok_abc123"), "token header missing: {raw}");
        }
        other => panic!("unexpected body: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_aborts_promptly() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Slow", "GET", "{{baseUrl}}/delay/50");

    let sref = store.create_scenario("long").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    // A zero-length first stage jumps straight to full load, so the run is
    // genuinely busy by the time we stop it.
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
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: None,
        parallel: false,
    }];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let cancel = ctrl.cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        cancel.cancel();
    });

    let t0 = Instant::now();
    let out = run(plan, ctrl).await;
    let elapsed = t0.elapsed();

    assert!(
        elapsed < Duration::from_secs(4),
        "stop took {elapsed:?}, expected under 4s"
    );
    assert_eq!(out.summary.state, RunState::Stopped);
    assert!(out.summary.total_requests > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_model_measures_from_scheduled_start() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    // One worker, a slow endpoint, and a rate far above what it can serve:
    // queueing must show up in the latency, not vanish.
    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Slow", "GET", "{{baseUrl}}/delay/100");

    let sref = store.create_scenario("open").unwrap();
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
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: Some("slow".into()),
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert!(out.summary.total_requests > 0);
    // The endpoint itself takes ~100ms; with a backlog, p95 must be far higher.
    assert!(
        out.summary.overall.p95 > 200.0,
        "p95 was {:.1}ms — queueing delay appears to have been dropped",
        out.summary.overall.p95
    );
    assert!(
        out.summary.dropped_iterations > 0 || out.summary.overall.max > 500.0,
        "expected visible saturation: dropped={} max={:.1}",
        out.summary.dropped_iterations,
        out.summary.overall.max
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_model_hits_its_target_rate_when_capacity_allows() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Fast", "GET", "{{baseUrl}}/json");

    let sref = store.create_scenario("rate").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.mode = LoadMode::Open;
    s.max_vus = 50;
    s.stages = vec![Stage {
        duration_sec: 3,
        target: 100.0,
        relative: false,
    }
    .into()];
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: None,
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    // A 0 -> 100/s ramp over 3s averages ~50/s, so expect ~150 requests.
    assert!(
        out.summary.total_requests >= 90 && out.summary.total_requests <= 220,
        "expected ~150 requests for the ramp, got {}",
        out.summary.total_requests
    );
    assert_eq!(out.summary.total_errors, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn think_time_reduces_throughput() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Fast", "GET", "{{baseUrl}}/json");

    let sref = store.create_scenario("think").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![Stage {
        duration_sec: 2,
        target: 2.0,
        relative: false,
    }
    .into()];
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: Some([200, 300]),
        capture: vec![],
        tag: None,
        parallel: false,
    }];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    // 2 VUs over ~2s with 200-300ms of think time: well under 30 requests.
    assert!(
        out.summary.total_requests > 0 && out.summary.total_requests < 30,
        "think time did not throttle: {} requests",
        out.summary.total_requests
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unreachable_target_reports_errors_rather_than_hanging() {
    let (_t, store) = {
        // Point at a port nothing is listening on.
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        workspace(addr)
    };

    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Dead", "GET", "{{baseUrl}}/x");

    let sref = store.create_scenario("dead").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.timeout_ms = 500;
    s.stages = vec![Stage {
        duration_sec: 1,
        target: 2.0,
        relative: false,
    }
    .into()];
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: None,
        parallel: false,
    }];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert!(out.summary.total_requests > 0);
    assert_eq!(out.summary.total_errors, out.summary.total_requests);
    // A request that never got a response is code zero *for HTTP* — which is
    // a different thing from gRPC's zero, and has to stay distinguishable.
    assert_eq!(
        out.summary.status_codes,
        vec![StatusCount {
            protocol: Protocol::Http,
            code: 0,
            count: out.summary.total_requests,
        }]
    );
    // And the failure is named, which a status of zero cannot do on its own.
    assert!(
        !out.summary.errors_by_message.is_empty(),
        "transport failures were not classified"
    );
    let total_classified: u64 = out.summary.errors_by_message.iter().map(|e| e.count).sum();
    assert_eq!(total_classified, out.summary.total_errors);
}

// ---------------------------------------------------------------------------
// Engine B: scripted virtual users
// ---------------------------------------------------------------------------

fn write_user_script(store: &WorkspaceStore, name: &str, source: &str) -> String {
    store.create_user_script(name, source).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scripted_users_run_with_the_configured_mix() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let src = r#"
        export const options = {
          environment: "test",
          mode: "closed",
          stages: [{ durationSec: 1, target: 8 }, { durationSec: 3, target: 8 }],
          maxVus: 8,
          userMix: [
            { exec: "shopper", weight: 3 },
            { exec: "admin", weight: 1 }
          ]
        };

        export async function shopper(ctx) {
          const res = await ctx.http.get("{{baseUrl}}/json", { tag: "shopper list" });
          ctx.check(res, { "list ok": r => r.status === 200 });
          ctx.vars.runs = (ctx.vars.runs || 0) + 1;
          ctx.check(res, { "vars persist": () => ctx.vars.runs === ctx.vu.iteration + 1 });
        }

        export async function admin(ctx) {
          const res = await ctx.http.post("{{baseUrl}}/echo", { json: { a: 1 }, tag: "admin write" });
          ctx.check(res, { "write ok": r => r.status === 200 });
        }
    "#;
    let sref = write_user_script(&store, "mix", src);

    let plan = LoadPlan::from_user_script(&store, &sref).await.unwrap();
    assert!(matches!(plan.kind, PlanKind::Scripted { .. }));

    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert!(
        out.summary.total_requests > 10,
        "expected traffic, got {}",
        out.summary.total_requests
    );
    assert_eq!(out.summary.total_errors, 0);

    let shopper = out
        .summary
        .per_tag
        .iter()
        .find(|t| t.tag == "shopper list")
        .expect("shopper tag missing")
        .count;
    let admin = out
        .summary
        .per_tag
        .iter()
        .find(|t| t.tag == "admin write")
        .expect("admin tag missing")
        .count;

    // 6 shopper VUs to 2 admin VUs; both do one request per iteration.
    let ratio = shopper as f64 / admin as f64;
    assert!(
        (1.8..=5.0).contains(&ratio),
        "expected roughly 3:1, got {shopper}:{admin} ({ratio:.2})"
    );

    // Checks made it into the summary, including the per-VU state check.
    let names: Vec<&str> = out.summary.checks.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"list ok"), "{names:?}");
    assert!(names.contains(&"vars persist"), "{names:?}");
    let persist = out
        .summary
        .checks
        .iter()
        .find(|c| c.name == "vars persist")
        .unwrap();
    assert_eq!(
        persist.fails, 0,
        "ctx.vars did not persist across iterations"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scripted_capture_and_reuse_of_a_token() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let src = r#"
        export const options = {
          environment: "test",
          stages: [{ durationSec: 2, target: 2 }],
          maxVus: 2,
          userMix: [{ exec: "flow", weight: 1 }]
        };

        export async function flow(ctx) {
          if (!ctx.vars.token) {
            const login = await ctx.http.post("{{baseUrl}}/token", { tag: "login" });
            ctx.vars.token = login.json().token;
          }
          const res = await ctx.http.get("{{baseUrl}}/echo", {
            headers: { Authorization: `Bearer ${ctx.vars.token}` },
            tag: "authed"
          });
          ctx.check(res, {
            "token echoed": r => r.json().headers.authorization === "Bearer tok_abc123"
          });
        }
    "#;
    let sref = write_user_script(&store, "token", src);
    let plan = LoadPlan::from_user_script(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_eq!(out.summary.total_errors, 0);
    let check = out
        .summary
        .checks
        .iter()
        .find(|c| c.name == "token echoed")
        .expect("check missing");
    assert!(check.passes > 0);
    assert_eq!(check.fails, 0);

    // Login happened once per VU, not once per iteration.
    let logins = out
        .summary
        .per_tag
        .iter()
        .find(|t| t.tag == "login")
        .unwrap()
        .count;
    let authed = out
        .summary
        .per_tag
        .iter()
        .find(|t| t.tag == "authed")
        .unwrap()
        .count;
    assert!(
        logins <= 2,
        "expected at most one login per VU, got {logins}"
    );
    assert!(authed > logins, "authed={authed} logins={logins}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scripted_broken_script_fails_the_run_clearly() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let sref = write_user_script(
        &store,
        "broken",
        "export const options = { stages: [{ durationSec: 1, target: 1 }] };\nthis is not javascript {{{",
    );

    // The failure surfaces at planning time, before any traffic is sent.
    let err = match LoadPlan::from_user_script(&store, &sref).await {
        Ok(_) => panic!("expected the broken script to be rejected"),
        Err(e) => e.to_string(),
    };
    assert!(err.to_lowercase().contains("error"), "got: {err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scripted_run_can_be_stopped() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let src = r#"
        export const options = {
          environment: "test",
          stages: [{ durationSec: 0, target: 4 }, { durationSec: 120, target: 4 }],
          maxVus: 4,
          userMix: [{ exec: "hit", weight: 1 }]
        };
        export async function hit(ctx) {
          await ctx.http.get("{{baseUrl}}/delay/50", { tag: "slow" });
        }
    "#;
    let sref = write_user_script(&store, "stoppable", src);
    let plan = LoadPlan::from_user_script(&store, &sref).await.unwrap();

    let (ctrl, _rx) = control();
    let cancel = ctrl.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(800)).await;
        cancel.cancel();
    });

    let t0 = Instant::now();
    let out = run(plan, ctrl).await;
    assert!(
        t0.elapsed() < Duration::from_secs(6),
        "stopping took {:?}",
        t0.elapsed()
    );
    assert_eq!(out.summary.state, RunState::Stopped);
    assert!(out.summary.total_requests > 0);
}

// ---------------------------------------------------------------------------
// Planning guardrails
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plan_rejects_empty_and_oversized_scenarios() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let sref = store.create_scenario("empty").unwrap();
    assert!(LoadPlan::from_scenario(&store, &sref).await.is_err());

    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "R", "GET", "{{baseUrl}}/json");
    let mut s = store.get_scenario(&sref).unwrap();
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: None,
        parallel: false,
    }];
    s.max_vus = 10;
    s.stages = vec![Stage {
        duration_sec: 1,
        target: 5_000.0,
        relative: false,
    }
    .into()];
    store.save_scenario(&sref, &s).unwrap();
    let err = LoadPlan::from_scenario(&store, &sref)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("maxVus"), "got: {err}");
}

#[tokio::test]
async fn target_hosts_listed_for_the_confirmation_dialog() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "R", "GET", "{{baseUrl}}/json");
    let sref = store.create_scenario("hosts").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.stages = vec![Stage {
        duration_sec: 1,
        target: 1.0,
        relative: false,
    }
    .into()];
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: None,
        parallel: false,
    }];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    assert_eq!(plan.target_hosts(), vec!["127.0.0.1".to_string()]);
}

#[tokio::test]
async fn base_vars_come_from_the_named_environment() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    let scope: HashMap<String, String> = store.var_scope(Some("test")).unwrap().flatten();
    assert_eq!(
        scope.get("baseUrl").map(String::as_str),
        Some(format!("http://{addr}").as_str())
    );
}

// ---------------------------------------------------------------------------
// Ending a run
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn think_time_after_the_last_step_does_not_outlast_the_run() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "Fast", "GET", "{{baseUrl}}/json");

    let sref = store.create_scenario("pacing").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.mode = LoadMode::Closed;
    s.stages = vec![Stage {
        duration_sec: 1,
        target: 2.0,
        relative: false,
    }
    .into()];
    s.max_vus = 2;
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: Some([4000, 4000]),
        capture: vec![],
        tag: None,
        parallel: false,
    }];
    s.thresholds = vec![];
    store.save_scenario(&sref, &s).unwrap();

    let plan = LoadPlan::from_scenario(&store, &sref).await.unwrap();
    let started = Instant::now();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    // The pause only paces a next iteration that will never start; waiting
    // it out held a 1s run open for 4s and diluted its rate.
    assert!(
        started.elapsed() < Duration::from_millis(3000),
        "took {:?}",
        started.elapsed()
    );
    assert!(out.summary.total_requests > 0);
    assert_eq!(out.summary.state, RunState::Passed);
}

#[tokio::test]
async fn plan_rejects_a_duration_with_nothing_to_drive_it() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);
    let coll = store.create_collection("C").unwrap();
    let req = request(&store, &coll, "R", "GET", "{{baseUrl}}/json");

    let sref = store.create_scenario("idle").unwrap();
    let mut s = store.get_scenario(&sref).unwrap();
    s.environment = Some("test".into());
    s.mode = LoadMode::Open;
    s.stages = vec![];
    s.duration_sec = Some(10);
    s.arrival_rate_per_sec = None;
    s.steps = vec![LoadStep {
        request_ref: req,
        request_id: None,
        think_time_ms: None,
        capture: vec![],
        tag: None,
        parallel: false,
    }];
    store.save_scenario(&sref, &s).unwrap();

    // This used to plan, send nothing for ten seconds, and report a pass.
    assert!(LoadPlan::from_scenario(&store, &sref).await.is_err());

    s.arrival_rate_per_sec = Some(5.0);
    store.save_scenario(&sref, &s).unwrap();
    assert!(LoadPlan::from_scenario(&store, &sref).await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scripted_run_ending_mid_first_iteration_is_not_an_error() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    // Every virtual user is still inside its first iteration when the run
    // ends, which interrupts the script. That used to count as every user
    // failing to start, and the whole run was reported as Errored.
    let src = r#"
        export const options = {
          environment: "test",
          stages: [{ durationSec: 1, target: 2 }],
          maxVus: 2,
          userMix: [{ exec: "slow", weight: 1 }]
        };
        export async function slow(ctx) {
          await ctx.http.get("{{baseUrl}}/json", { tag: "first" });
          ctx.sleep(30);
          await ctx.http.get("{{baseUrl}}/json", { tag: "never" });
        }
    "#;
    let sref = write_user_script(&store, "interrupted", src);
    let plan = LoadPlan::from_user_script(&store, &sref).await.unwrap();

    let started = Instant::now();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert!(started.elapsed() < Duration::from_secs(6));
    assert_ne!(
        out.summary.state,
        RunState::Errored,
        "{:?}",
        out.summary.error
    );
    assert!(out.summary.total_requests > 0);
    assert!(
        !out.summary.per_tag.iter().any(|t| t.tag == "never"),
        "a request was sent after the run ended"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scripted_run_against_a_dead_server_records_failures_rather_than_erroring() {
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let (_t, store) = workspace(dead);

    // Every first request fails, and the script sees each as a throw. Those
    // failures are recorded samples — the run has results, not a broken script.
    let src = r#"
        export const options = {
          environment: "test",
          stages: [{ durationSec: 1, target: 2 }],
          maxVus: 2,
          timeoutMs: 500,
          userMix: [{ exec: "hit", weight: 1 }]
        };
        export async function hit(ctx) {
          await ctx.http.get("{{baseUrl}}/x", { tag: "dead" });
        }
    "#;
    let sref = write_user_script(&store, "dead", src);
    let plan = LoadPlan::from_user_script(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_ne!(
        out.summary.state,
        RunState::Errored,
        "{:?}",
        out.summary.error
    );
    assert!(out.summary.total_requests > 0);
    assert_eq!(out.summary.total_errors, out.summary.total_requests);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_script_that_throws_in_every_user_still_errors_the_run() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    let src = r#"
        export const options = {
          environment: "test",
          stages: [{ durationSec: 1, target: 2 }],
          maxVus: 2,
          userMix: [{ exec: "broken", weight: 1 }]
        };
        export async function broken(ctx) {
          throw new Error("genuinely broken");
        }
    "#;
    let sref = write_user_script(&store, "throws", src);
    let plan = LoadPlan::from_user_script(&store, &sref).await.unwrap();
    let (ctrl, _rx) = control();
    let out = run(plan, ctrl).await;

    assert_eq!(out.summary.state, RunState::Errored);
    assert!(
        out.summary
            .error
            .as_deref()
            .is_some_and(|e| e.contains("genuinely broken")),
        "{:?}",
        out.summary.error
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scripted_open_model_shows_queueing_active_users_and_leftovers() {
    let (addr, _srv) = echo_server::spawn().await;
    let (_t, store) = workspace(addr);

    // One executor, a slow endpoint, and a rate it cannot keep up with.
    let src = r#"
        export const options = {
          environment: "test",
          mode: "open",
          stages: [{ durationSec: 0, target: 20 }, { durationSec: 3, target: 20 }],
          maxVus: 1,
          userMix: [{ exec: "hit", weight: 1 }]
        };
        export async function hit(ctx) {
          await ctx.http.get("{{baseUrl}}/delay/100", { tag: "slow" });
        }
    "#;
    let sref = write_user_script(&store, "open-backlog", src);
    let plan = LoadPlan::from_user_script(&store, &sref).await.unwrap();

    let (ctrl, mut rx) = control();
    let collector = tokio::spawn(async move {
        let mut snaps = Vec::new();
        while let Ok(s) = rx.recv().await {
            snaps.push(s);
        }
        snaps
    });
    let out = run(plan, ctrl).await;
    let snaps = collector.await.unwrap();

    assert!(out.summary.total_requests > 0);
    // Queueing delay is charged to the request, as in the native engine.
    assert!(
        out.summary.overall.p95 > 200.0,
        "p95 was {:.1}ms — queueing delay appears to have been dropped",
        out.summary.overall.p95
    );
    // Arrivals still queued when the run ended are counted, not lost.
    assert!(
        out.summary.dropped_iterations > 0,
        "the backlog left at the end went uncounted"
    );
    // And the busy executor shows up as an active user.
    assert!(
        snaps.iter().any(|s| s.active_vus > 0),
        "open-model runs reported no active users"
    );
}
