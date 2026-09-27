//! Standalone run reports, for sending to someone who does not have Swarmo.
//!
//! The output is a single self-contained HTML file: no scripts, no external
//! stylesheets, no fonts to fetch. That is deliberate — a report is usually
//! read as an email attachment or a CI artifact, where anything that has to be
//! loaded from elsewhere either fails or leaks that the file was opened.

use crate::model::{DistBucket, RoundsSummary, RunAnnotation, RunSummary, Snapshot};

/// Escape text for inclusion in HTML body or attribute content.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Byte counts at the scale a load test produces them.
pub fn human_bytes(n: u64) -> String {
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

/// Format Unix millis as a readable UTC timestamp.
///
/// Reports travel between machines, so a local time with no zone on it would
/// be actively misleading; UTC is stated outright.
fn utc_stamp(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}

/// Unix millis as ISO-8601 UTC, e.g. `2026-09-01T17:25:56.919Z`.
pub fn iso8601(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let millis = ms % 1000;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

/// Days since the Unix epoch to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A named, coloured series: (label, stroke colour, points).
type Series<'a> = (&'a str, &'a str, Vec<(f64, f64)>);

/// One line chart, drawn as inline SVG so the report needs no scripting.
///
/// Returns an empty string when there is nothing to plot, so a run that ended
/// before its first snapshot does not produce an empty box.
fn line_chart(series: &[Series<'_>], y_label: &str) -> String {
    let has_data = series.iter().any(|(_, _, pts)| pts.len() > 1);
    if !has_data {
        return String::new();
    }
    const W: f64 = 900.0;
    const H: f64 = 220.0;
    const PAD_L: f64 = 64.0;
    const PAD_B: f64 = 34.0;
    const PAD_T: f64 = 12.0;
    const PAD_R: f64 = 16.0;

    let max_x = series
        .iter()
        .flat_map(|(_, _, p)| p.iter().map(|(x, _)| *x))
        .fold(0.0_f64, f64::max)
        .max(1.0);
    let max_y = series
        .iter()
        .flat_map(|(_, _, p)| p.iter().map(|(_, y)| *y))
        .fold(0.0_f64, f64::max)
        .max(1e-9);

    let px = |x: f64| PAD_L + (x / max_x) * (W - PAD_L - PAD_R);
    let py = |y: f64| H - PAD_B - (y / max_y) * (H - PAD_B - PAD_T);

    let mut out = format!(
        r#"<svg viewBox="0 0 {W} {H}" class="chart" role="img" aria-label="{}">"#,
        esc(y_label)
    );

    // Horizontal gridlines with their values, so the shape can be read as
    // numbers rather than just a silhouette.
    for i in 0..=4 {
        let frac = i as f64 / 4.0;
        let v = max_y * frac;
        let y = py(v);
        out.push_str(&format!(
            r#"<line class="grid" x1="{PAD_L}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}"/>"#,
            W - PAD_R
        ));
        out.push_str(&format!(
            r#"<text class="tick" x="{:.1}" y="{:.1}" text-anchor="end">{}</text>"#,
            PAD_L - 6.0,
            y + 3.5,
            fmt_axis(v)
        ));
    }
    for i in 0..=4 {
        let v = max_x * (i as f64 / 4.0);
        out.push_str(&format!(
            r#"<text class="tick" x="{:.1}" y="{:.1}" text-anchor="middle">{:.0}</text>"#,
            px(v),
            H - PAD_B + 16.0,
            v
        ));
    }
    out.push_str(&format!(
        r#"<text class="axis" x="{:.1}" y="{:.1}" text-anchor="middle">Elapsed (s)</text>"#,
        PAD_L + (W - PAD_L - PAD_R) / 2.0,
        H - 4.0
    ));
    out.push_str(&format!(
        r#"<text class="axis" transform="translate(14,{:.1}) rotate(-90)" text-anchor="middle">{}</text>"#,
        PAD_T + (H - PAD_B - PAD_T) / 2.0,
        esc(y_label)
    ));

    for (_, color, pts) in series {
        if pts.len() < 2 {
            continue;
        }
        let d: Vec<String> = pts
            .iter()
            .map(|(x, y)| format!("{:.1},{:.1}", px(*x), py(*y)))
            .collect();
        out.push_str(&format!(
            r#"<polyline fill="none" stroke="{color}" stroke-width="2" points="{}"/>"#,
            d.join(" ")
        ));
    }
    out.push_str("</svg>");

    out.push_str(r#"<div class="legend">"#);
    for (name, color, pts) in series {
        if pts.len() < 2 {
            continue;
        }
        out.push_str(&format!(
            r#"<span><i style="background:{color}"></i>{}</span>"#,
            esc(name)
        ));
    }
    out.push_str("</div>");
    out
}

/// A distribution as bars, drawn as inline SVG like every other chart here.
fn bar_chart(buckets: &[DistBucket]) -> String {
    if buckets.is_empty() {
        return String::new();
    }
    const W: f64 = 900.0;
    const H: f64 = 200.0;
    const PAD_L: f64 = 64.0;
    const PAD_B: f64 = 34.0;
    const PAD_T: f64 = 12.0;
    const PAD_R: f64 = 16.0;

    let max_count = buckets.iter().map(|b| b.count).max().unwrap_or(1).max(1) as f64;
    let max_ms = buckets.last().map(|b| b.upper_ms).unwrap_or(1.0).max(1e-9);
    let plot_w = W - PAD_L - PAD_R;
    let plot_h = H - PAD_T - PAD_B;
    let bar_w = (plot_w / buckets.len() as f64).max(1.0);

    let mut out = format!(
        r#"<svg viewBox="0 0 {W} {H}" class="chart" role="img" aria-label="Latency distribution">"#
    );
    for i in 0..=4 {
        let v = max_count * (i as f64 / 4.0);
        let y = H - PAD_B - (v / max_count) * plot_h;
        out.push_str(&format!(
            r#"<line class="grid" x1="{PAD_L}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}"/>"#,
            W - PAD_R
        ));
        out.push_str(&format!(
            r#"<text class="tick" x="{:.1}" y="{:.1}" text-anchor="end">{}</text>"#,
            PAD_L - 6.0,
            y + 3.5,
            fmt_axis(v)
        ));
    }
    for (i, b) in buckets.iter().enumerate() {
        if b.count == 0 {
            continue;
        }
        let h = (b.count as f64 / max_count) * plot_h;
        let x = PAD_L + i as f64 * bar_w;
        out.push_str(&format!(
            r##"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="#3b82f6"/>"##,
            x,
            H - PAD_B - h,
            (bar_w - 1.0).max(0.5),
            h
        ));
    }
    for i in 0..=4 {
        let v = max_ms * (i as f64 / 4.0);
        out.push_str(&format!(
            r#"<text class="tick" x="{:.1}" y="{:.1}" text-anchor="middle">{}</text>"#,
            PAD_L + (i as f64 / 4.0) * plot_w,
            H - PAD_B + 16.0,
            fmt_axis(v)
        ));
    }
    out.push_str(&format!(
        r#"<text class="axis" x="{:.1}" y="{:.1}" text-anchor="middle">Latency (ms)</text>"#,
        PAD_L + plot_w / 2.0,
        H - 4.0
    ));
    out.push_str(&format!(
        r#"<text class="axis" transform="translate(14,{:.1}) rotate(-90)" text-anchor="middle">Requests</text>"#,
        PAD_T + plot_h / 2.0
    ));
    out.push_str("</svg>");
    out
}

/// A wall time at a precision that suits its length: a batch round finishing
/// in 50 ms must not read as "0 s", and an hour must not read as "3600.000 s".
/// Mirrors `fmtDuration` in the UI so the report and the screen agree.
fn human_secs(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "—".to_string();
    }
    if seconds < 1.0 {
        return format!("{:.0} ms", seconds * 1000.0);
    }
    if seconds < 10.0 {
        return format!("{seconds:.3} s");
    }
    if seconds < 60.0 {
        return format!("{seconds:.1} s");
    }
    let whole = seconds.floor() as u64;
    let (h, m, sec) = (whole / 3600, (whole % 3600) / 60, whole % 60);
    if h > 0 {
        format!("{h}h {m:02}m {sec:02}s")
    } else {
        format!("{m}m {sec:02}s")
    }
}

fn fmt_axis(v: f64) -> String {
    if v == 0.0 {
        "0".to_string()
    } else if v >= 10.0 {
        format!("{v:.0}")
    } else if v >= 1.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

fn state_word(s: crate::model::RunState) -> (&'static str, &'static str) {
    use crate::model::RunState::*;
    match s {
        Passed => ("Passed", "ok"),
        Failed => ("Failed", "err"),
        Errored => ("Errored", "err"),
        Stopped => ("Stopped", "warn"),
        Running => ("Running", "neutral"),
    }
}

/// The run as a machine-readable report.
///
/// Shaped for reading and for diffing against other tools' output: the
/// headline blocks first — what ran, how fast, how the latency looked, what
/// the server said — with the raw per-second timeline after.
pub fn json_report(
    summary: &RunSummary,
    timeline: &[Snapshot],
    annotation: &RunAnnotation,
) -> serde_json::Value {
    let o = &summary.overall;
    let status_counts: serde_json::Map<String, serde_json::Value> = summary
        .status_codes
        .iter()
        .map(|sc| (sc.describe(), serde_json::json!(sc.count)))
        .collect();
    serde_json::json!({
        "test": {
            "name": annotation
                .label
                .clone()
                .unwrap_or_else(|| summary.scenario_name.clone()),
            "scenario": summary.scenario_ref,
            "startedAt": iso8601(summary.started_at),
            "finishedAt": iso8601(summary.ended_at),
            "notes": annotation.notes,
        },
        "summary": {
            "requests": summary.total_requests,
            "succeeded": summary.total_requests - summary.total_errors,
            "failed": summary.total_errors,
            "errorRate": summary.error_rate,
            "wallTimeSeconds": summary.duration_sec,
            "requestsPerSecond": summary.rps,
            "peakRequestsPerSecond": summary.peak_rps,
            "bytesSent": summary.bytes_out,
            "bytesReceived": summary.bytes_in,
            "sendMBPerSecond": summary.bytes_out_per_sec / 1_048_576.0,
            "receiveMBPerSecond": summary.bytes_per_sec / 1_048_576.0,
            "droppedIterations": summary.dropped_iterations,
            "tokenRefreshes": summary.token_refreshes,
            "stoppedBecause": summary.stopped_because,
            "state": summary.state,
        },
        "latencyMs": {
            "min": o.min,
            "mean": o.avg,
            "p50": o.p50,
            "p90": o.p90,
            "p95": o.p95,
            "p99": o.p99,
            "p999": o.p999,
            "max": o.max,
        },
        "statusCounts": status_counts,
        "errors": summary.errors_by_message,
        "perStep": summary.per_tag.iter().map(|t| serde_json::json!({
            "tag": t.tag,
            "requests": t.count,
            "failed": t.errors,
            "latencyMs": {
                "min": t.min, "mean": t.avg, "p50": t.p50,
                "p95": t.p95, "p99": t.p99, "max": t.max,
            },
            "bytesSent": t.bytes_out,
            "bytesReceived": t.bytes_in,
        })).collect::<Vec<_>>(),
        "rounds": summary.rounds.iter().map(|r| serde_json::json!({
            "index": r.index,
            "planned": r.planned,
            "concurrency": r.concurrency,
            "requests": r.stats.count,
            "failed": r.stats.errors,
            "wallTimeSeconds": r.wall_sec,
            "requestsPerSecond": r.rps,
            "gapSeconds": r.gap_sec,
            "latencyMs": {
                "min": r.stats.min,
                "mean": r.stats.avg,
                "p50": r.stats.p50,
                "p90": r.stats.p90,
                "p95": r.stats.p95,
                "p99": r.stats.p99,
                "max": r.stats.max,
            },
            "bytesSent": r.stats.bytes_out,
            "bytesReceived": r.stats.bytes_in,
        })).collect::<Vec<_>>(),
        "roundsSummary": RoundsSummary::of(&summary.rounds),
        "latencyDistribution": summary.latency_distribution,
        "thresholds": summary.thresholds,
        "checks": summary.checks,
        "timeline": timeline,
    })
}

/// Render a complete, self-contained HTML report for one run.
pub fn html_report(
    summary: &RunSummary,
    timeline: &[Snapshot],
    annotation: &RunAnnotation,
) -> String {
    let title = annotation
        .label
        .clone()
        .unwrap_or_else(|| summary.scenario_name.clone());
    let (state_label, state_cls) = state_word(summary.state);

    let rps_pts: Vec<(f64, f64)> = timeline
        .iter()
        .map(|s| (s.elapsed_sec as f64, s.rps))
        .collect();
    let err_pts: Vec<(f64, f64)> = timeline
        .iter()
        .map(|s| (s.elapsed_sec as f64, s.error_rate * 100.0))
        .collect();
    let vu_pts: Vec<(f64, f64)> = timeline
        .iter()
        .map(|s| (s.elapsed_sec as f64, s.active_vus as f64))
        .collect();
    let p50: Vec<(f64, f64)> = timeline
        .iter()
        .map(|s| (s.elapsed_sec as f64, s.p50))
        .collect();
    let p95: Vec<(f64, f64)> = timeline
        .iter()
        .map(|s| (s.elapsed_sec as f64, s.p95))
        .collect();
    let p99: Vec<(f64, f64)> = timeline
        .iter()
        .map(|s| (s.elapsed_sec as f64, s.p99))
        .collect();

    let mut h = String::with_capacity(16 * 1024);
    h.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    h.push_str(r#"<meta name="viewport" content="width=device-width,initial-scale=1">"#);
    h.push_str(&format!(
        "<title>{} — Swarmo run report</title>",
        esc(&title)
    ));
    h.push_str(STYLE);
    h.push_str("</head><body><main>");

    // -- header -------------------------------------------------------------
    h.push_str(&format!(
        r#"<header><h1>{}</h1><p class="sub"><span class="pill {state_cls}">{state_label}</span>
        <span>{}</span> · <span>{:.1} s</span> · <span>scenario <code>{}</code></span></p></header>"#,
        esc(&title),
        utc_stamp(summary.started_at),
        summary.duration_sec,
        esc(&summary.scenario_ref)
    ));

    if let Some(notes) = &annotation.notes {
        h.push_str(&format!(
            r#"<section><h2>Notes</h2><p class="notes">{}</p></section>"#,
            esc(notes).replace('\n', "<br>")
        ));
    }
    if let Some(err) = &summary.error {
        h.push_str(&format!(
            r#"<section><p class="banner err">{}</p></section>"#,
            esc(err)
        ));
    }

    // -- headline numbers ---------------------------------------------------
    let o = &summary.overall;
    h.push_str("<section class=\"tiles\">");
    for (label, value, sub) in [
        (
            "Requests",
            format!("{}", summary.total_requests),
            format!("{} failed", summary.total_errors),
        ),
        (
            "Throughput",
            format!("{:.0}/s", summary.rps),
            format!("peak {:.0}/s", summary.peak_rps),
        ),
        (
            "Error rate",
            format!("{:.2}%", summary.error_rate * 100.0),
            String::new(),
        ),
        (
            "p95 latency",
            format!("{:.1} ms", o.p95),
            format!("p99 {:.1} ms", o.p99),
        ),
        (
            "Data received",
            human_bytes(summary.bytes_in),
            format!("{}/s", human_bytes(summary.bytes_per_sec as u64)),
        ),
        (
            "Data sent",
            human_bytes(summary.bytes_out),
            format!("{}/s", human_bytes(summary.bytes_out_per_sec as u64)),
        ),
    ] {
        h.push_str(&format!(
            r#"<div class="tile"><div class="k">{label}</div><div class="v">{value}</div><div class="s">{sub}</div></div>"#
        ));
    }
    h.push_str("</section>");

    // -- warnings -----------------------------------------------------------
    let mut warnings = Vec::new();
    if summary.dropped_iterations > 0 {
        warnings.push(format!(
            "{} iterations were dropped: the generator could not keep up with the target arrival rate, so the achieved rate is below the one requested.",
            summary.dropped_iterations
        ));
    }
    if summary.samples_dropped > 0 {
        warnings.push(format!(
            "{} samples were dropped before they could be recorded, so the statistics below are drawn from a subset of the requests.",
            summary.samples_dropped
        ));
    }
    // Stated as a note rather than a caveat: a refresh is the feature working,
    // not a problem, but it does explain a handful of slower requests.
    let notes = if summary.token_refreshes > 0 {
        Some(format!(
            "The auth token expired and was refreshed {} time(s) during the run. \
             The requests that triggered a refresh include the time it took.",
            summary.token_refreshes
        ))
    } else {
        None
    };

    if !warnings.is_empty() {
        h.push_str("<section><h2>Caveats</h2>");
        for w in warnings {
            h.push_str(&format!(r#"<p class="banner warn">{}</p>"#, esc(&w)));
        }
        h.push_str("</section>");
    }
    if let Some(reason) = &summary.stopped_because {
        h.push_str(&format!(
            r#"<section><p class="banner ok">{}</p><p class="hint">The run ended on its stop condition rather than finishing its ramp.</p></section>"#,
            esc(reason)
        ));
    }

    if let Some(note) = notes {
        h.push_str(&format!(
            r#"<section><p class="hint">{}</p></section>"#,
            esc(&note)
        ));
    }

    // -- latency ------------------------------------------------------------
    h.push_str("<section><h2>Latency</h2><table><thead><tr>");
    for c in ["Min", "p50", "p90", "p95", "p99", "p99.9", "Max", "Mean"] {
        h.push_str(&format!("<th>{c}</th>"));
    }
    h.push_str("</tr></thead><tbody><tr>");
    for v in [o.min, o.p50, o.p90, o.p95, o.p99, o.p999, o.max, o.avg] {
        h.push_str(&format!("<td>{v:.1} ms</td>"));
    }
    h.push_str("</tr></tbody></table></section>");

    // -- rounds -------------------------------------------------------------
    // A fixed-count run's rounds sit directly under the headline numbers, as
    // they do on screen: the per-round wall time is the figure such a run is
    // made for, and the first-to-last comparison is the reason to repeat it.
    if let Some(rs) = RoundsSummary::of(&summary.rounds) {
        h.push_str("<section><h2>Rounds</h2>");
        let trend = if summary.rounds.len() > 1 && rs.first_wall_sec > 0.0 {
            let share = rs.change_sec.abs() / rs.first_wall_sec;
            // Below five percent it is timing noise, not a finding.
            if share <= 0.05 {
                String::new()
            } else if rs.change_sec < 0.0 {
                format!(
                    r#" <span class="pill ok">{} faster by the last round</span>"#,
                    human_secs(-rs.change_sec)
                )
            } else {
                format!(
                    r#" <span class="pill err">{} slower by the last round</span>"#,
                    human_secs(rs.change_sec)
                )
            }
        } else {
            String::new()
        };
        h.push_str(&format!(
            r#"<p class="hint">{} round{} · mean {} · fastest {} · slowest {}{}{}</p>"#,
            rs.rounds,
            if rs.rounds == 1 { "" } else { "s" },
            human_secs(rs.mean_wall_sec),
            human_secs(rs.fastest_wall_sec),
            human_secs(rs.slowest_wall_sec),
            if rs.total_gap_sec > 0 {
                format!(" · {}s waited between rounds", rs.total_gap_sec)
            } else {
                String::new()
            },
            trend
        ));
        h.push_str("<table><thead><tr>");
        for c in [
            "Round",
            "Requests",
            "Conc.",
            "Wall time",
            "Req/s",
            "Errors",
            "p50",
            "p95",
            "p99",
            "Max",
            "Then waited",
        ] {
            h.push_str(&format!("<th>{c}</th>"));
        }
        h.push_str("</tr></thead><tbody>");
        for r in &summary.rounds {
            h.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.0}</td><td>{}</td><td>{:.1} ms</td><td>{:.1} ms</td><td>{:.1} ms</td><td>{:.1} ms</td><td>{}</td></tr>",
                r.index,
                r.stats.count,
                r.concurrency,
                human_secs(r.wall_sec),
                r.rps,
                r.stats.errors,
                r.stats.p50,
                r.stats.p95,
                r.stats.p99,
                r.stats.max,
                if r.gap_sec > 0 { format!("{}s", r.gap_sec) } else { "—".to_string() }
            ));
        }
        h.push_str("</tbody></table></section>");
    }

    // -- distribution -------------------------------------------------------
    let dist = bar_chart(&summary.latency_distribution);
    if !dist.is_empty() {
        h.push_str("<section><h2>Latency distribution</h2>");
        h.push_str(&format!(
            r#"<p class="hint">How the requests were actually spread, which percentiles alone cannot show. The slowest 0.1% reached {:.1} ms.</p>"#,
            o.p999
        ));
        h.push_str(&dist);
        h.push_str("</section>");
    }

    // -- charts -------------------------------------------------------------
    let rps_chart = line_chart(
        &[
            ("Requests/sec", "#3b82f6", rps_pts),
            ("Error rate %", "#ef4444", err_pts),
        ],
        "Requests / sec",
    );
    if !rps_chart.is_empty() {
        h.push_str("<section><h2>Throughput over time</h2>");
        h.push_str(&rps_chart);
        h.push_str("</section>");
    }
    let lat_chart = line_chart(
        &[
            ("p50", "#22c55e", p50),
            ("p95", "#f59e0b", p95),
            ("p99", "#ef4444", p99),
        ],
        "Latency (ms)",
    );
    if !lat_chart.is_empty() {
        h.push_str("<section><h2>Latency over time</h2>");
        h.push_str(&lat_chart);
        h.push_str("</section>");
    }
    let vu_chart = line_chart(&[("Active VUs", "#8b5cf6", vu_pts)], "Virtual users");
    if !vu_chart.is_empty() {
        h.push_str("<section><h2>Virtual users over time</h2>");
        h.push_str(&vu_chart);
        h.push_str("</section>");
    }

    // -- status codes -------------------------------------------------------
    if !summary.status_codes.is_empty() {
        h.push_str("<section><h2>Status codes</h2><table><thead><tr><th>Status</th><th>Count</th><th>Share</th></tr></thead><tbody>");
        for sc in &summary.status_codes {
            let share = if summary.total_requests == 0 {
                0.0
            } else {
                sc.count as f64 / summary.total_requests as f64 * 100.0
            };
            let cls = if sc.is_ok() { "ok" } else { "err" };
            h.push_str(&format!(
                r#"<tr><td><span class="pill {cls}">{}</span></td><td>{}</td><td>{share:.1}%</td></tr>"#,
                esc(&sc.describe()),
                sc.count
            ));
        }
        h.push_str("</tbody></table></section>");
    }

    // -- errors -------------------------------------------------------------
    if !summary.errors_by_message.is_empty() {
        h.push_str("<section><h2>Failures</h2><p class=\"hint\">Requests that never produced a response, grouped by cause.</p>");
        h.push_str("<table><thead><tr><th>Cause</th><th>Count</th></tr></thead><tbody>");
        for e in &summary.errors_by_message {
            h.push_str(&format!(
                r#"<tr><td class="text">{}</td><td>{}</td></tr>"#,
                esc(&e.message),
                e.count
            ));
        }
        h.push_str("</tbody></table></section>");
    }

    // -- per step -----------------------------------------------------------
    if !summary.per_tag.is_empty() {
        h.push_str("<section><h2>By step</h2><table><thead><tr>");
        for c in [
            "Step", "Count", "Errors", "Min", "p50", "p95", "p99", "p99.9", "Max", "Data",
        ] {
            h.push_str(&format!("<th>{c}</th>"));
        }
        h.push_str("</tr></thead><tbody>");
        for t in &summary.per_tag {
            h.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{:.1} ms</td><td>{:.1} ms</td><td>{:.1} ms</td><td>{:.1} ms</td><td>{:.1} ms</td><td>{:.1} ms</td><td>{}</td></tr>",
                esc(&t.tag),
                t.count,
                t.errors,
                t.min,
                t.p50,
                t.p95,
                t.p99,
                t.p999,
                t.max,
                human_bytes(t.bytes_in)
            ));
        }
        h.push_str("</tbody></table></section>");
    }

    // -- thresholds and checks ----------------------------------------------
    if !summary.thresholds.is_empty() {
        h.push_str("<section><h2>Thresholds</h2><table><thead><tr><th>Threshold</th><th>Target</th><th>Actual</th><th>Result</th></tr></thead><tbody>");
        for t in &summary.thresholds {
            let (word, cls) = if t.passed {
                ("Passed", "ok")
            } else {
                ("Failed", "err")
            };
            h.push_str(&format!(
                r#"<tr><td>{}</td><td>{:.3}</td><td>{:.3}</td><td><span class="pill {cls}">{word}</span></td></tr>"#,
                esc(&t.description),
                t.target,
                t.actual
            ));
        }
        h.push_str("</tbody></table></section>");
    }
    if !summary.checks.is_empty() {
        h.push_str("<section><h2>Checks</h2><table><thead><tr><th>Check</th><th>Passed</th><th>Failed</th></tr></thead><tbody>");
        for c in &summary.checks {
            h.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(&c.name),
                c.passes,
                c.fails
            ));
        }
        h.push_str("</tbody></table></section>");
    }

    h.push_str(&format!(
        r#"<footer>Generated by Swarmo {} · run <code>{}</code></footer>"#,
        env!("CARGO_PKG_VERSION"),
        esc(&summary.run_id)
    ));
    h.push_str("</main></body></html>");
    h
}

