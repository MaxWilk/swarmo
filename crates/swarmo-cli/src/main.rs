//! `swarmo` — run Swarmo load tests headlessly (for CI and for scripting).
//!
//! Usage:
//!   swarmo run <workspace> <ref> [--json] [--no-save] [--yes]
//!                                  [--name <text>] [--notes <text>]
//!                                  [--report <file.html>]
//!   swarmo list <workspace>
//!
//! `<ref>` is workspace-relative, e.g. `loadtests/smoke.load.json` or
//! `loadtests/shopper.user.js`.
//!
//! Exit codes: 0 = every threshold passed, 1 = a threshold failed,
//! 2 = the run could not start.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use swarmo_core::model::{RunAnnotation, RunState, RunSummary, Snapshot};
use swarmo_core::WorkspaceStore;
use swarmo_load::plan::LoadPlan;
use swarmo_load::{run, RunControl};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

const USAGE: &str = "\
swarmo — run Swarmo load tests from the command line

USAGE:
    swarmo run <workspace-dir> <test-ref> [OPTIONS]
    swarmo list <workspace-dir>

ARGS:
    <workspace-dir>   A Swarmo workspace (the folder containing swarmo.json)
    <test-ref>        e.g. loadtests/smoke.load.json or loadtests/shopper.user.js

OPTIONS:
    --json       Print the run summary as JSON instead of a table
    --no-save    Do not write the run to .swarmo/runs/
    --name       Save the run under this name (e.g. a CI build number)
    --notes      Save a note alongside the run
    --report     Write a self-contained HTML report to this path
    --yes, -y    Skip the host confirmation prompt (required when not on a TTY)
    --quiet, -q  Do not print live progress
    -h, --help   Show this message

EXIT CODES:
    0  every threshold passed
    1  a threshold failed, or the run was stopped
    2  the run could not start
";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    match args[0].as_str() {
        "list" => cmd_list(&args[1..]),
        "run" => cmd_run(&args[1..]),
        other => {
            eprintln!("Unknown command \"{other}\".\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn cmd_list(args: &[String]) -> ExitCode {
    let Some(dir) = args.first() else {
        eprintln!("A workspace directory is required.\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let store = match WorkspaceStore::open(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Could not open the workspace: {e}");
            return ExitCode::from(2);
        }
    };
    match store.list_load_tests() {
        Ok(tests) if tests.is_empty() => println!("This workspace has no load tests."),
        Ok(tests) => {
            for t in tests {
                let kind = match t.kind {
                    swarmo_core::LoadTestKind::Scenario => "scenario",
                    swarmo_core::LoadTestKind::UserScript => "user script",
                    // The flat listing never yields one, but the match must
                    // still be total.
                    swarmo_core::LoadTestKind::Folder => continue,
                };
                println!("{:<12} {}", kind, t.node_ref);
            }
        }
        Err(e) => {
            eprintln!("Could not list load tests: {e}");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

/// Read `--name value` or `--name=value`.
fn value_of(args: &[String], name: &str) -> Option<String> {
    let eq = format!("{name}=");
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&eq) {
            return Some(v.to_string());
        }
        if a == name {
            return args.get(i + 1).cloned();
        }
    }
    None
}

/// Positional arguments, skipping any word consumed as a value flag.
fn positionals<'a>(args: &'a [String], value_flags: &[&str]) -> Vec<&'a String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if a.starts_with('-') {
            skip = value_flags.contains(&a.as_str());
            continue;
        }
        out.push(a);
    }
    out
}

