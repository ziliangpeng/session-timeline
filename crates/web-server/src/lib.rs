//! web-server: harness-agnostic JSON API over the data-read loaders.
//!
//! Architecture per spec/architecture.md (first-principles rulings, 2026-09-26):
//! - a persistent in-memory session INDEX (summaries + interval map + per-day
//!   counts) built once at startup; invalidated by source-file mtimes
//!   (~276 files, <1ms stat sweep); span content is NEVER cached — every
//!   day/session request reads spans from disk on demand
//! - endpoints: /api/index (from memory), /api/day (interval-map overlap →
//!   per-session span reads), /api/session?id= (single session)
//! - gzip on span payloads (meta-heavy, compresses hard)
//! - binds 127.0.0.1 only (local production contract)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use data_read::{model::Session, scan_stats, ScanStats, Sources};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// in-memory index
// ---------------------------------------------------------------------------

#[derive(Clone, serde::Serialize)]
struct SummaryRow {
    id: String,
    title: Option<String>,
    profile: Option<String>,
    source: Option<String>,
    kind: String,
    t_start: f64,
    t_end: f64,
    span_count: usize,
    days: Vec<String>,
}

pub struct Index {
    built_at: std::time::Instant,
    /// summaries in scan order
    rows: Vec<SummaryRow>,
    /// date → row positions active that day (active-day bucketing, I1)
    by_day: HashMap<String, Vec<usize>>,
    /// source file path → mtime at index build
    source_mtimes: Vec<(PathBuf, std::time::SystemTime)>,
    skipped: SkippedCounts,
}

#[derive(Clone, Copy, Default)]
struct SkippedCounts {
    sources: u64,
    sessions: u64,
    rows: u64,
}

/// Enumerate every source file the index depends on (for mtime invalidation).
fn source_files(sources: &Sources) -> Vec<(PathBuf, Option<std::time::SystemTime>)> {
    let mut out = Vec::new();
    let mut push_db = |p: PathBuf| {
        let m = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        out.push((p, m));
    };
    if sources.hermes_home.join("state.db").is_file() {
        push_db(sources.hermes_home.join("state.db"));
    }
    if let Ok(entries) = std::fs::read_dir(sources.hermes_home.join("profiles")) {
        for e in entries.filter_map(|e| e.ok()) {
            let db = e.path().join("state.db");
            if db.is_file() {
                push_db(db);
            }
        }
    }
    if let Some(dir) = &sources.prime_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                    let m = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
                    out.push((p, m));
                }
            }
        }
    }
    out
}

fn build_index(sources: &Sources) -> Index {
    // scan EVERYTHING once (no window): the index covers all history
    let stats = ScanStats::default();
    let sessions = scan_stats(sources, f64::NEG_INFINITY, f64::INFINITY, &stats);

    let mut rows = Vec::with_capacity(sessions.len());
    let mut by_day: HashMap<String, Vec<usize>> = HashMap::new();

    for s in sessions {
        let days = active_days(&s);
        let pos = rows.len();
        for d in &days {
            by_day.entry(d.clone()).or_default().push(pos);
        }
        rows.push(SummaryRow {
            id: s.id,
            title: s.title,
            profile: s.profile,
            source: s.source,
            kind: match s.kind {
                data_read::model::SessionKind::Human => "human".to_string(),
                data_read::model::SessionKind::Sub => "sub".to_string(),
            },
            t_start: s.t_start,
            t_end: s.t_end,
            span_count: s.spans.len(),
            days,
        });
    }

    Index {
        built_at: std::time::Instant::now(),
        rows,
        by_day,
        source_mtimes: source_files(sources)
            .into_iter()
            .filter_map(|(p, m)| m.map(|m| (p, m)))
            .collect(),
        skipped: SkippedCounts {
            sources: stats.skipped_sources(),
            sessions: stats.skipped_sessions(),
            rows: stats.skipped_rows(),
        },
    }
}

/// True when any source file's mtime differs from the indexed snapshot.
fn sources_changed(idx: &Index, sources: &Sources) -> bool {
    let current = source_files(sources);
    if current.len() != idx.source_mtimes.len() {
        return true;
    }
    let old: HashMap<&Path, std::time::SystemTime> = idx
        .source_mtimes
        .iter()
        .map(|(p, m)| (p.as_path(), *m))
        .collect();
    for (p, m) in &current {
        match (old.get(p.as_path()), m) {
            (Some(o), Some(n)) if o == n => {}
            _ => return true,
        }
    }
    false
}

type SharedIndex = Arc<std::sync::RwLock<Arc<Index>>>;

fn fresh_index(shared: &SharedIndex, sources: &Sources) -> Arc<Index> {
    let current = shared.read().unwrap().clone();
    if sources_changed(&current, sources) {
        // rebuild (blocking this request; the sweep is cheap, the rebuild is
        // the full scan — acceptable for a single-user local tool; the common
        // case is "no change" and costs <1ms)
        let fresh = Arc::new(build_index(sources));
        *shared.write().unwrap() = fresh.clone();
        fresh
    } else {
        current
    }
}

// ---------------------------------------------------------------------------
// local-day math (libc; server-local timezone is the contract)
// ---------------------------------------------------------------------------

fn day_bounds(t: f64) -> (f64, String, f64) {
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
        let mut start = tm;
        start.tm_hour = 0;
        start.tm_min = 0;
        start.tm_sec = 0;
        let s = libc::mktime(&mut start);
        (s as f64, date, s as f64 + 86400.0)
    }
}

/// Invariant I1: every local day the session has activity on (span touches it).
fn active_days(s: &Session) -> Vec<String> {
    let mut days: Vec<String> = Vec::new();
    let (mut t, mut date, mut end) = day_bounds(s.t_start);
    loop {
        let active = s.spans.iter().any(|sp| sp.t_start < end && sp.t_end >= t)
            || (s.t_end >= t && s.t_start < end);
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
            break; // pathological guard
        }
    }
    days
}

