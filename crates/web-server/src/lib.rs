//! web-server: harness-agnostic JSON API over the data-read loaders.
//!
//! Design (spec/overview.md + Fable ruling 2026-09-26):
//! - pure on-demand: every request scans its window fresh (1-day scan ≈ 0.17s);
//!   NO cache until a measured regression says otherwise
//! - endpoints mirror the Python prototype's proven split:
//!   GET /api/index?days=N        → month/day → session summaries
//!   GET /api/day?date=YYYY-MM-DD → full sessions active that day
//! - active-day bucketing: a session appears on every day its spans touch
//!   (invariant I1), computed by intersecting [t_start, t_end] with the day
//! - the UI shell is embedded via include_str! (single binary)

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, Json},
    routing::get,
    Router,
};
use data_read::{model::Session, scan_stats, ScanStats, Sources};
use serde::Deserialize;

/// Local-time day boundaries for an epoch timestamp: (day_start_epoch, date string).
/// The machine's local timezone is the display timezone (matches the Python
/// prototype's behaviour).
fn day_bounds(t: f64) -> (f64, String, f64) {
    // use libc localtime via chrono-free math: delegate to std by formatting
    // through the `time` crate? No — keep deps minimal: compute via
    // `date`-equivalent using libc::localtime_r.
    unsafe {
        let t_t = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t_t, &mut tm).is_null() {
            return (t, String::new(), t + 86400.0);
        }
        let date = format!(
            "{:04}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday
        );
        // start of the local day
        let mut start = tm;
        start.tm_hour = 0;
        start.tm_min = 0;
        start.tm_sec = 0;
        let s = libc::mktime(&mut start);
        (s as f64, date, s as f64 + 86400.0)
    }
}

#[derive(Deserialize)]
struct IndexParams {
    days: Option<f64>,
}

/// Light row for the index: no spans.
#[derive(serde::Serialize)]
struct SessionSummary {
    id: String,
    title: Option<String>,
    profile: Option<String>,
    source: Option<String>,
    kind: String,
    t_start: f64,
    t_end: f64,
    /// days this session touches (active-day bucketing, invariant I1)
    days: Vec<String>,
    span_count: usize,
}

#[derive(serde::Serialize)]
struct IndexResponse {
    sessions: Vec<SessionSummary>,
    elapsed_s: f64,
    skipped: Skipped,
}

#[derive(serde::Serialize)]
struct Skipped {
    sources: u64,
    sessions: u64,
    rows: u64,
}

async fn api_index(
    State(sources): State<Sources>,
    Query(p): Query<IndexParams>,
) -> Json<IndexResponse> {
    let days = p.days.unwrap_or(31.0).clamp(0.1, 366.0);
    let t1 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let t0 = t1 - days * 86400.0;
    let started = std::time::Instant::now();

    // blocking scan in a worker thread (rusqlite is sync)
    let sources_clone = sources.clone();
    let (sessions, stats) = tokio::task::spawn_blocking(move || {
        let stats = ScanStats::default();
        let s = scan_stats(&sources_clone, t0, t1, &stats);
        (s, stats)
    })
    .await
    .unwrap_or_else(|_| (Vec::new(), ScanStats::default()));

    let rows = sessions
        .iter()
        .map(|s| {
            let days = active_days(s);
            SessionSummary {
                id: s.id.clone(),
                title: s.title.clone(),
                profile: s.profile.clone(),
                source: s.source.clone(),
                kind: match s.kind {
                    data_read::model::SessionKind::Human => "human".into(),
                    data_read::model::SessionKind::Sub => "sub".into(),
                },
                t_start: s.t_start,
                t_end: s.t_end,
                span_count: s.spans.len(),
                days,
            }
        })
        .collect();
    Json(IndexResponse {
        sessions: rows,
        elapsed_s: started.elapsed().as_secs_f64(),
        skipped: Skipped {
            sources: stats.skipped_sources(),
            sessions: stats.skipped_sessions(),
            rows: stats.skipped_rows(),
        },
    })
}

/// Invariant I1: every local day the session has activity on (span touches it).
fn active_days(s: &Session) -> Vec<String> {
    let mut days: Vec<String> = Vec::new();
    let (mut t, mut date, mut end) = day_bounds(s.t_start);
    loop {
        // session is active on `date` if any span overlaps [t, t+86400)
        let active = s.spans.iter().any(|sp| sp.t_start < end && sp.t_end >= t)
            || s.t_end >= t && s.t_start < end;
        if active && !days.contains(&date) {
            days.push(date.clone());
        }
        if end > s.t_end {
            break;
        }
        let nxt = day_bounds(end + 1.0);
        t = nxt.0;
        date = nxt.1;
        end = nxt.2;
        if days.len() > 400 {
            break; // pathological guard: >13 months of days
        }
    }
    days
}

#[derive(Deserialize)]
struct DayParams {
    date: String,
}

async fn api_day(
    State(sources): State<Sources>,
    Query(p): Query<DayParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // parse YYYY-MM-DD in local time
    let parts: Vec<&str> = p.date.split('-').collect();
    if parts.len() != 3 {
        return Err((StatusCode::BAD_REQUEST, "date must be YYYY-MM-DD".into()));
    }
    let (y, m, d): (i32, i32, i32) = match (parts[0].parse(), parts[1].parse(), parts[2].parse()) {
        (Ok(y), Ok(m), Ok(d)) => (y, m, d),
        _ => return Err((StatusCode::BAD_REQUEST, "date must be YYYY-MM-DD".into())),
    };
    let (t0, t1) = match day_range(y, m, d) {
        Some(r) => r,
        None => return Err((StatusCode::BAD_REQUEST, "invalid date".into())),
    };
    let sources_clone = sources.clone();
    let sessions = tokio::task::spawn_blocking(move || {
        // scan a window that certainly contains any session touching this day
        data_read::scan(&sources_clone, t0 - 31.0 * 86400.0, t1 + 86400.0)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // sessions active on this local day: [t_start, t_end] intersects [t0, t1)
    let active: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.t_start < t1 && s.t_end > t0)
        .collect();
    Ok(Json(serde_json::json!({
        "date": p.date,
        "sessions": active,
    })))
}

/// Local-time [start, end) epoch seconds for a calendar day, or None if invalid.
fn day_range(y: i32, m: i32, d: i32) -> Option<(f64, f64)> {
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = y - 1900;
        tm.tm_mon = m - 1;
        tm.tm_mday = d;
        let start = libc::mktime(&mut tm);
        if start == -1 {
            return None;
        }
        Some((start as f64, start as f64 + 86400.0))
    }
}

async fn shell() -> Html<&'static str> {
    Html(include_str!("app.html"))
}

/// Bind and serve (production entry point).
pub async fn serve() {
    let sources = Sources::default();
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8767);

    let app = app(sources);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    println!("session-timeline server on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind failed");
    axum::serve(listener, app).await.unwrap();
}

/// Testable app builder (no bind).
pub fn app(sources: Sources) -> Router {
    Router::new()
        .route("/", get(shell))
        .route("/api/index", get(api_index))
        .route("/api/day", get(api_day))
        .with_state(sources)
}