fn cmd_run(args: &[String]) -> ExitCode {
    let positional = positionals(args, &["--name", "--notes", "--report"]);
    let flag = |name: &str| args.iter().any(|a| a == name);
    let report_path = value_of(args, "--report");
    let label = value_of(args, "--name");
    let notes = value_of(args, "--notes");

    if positional.len() < 2 {
        eprintln!("A workspace directory and a test ref are required.\n\n{USAGE}");
        return ExitCode::from(2);
    }
    let (dir, test_ref) = (positional[0], positional[1]);
    let as_json = flag("--json");
    let no_save = flag("--no-save");
    let quiet = flag("--quiet") || flag("-q") || as_json;
    let assume_yes = flag("--yes") || flag("-y");

    let store = match WorkspaceStore::open(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Could not open the workspace: {e}");
            return ExitCode::from(2);
        }
    };

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Could not start the async runtime: {e}");
            return ExitCode::from(2);
        }
    };

    // Planning compiles .proto files and may fetch a schema over reflection,
    // so it needs the runtime too.
    let plan = rt.block_on(async {
        if test_ref.ends_with(".user.js") {
            LoadPlan::from_user_script(&store, test_ref).await
        } else {
            LoadPlan::from_scenario(&store, test_ref).await
        }
    });
    let plan = match plan {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Could not prepare the run: {e}");
            return ExitCode::from(2);
        }
    };

    // The same safety rail the desktop app enforces.
    let hosts = plan.target_hosts();
    if !assume_yes && !confirm_hosts(&plan, &hosts) {
        eprintln!("Cancelled.");
        return ExitCode::from(2);
    }

    // Auth commands run on this machine, so they need the same explicit
    // consent the hosts do: --yes, or an interactive confirmation. Tokens are
    // fetched once here rather than per virtual user.
    let mut plan = plan;
    let auth_commands = plan.auth_commands();
    if !auth_commands.is_empty() {
        if !assume_yes && !confirm_commands(&auth_commands) {
            eprintln!("Cancelled.");
            return ExitCode::from(2);
        }
        // The same cache the desktop app uses, so a token that expires during
        // a long CI run is refreshed once rather than per virtual user.
        let cache = std::sync::Arc::new(swarmo_load::auth::TokenCache::new());
        for command in &auth_commands {
            match rt.block_on(cache.get(command)) {
                Ok(token) => plan.install_auth_token(command, cache.clone(), token),
                Err(e) => {
                    eprintln!("Could not get an auth token from \"{command}\": {e}");
                    return ExitCode::from(2);
                }
            }
        }
    }

    if !quiet && !auth_commands.is_empty() {
        eprintln!(
            "Auth tokens resolved from {} command(s).",
            auth_commands.len()
        );
    }

    if !quiet {
        eprintln!(
            "Running \"{}\" for {}s against {}",
            plan.name,
            plan.total_duration_sec(),
            if hosts.is_empty() {
                "an unresolved host".to_string()
            } else {
                hosts.join(", ")
            }
        );
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    let out = rt.block_on(async {
        let (tx, rx) = broadcast::channel::<Snapshot>(1024);
        let cancel = CancellationToken::new();

        // Ctrl-C stops the run cleanly and still prints a summary.
        {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    eprintln!("\nStopping…");
                    cancel.cancel();
                }
            });
        }

        let progress = tokio::spawn(print_progress(rx, quiet));

        let out = run(
            plan,
            RunControl {
                run_id: run_id.clone(),
                cancel,
                snapshots: tx,
            },
        )
        .await;

        let _ = progress.await;
        out
    });

    if let Some(path) = &report_path {
        let annotation = swarmo_core::model::RunAnnotation {
            label: label.clone(),
            notes: notes.clone(),
        }
        .normalized();
        // The extension picks the shape: .json for machines, anything else
        // gets the self-contained HTML page.
        let rendered = if path.ends_with(".json") {
            serde_json::to_string_pretty(&swarmo_core::report::json_report(
                &out.summary,
                &out.timeline,
                &annotation,
            ))
            .unwrap_or_default()
        } else {
            swarmo_core::report::html_report(&out.summary, &out.timeline, &annotation)
        };
        match std::fs::write(path, rendered) {
            Ok(()) => {
                if !quiet {
                    eprintln!("Report written to {path}");
                }
            }
            Err(e) => eprintln!("Could not write the report to {path}: {e}"),
        }
    }

    if !no_save {
        if label.is_some() || notes.is_some() {
            let run_id = out.summary.run_id.clone();
            if let Err(e) = store.save_run_annotation(&run_id, RunAnnotation { label, notes }) {
                eprintln!("Could not save the run's name and notes: {e}");
            }
        }
        if let Err(e) = store.save_run_summary(&out.summary) {
            eprintln!("Warning: could not save the run summary: {e}");
        }
        if let Err(e) = store.save_run_timeline(&run_id, &out.timeline) {
            eprintln!("Warning: could not save the run timeline: {e}");
        }
    }

    if as_json {
        match serde_json::to_string_pretty(&out.summary) {
            Ok(j) => println!("{j}"),
            Err(e) => eprintln!("Could not serialize the summary: {e}"),
        }
    } else {
        print_summary(&out.summary);
    }

    match out.summary.state {
        RunState::Passed => ExitCode::SUCCESS,
        RunState::Errored => ExitCode::from(2),
        _ => ExitCode::from(1),
    }
}

fn confirm_hosts(plan: &LoadPlan, hosts: &[String]) -> bool {
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "Refusing to send load without confirmation. Re-run with --yes once you have \
             checked that you own, or have permission to test, these hosts: {}",
            if hosts.is_empty() {
                "(none resolved)".to_string()
            } else {
                hosts.join(", ")
            }
        );
        return false;
    }

    eprintln!("About to send load to: {}", hosts.join(", "));
    eprintln!(
        "  {} for {}s, peak {} {}",
        plan.name,
        plan.total_duration_sec(),
        plan.peak_target().round(),
        match plan.mode {
            swarmo_core::model::LoadMode::Closed => "virtual users",
            swarmo_core::model::LoadMode::Open => "arrivals/sec",
        }
    );
    eprint!("Only test systems you own or have permission to test. Continue? [y/N] ");
    let _ = std::io::stderr().flush();

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