/// Local-time [start, end) epoch seconds for a calendar day.
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

// ---------------------------------------------------------------------------
// endpoints
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct IndexParams {
    days: Option<f64>,
}

async fn api_index(State(app): State<AppState>, Query(p): Query<IndexParams>) -> Response {
    let idx = {
        let shared = app.index.clone();
        let sources = app.sources.clone();
        match tokio::task::spawn_blocking(move || fresh_index(&shared, &sources)).await {
            Ok(i) => i,
            Err(_) => app.index.read().unwrap().clone(),
        }
    };
    let days = p.days.unwrap_or(31.0).clamp(0.1, 3660.0);
    let t1 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let t0 = t1 - days * 86400.0;
    let sessions: Vec<&SummaryRow> = idx
        .rows
        .iter()
        .filter(|r| r.t_start < t1 && r.t_end > t0)
        .collect();
    json_response(
        serde_json::json!({
            "sessions": sessions,
            "index_age_s": idx.built_at.elapsed().as_secs_f64(),
            "skipped": {
                "sources": idx.skipped.sources,
                "sessions": idx.skipped.sessions,
                "rows": idx.skipped.rows,
            },
        }),
        false,
    )
}

#[derive(Deserialize)]
struct DayParams {
    date: String,
}

async fn api_day(State(app): State<AppState>, Query(p): Query<DayParams>) -> Response {
    let parts: Vec<&str> = p.date.split('-').collect();
    if parts.len() != 3 {
        return err400("date must be YYYY-MM-DD");
    }
    let (y, m, d): (i32, i32, i32) = match (parts[0].parse(), parts[1].parse(), parts[2].parse()) {
        (Ok(y), Ok(m), Ok(d)) => (y, m, d),
        _ => return err400("date must be YYYY-MM-DD"),
    };
    let Some((t0, t1)) = day_range(y, m, d) else {
        return err400("invalid date");
    };

    // 1) which sessions are active this day (from the in-memory index)
    let idx = {
        let shared = app.index.clone();
        let sources = app.sources.clone();
        match tokio::task::spawn_blocking(move || fresh_index(&shared, &sources)).await {
            Ok(i) => i,
            Err(_) => app.index.read().unwrap().clone(),
        }
    };
    let ids: Vec<String> = match idx.by_day.get(&p.date) {
        Some(positions) => positions.iter().map(|&i| idx.rows[i].id.clone()).collect(),
        None => idx
            .rows
            .iter()
            .filter(|r| r.t_start < t1 && r.t_end > t0)
            .map(|r| r.id.clone())
            .collect(),
    };

    // 2) read spans ONLY for those sessions (disk truth, on demand)
    let sources = app.sources.clone();
    let sessions = tokio::task::spawn_blocking(move || {
        ids.iter()
            .filter_map(|id| data_read::load_session_by_id(&sources, id))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();

    json_response(
        serde_json::json!({
            "date": p.date,
            "sessions": sessions,
        }),
        true, // gzip: meta-heavy payload compresses hard
    )
}

#[derive(Deserialize)]
struct SessionParams {
    id: String,
}

async fn api_session(State(app): State<AppState>, Query(p): Query<SessionParams>) -> Response {
    let sources = app.sources.clone();
    let id = p.id.clone();
    let session = tokio::task::spawn_blocking(move || data_read::load_session_by_id(&sources, &id))
        .await
        .unwrap_or(None);
    match session {
        Some(s) => json_response(serde_json::json!({ "session": s }), true),
        None => (StatusCode::NOT_FOUND, "unknown session id").into_response(),
    }
}

async fn shell() -> Html<&'static str> {
    Html(include_str!("app.html"))
}

// ---------------------------------------------------------------------------
// plumbing
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    sources: Sources,
    index: SharedIndex,
}

fn err400(msg: &str) -> Response {
    (StatusCode::BAD_REQUEST, msg.to_string()).into_response()
}

fn json_response(v: serde_json::Value, gzip: bool) -> Response {
    use axum::http::HeaderValue;
    let body = serde_json::to_vec(&v).unwrap_or_default();
    let mut b = Response::builder().status(StatusCode::OK).header(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if gzip {
        b = b.header(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        b.body(Body::from(gzip_bytes(&body))).unwrap()
    } else {
        b.body(Body::from(body)).unwrap()
    }
}

fn gzip_bytes(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = enc.write_all(data);
    enc.finish().unwrap_or_default()
}

/// Build an index from sources (exposed for tests and tools).
pub fn test_index(sources: &Sources) -> Index {
    build_index(sources)
}

/// Testable app builder over a pre-built index (startup warm).
pub fn app_with_index(sources: Sources, index: Index) -> Router {
    let state = AppState {
        sources,
        index: Arc::new(std::sync::RwLock::new(Arc::new(index))),
    };
    Router::new()
        .route("/", get(shell))
        .route("/api/index", get(api_index))
        .route("/api/day", get(api_day))
        .route("/api/session", get(api_session))
        .with_state(state)
}

/// Bind and serve (production entry point): warm the index, then listen.
pub async fn serve() {
    let sources = Sources::default();
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8767);

    println!("building session index…");
    let t = std::time::Instant::now();
    let index = build_index(&sources);
    println!(
        "index: {} sessions, {} days (built in {:.1}s)",
        index.rows.len(),
        index.by_day.len(),
        t.elapsed().as_secs_f64()
    );
    let app = app_with_index(sources, index);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    println!("session-timeline server on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind failed");
    axum::serve(listener, app).await.unwrap();
}