const STYLE: &str = r#"<style>
:root{--bg:#fff;--fg:#16191d;--muted:#5c6470;--line:#e3e6ea;--card:#f7f8fa}
@media(prefers-color-scheme:dark){:root{--bg:#14171a;--fg:#e6e8eb;--muted:#9aa3ad;--line:#2a2f36;--card:#1b1f24}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);
 font:14px/1.5 system-ui,-apple-system,Segoe UI,Roboto,sans-serif}
main{max-width:960px;margin:0 auto;padding:32px 20px 64px}
h1{font-size:24px;margin:0 0 6px}
h2{font-size:15px;margin:0 0 10px;text-transform:uppercase;letter-spacing:.05em;color:var(--muted)}
section{margin:28px 0}
.sub{color:var(--muted);margin:0;display:flex;gap:8px;align-items:center;flex-wrap:wrap}
code{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.92em}
.pill{display:inline-block;padding:1px 8px;border-radius:999px;font-size:12px;font-weight:600;
 border:1px solid var(--line)}
.pill.ok{background:#dcfce7;color:#14532d;border-color:#bbf7d0}
.pill.err{background:#fee2e2;color:#7f1d1d;border-color:#fecaca}
.pill.warn{background:#fef3c7;color:#78350f;border-color:#fde68a}
.pill.neutral{background:var(--card);color:var(--muted)}
@media(prefers-color-scheme:dark){
 .pill.ok{background:#052e16;color:#86efac;border-color:#14532d}
 .pill.err{background:#450a0a;color:#fca5a5;border-color:#7f1d1d}
 .pill.warn{background:#451a03;color:#fcd34d;border-color:#78350f}}
.tiles{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px}
.tile{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:12px}
.tile .k{font-size:11px;text-transform:uppercase;letter-spacing:.05em;color:var(--muted)}
.tile .v{font-size:22px;font-weight:650;margin-top:2px}
.tile .s{font-size:12px;color:var(--muted);min-height:1.2em}
table{border-collapse:collapse;width:100%;font-variant-numeric:tabular-nums}
th,td{text-align:left;padding:6px 10px;border-bottom:1px solid var(--line);white-space:nowrap}
/* Numbers should never reflow, but a failure cause is a sentence. */
td.text{white-space:normal;overflow-wrap:anywhere}
th{font-size:11px;text-transform:uppercase;letter-spacing:.05em;color:var(--muted);font-weight:600}
section:has(table){overflow-x:auto}
.banner{padding:10px 12px;border-radius:8px;border:1px solid var(--line);margin:0 0 8px}
.banner.err{background:#fee2e2;color:#7f1d1d;border-color:#fecaca}
.banner.warn{background:#fef3c7;color:#78350f;border-color:#fde68a}
.banner.ok{background:#dcfce7;color:#14532d;border-color:#bbf7d0}
@media(prefers-color-scheme:dark){
 .banner.err{background:#450a0a;color:#fca5a5;border-color:#7f1d1d}
 .banner.warn{background:#451a03;color:#fcd34d;border-color:#78350f}}
.notes{white-space:pre-wrap;background:var(--card);border:1px solid var(--line);
 border-radius:8px;padding:12px;margin:0}
.hint{color:var(--muted);font-size:12px;margin:0 0 8px}
.chart{width:100%;height:auto;display:block}
.chart .grid{stroke:var(--line);stroke-width:1}
.chart .tick{fill:var(--muted);font-size:11px}
.chart .axis{fill:var(--muted);font-size:11px;font-weight:600}
.legend{display:flex;gap:14px;flex-wrap:wrap;color:var(--muted);font-size:12px;margin-top:4px}
.legend i{display:inline-block;width:10px;height:10px;border-radius:2px;margin-right:5px;
 vertical-align:-1px}
footer{margin-top:48px;padding-top:12px;border-top:1px solid var(--line);
 color:var(--muted);font-size:12px}
@media print{body{background:#fff;color:#000}main{max-width:none;padding:0}}
</style>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RoundStats;
    use crate::model::{ErrorCount, RunState, StatusCount, TagStats};
    use crate::store::Protocol;

    fn stats(tag: &str) -> TagStats {
        TagStats {
            tag: tag.into(),
            count: 100,
            errors: 2,
            min: 1.0,
            p50: 10.0,
            p90: 20.0,
            p95: 30.0,
            p99: 40.0,
            p999: 90.0,
            max: 120.0,
            avg: 12.0,
            bytes_in: 4096,
            bytes_out: 0,
        }
    }

    fn round(index: u32, wall_sec: f64, gap_sec: u64) -> RoundStats {
        RoundStats {
            index,
            planned: 5000,
            concurrency: 64,
            wall_sec,
            rps: 5000.0 / wall_sec,
            gap_sec,
            stats: stats(&format!("round {index}")),
        }
    }

    #[test]
    fn rounds_appear_in_the_html_report_with_their_wall_times() {
        let mut s = summary();
        s.rounds = vec![round(1, 0.056, 2), round(2, 0.051, 2), round(3, 0.050, 0)];
        let html = html_report(&s, &[], &RunAnnotation::default());

        assert!(html.contains("<h2>Rounds</h2>"), "section present");
        // Each round's wall time, at millisecond precision.
        assert!(html.contains("56 ms"), "{html}");
        assert!(html.contains("50 ms"), "{html}");
        // The comparison the repeat exists for: first to last, over the noise
        // floor, so it is called out.
        assert!(html.contains("faster by the last round"), "{html}");
        // Gaps are counted between rounds only — 2 + 2, not the trailing 0.
        assert!(html.contains("4s waited between rounds"), "{html}");
    }

    #[test]
    fn a_run_without_rounds_has_no_rounds_section() {
        let html = html_report(&summary(), &[], &RunAnnotation::default());
        assert!(!html.contains("<h2>Rounds</h2>"));
    }

    #[test]
    fn a_change_inside_the_noise_floor_is_not_called_a_trend() {
        let mut s = summary();
        // 2% apart: timing noise, not a finding.
        s.rounds = vec![round(1, 0.100, 0), round(2, 0.098, 0)];
        let html = html_report(&s, &[], &RunAnnotation::default());
        assert!(html.contains("<h2>Rounds</h2>"));
        assert!(!html.contains("by the last round"), "{html}");
    }

    #[test]
    fn human_secs_suits_its_precision_to_the_length() {
        assert_eq!(human_secs(0.073), "73 ms");
        assert_eq!(human_secs(1.304), "1.304 s");
        assert_eq!(human_secs(38.27), "38.3 s");
        assert_eq!(human_secs(185.0), "3m 05s");
        assert_eq!(human_secs(3725.0), "1h 02m 05s");
        assert_eq!(human_secs(f64::NAN), "—");
    }

    fn summary() -> RunSummary {
        RunSummary {
            version: 1,
            run_id: "run-1".into(),
            scenario_name: "Checkout <smoke>".into(),
            scenario_ref: "loadtests/checkout.load.json".into(),
            scenario_id: None,
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_060_000,
            duration_sec: 60.0,
            state: RunState::Passed,
            error: None,
            total_requests: 100,
            total_errors: 2,
            error_rate: 0.02,
            rps: 1.67,
            overall: stats("overall"),
            per_tag: vec![stats("get /cart")],
            checks: Vec::new(),
            thresholds: Vec::new(),
            samples_dropped: 0,
            dropped_iterations: 0,
            status_codes: vec![
                StatusCount {
                    protocol: Protocol::Http,
                    code: 200,
                    count: 98,
                },
                StatusCount {
                    protocol: Protocol::Http,
                    code: 0,
                    count: 2,
                },
            ],
            errors_by_message: vec![ErrorCount {
                message: "Connection refused".into(),
                count: 2,
            }],
            bytes_in: 4096,
            bytes_out: 0,
            bytes_per_sec: 68.0,
            bytes_out_per_sec: 0.0,
            peak_rps: 3.0,
            latency_distribution: vec![
                DistBucket {
                    upper_ms: 10.0,
                    count: 60,
                },
                DistBucket {
                    upper_ms: 20.0,
                    count: 30,
                },
                DistBucket {
                    upper_ms: 30.0,
                    count: 0,
                },
                DistBucket {
                    upper_ms: 40.0,
                    count: 10,
                },
            ],
            token_refreshes: 0,
            rounds: Vec::new(),
            stopped_because: None,
        }
    }

    fn snapshots(n: u64) -> Vec<Snapshot> {
        (0..n)
            .map(|i| Snapshot {
                elapsed_sec: i,
                active_vus: 10,
                target_vus: 10.0,
                rps: 5.0 + i as f64,
                error_rate: 0.01,
                p50: 10.0,
                p95: 30.0,
                p99: 40.0,
                per_tag: Vec::new(),
                checks: Vec::new(),
                total_requests: i * 5,
                total_errors: 0,
                samples_dropped: 0,
                dropped_iterations: 0,
                vus_saturated: false,
                threshold_results: Vec::new(),
                bytes_in: i * 100,
                bytes_out: 0,
                bytes_per_sec: 100.0,
                bytes_out_per_sec: 0.0,
                interval_error_rate: 0.0,
                interval_p95: 0.0,
                interval_p99: 0.0,
            })
            .collect()
    }

    #[test]
    fn the_report_is_self_contained() {
        let html = html_report(&summary(), &snapshots(10), &RunAnnotation::default());
        // Nothing may be fetched from elsewhere: a report is read as an
        // attachment, offline, and must not phone home.
        for probe in ["http://", "https://", "<script", "src=", "@import"] {
            assert!(!html.contains(probe), "report references {probe}");
        }
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.ends_with("</html>"));
    }

    #[test]
    fn user_text_cannot_inject_markup() {
        let mut s = summary();
        s.scenario_name = "<img src=x onerror=alert(1)>".into();
        let ann = RunAnnotation {
            label: None,
            notes: Some("</style><script>alert(2)</script>".into()),
        };
        let html = html_report(&s, &snapshots(3), &ann);
        assert!(!html.contains("<img src=x"));
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;img src=x"));
    }

    #[test]
    fn a_label_replaces_the_scenario_name_as_the_title() {
        let ann = RunAnnotation {
            label: Some("Baseline before the pool fix".into()),
            notes: None,
        };
        let html = html_report(&summary(), &snapshots(5), &ann);
        assert!(html.contains("<h1>Baseline before the pool fix</h1>"));
        // The scenario is still named, so the report says what actually ran.
        assert!(html.contains("loadtests/checkout.load.json"));
    }

    #[test]
    fn http_zero_is_not_reported_as_a_grpc_success() {
        let html = html_report(&summary(), &snapshots(3), &RunAnnotation::default());
        assert!(html.contains("HTTP (no response)"));
        assert!(!html.contains("gRPC 0 OK"));
    }

    #[test]
    fn a_run_with_no_timeline_still_renders() {
        // A run stopped before its first snapshot has no time series to plot;
        // the report must still open rather than showing an empty axis box.
        let html = html_report(&summary(), &[], &RunAnnotation::default());
        assert!(!html.contains("Throughput over time"));
        assert!(
            !html.contains("<polyline"),
            "a line chart was drawn with no timeline"
        );
        assert!(html.contains("Status codes"));
        // The distribution comes from the summary, not the timeline, so it is
        // still there — that is the point of keeping it separate.
        assert!(html.contains("Latency distribution"));
    }

    #[test]
    fn caveats_are_stated_when_numbers_are_incomplete() {
        let mut s = summary();
        s.dropped_iterations = 12;
        s.samples_dropped = 3;
        let html = html_report(&s, &snapshots(3), &RunAnnotation::default());
        assert!(html.contains("Caveats"));
        assert!(html.contains("12 iterations were dropped"));
        assert!(html.contains("3 samples were dropped"));
    }

    #[test]
    fn the_distribution_renders_one_bar_per_occupied_bucket() {
        let html = html_report(&summary(), &snapshots(5), &RunAnnotation::default());
        assert!(html.contains("Latency distribution"));
        // Three of the four buckets have requests in them; an empty bucket is
        // not drawn as a zero-height rectangle.
        assert_eq!(html.matches("<rect").count(), 3, "{html}");
    }

    #[test]
    fn distribution_bars_stay_inside_the_chart() {
        let html = html_report(&summary(), &snapshots(5), &RunAnnotation::default());
        // Pull every bar's geometry back out and check it is on the canvas.
        for rect in html.split("<rect").skip(1) {
            let attr = |name: &str| -> f64 {
                let at = rect.find(name).expect("attribute missing");
                let rest = &rect[at + name.len() + 2..];
                let end = rest.find('"').unwrap();
                rest[..end].parse().unwrap()
            };
            let (x, y, w, h) = (attr("x"), attr("y"), attr("width"), attr("height"));
            assert!(
                x >= 0.0 && x + w <= 900.0,
                "bar escapes horizontally: {rect}"
            );
            assert!(y >= 0.0 && y + h <= 200.0, "bar escapes vertically: {rect}");
            assert!(w > 0.0 && h > 0.0, "bar has no area: {rect}");
        }
    }

    #[test]
    fn a_run_without_a_distribution_omits_the_section() {
        // Runs recorded before distributions were captured simply do not show
        // the section, rather than showing an empty chart.
        let mut s = summary();
        s.latency_distribution.clear();
        let html = html_report(&s, &snapshots(5), &RunAnnotation::default());
        assert!(!html.contains("Latency distribution"));
        assert!(!html.contains("<rect"));
    }

    #[test]
    fn timestamps_are_rendered_in_utc() {
        // 1_700_000_000_000 ms is 2023-11-14 22:13:20 UTC.
        assert_eq!(utc_stamp(1_700_000_000_000), "2023-11-14 22:13:20 UTC");
        assert_eq!(utc_stamp(0), "1970-01-01 00:00:00 UTC");
    }

    #[test]
    fn byte_counts_scale() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}