async fn print_progress(mut rx: broadcast::Receiver<Snapshot>, quiet: bool) {
    while let Ok(s) = rx.recv().await {
        if quiet {
            continue;
        }
        eprintln!(
            "  {:>4}s  vus {:>4}  {:>8.0} req/s  errors {:>5.1}%  p95 {:>8.1}ms",
            s.elapsed_sec,
            s.active_vus,
            s.rps,
            s.error_rate * 100.0,
            s.p95
        );
    }
}

fn print_summary(s: &RunSummary) {
    println!();
    println!("{}  ({})", s.scenario_name, state_word(s.state));
    if let Some(err) = &s.error {
        println!("  error: {err}");
    }
    println!(
        "  {} requests in {:.1}s — {:.0} req/s (peak {:.0}), {:.2}% errors",
        s.total_requests,
        s.duration_sec,
        s.rps,
        s.peak_rps,
        s.error_rate * 100.0
    );
    println!(
        "  sent {} ({}/s) · received {} ({}/s)",
        human_bytes(s.bytes_out),
        human_bytes(s.bytes_out_per_sec as u64),
        human_bytes(s.bytes_in),
        human_bytes(s.bytes_per_sec as u64),
    );
    println!(
        "  latency min {:.1}ms  p50 {:.1}ms  p95 {:.1}ms  p99.9 {:.1}ms  max {:.1}ms",
        s.overall.min, s.overall.p50, s.overall.p95, s.overall.p999, s.overall.max
    );

    if !s.per_tag.is_empty() {
        println!();
        println!(
            "  {:<30} {:>8} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9}",
            "STEP", "COUNT", "ERRORS", "MIN", "P50", "P95", "P99", "P99.9"
        );
        for t in &s.per_tag {
            println!(
                "  {:<30} {:>8} {:>7} {:>7.1}ms {:>7.1}ms {:>7.1}ms {:>7.1}ms {:>7.1}ms",
                truncate(&t.tag, 30),
                t.count,
                t.errors,
                t.min,
                t.p50,
                t.p95,
                t.p99,
                t.p999
            );
        }
    }

    if !s.status_codes.is_empty() {
        println!();
        println!("  {:<28} {:>8}  {:>6}", "STATUS", "COUNT", "SHARE");
        for sc in &s.status_codes {
            let share = if s.total_requests == 0 {
                0.0
            } else {
                sc.count as f64 / s.total_requests as f64 * 100.0
            };
            println!("  {:<28} {:>8}  {:>5.1}%", sc.describe(), sc.count, share);
        }
    }

    if let Some(reason) = &s.stopped_because {
        println!();
        println!("  {reason}");
    }

    if s.token_refreshes > 0 {
        println!();
        println!(
            "  note: the auth token was refreshed {} time(s) during the run.",
            s.token_refreshes
        );
    }

    if !s.errors_by_message.is_empty() {
        println!();
        println!("  {:<40} {:>8}", "ERROR", "COUNT");
        for e in &s.errors_by_message {
            println!("  {:<40} {:>8}", truncate(&e.message, 40), e.count);
        }
    }

    if !s.checks.is_empty() {
        println!();
        println!("  CHECKS");
        for c in &s.checks {
            let mark = if c.fails == 0 { "ok  " } else { "FAIL" };
            println!(
                "  {mark} {:<40} {} passed, {} failed",
                truncate(&c.name, 40),
                c.passes,
                c.fails
            );
        }
    }

    if !s.thresholds.is_empty() {
        println!();
        println!("  THRESHOLDS");
        for t in &s.thresholds {
            let mark = if t.passed { "ok  " } else { "FAIL" };
            println!("  {mark} {:<44} actual {:.3}", t.description, t.actual);
        }
    }

    if s.samples_dropped > 0 {
        println!(
            "\n  note: {} metric samples were dropped because the aggregator fell behind; \
             percentiles are based on the rest.",
            s.samples_dropped
        );
    }
    if s.dropped_iterations > 0 {
        println!(
            "\n  note: {} iterations were dropped because the load generator could not keep \
             up with the target arrival rate.",
            s.dropped_iterations
        );
    }
    println!();
}

/// Ask before running commands the scenario wants to execute locally.
fn confirm_commands(commands: &[String]) -> bool {
    use std::io::Write;
    eprintln!();
    eprintln!("This run will execute the following on this machine:");
    for c in commands {
        eprintln!("  {c}");
    }
    eprint!("Continue? [y/N] ");
    let _ = std::io::stderr().flush();

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        // Not a terminal: refuse rather than run a command nobody agreed to.
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Byte counts at the scale a load test produces them.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn state_word(s: RunState) -> &'static str {
    match s {
        RunState::Running => "running",
        RunState::Passed => "passed",
        RunState::Failed => "failed thresholds",
        RunState::Stopped => "stopped",
        RunState::Errored => "errored",
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let head: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{head}…")
    }
}
