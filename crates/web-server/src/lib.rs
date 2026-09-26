//! web-server: harness-agnostic JSON API over the data-read loaders.
//!
//! Architecture per spec/architecture.md (first-principles rulings, 2026-09-26;
//! incremental-rebuild amendment after the live-UX incident, same day):
//! - a persistent in-memory session INDEX (summaries + per-day bucketing) built
//!   at startup and kept fresh INCREMENTALLY: when the mtime sweep notices a
//!   changed source file, only that source chunk (one Hermes profile DB or one
//!   Prime session file) is reloaded and the index reassembled — never a full
//!   ~13s rescan.
//! - SERVE-STALE: a request that finds the index stale answers IMMEDIATELY
//!   with the current snapshot and triggers a background refresh; the next
//!   request sees the fresh index. No request ever blocks on a rebuild.
//! - span content is NEVER cached — day/session requests read spans from disk
//!   on demand.
//! - endpoints: /api/index (from memory), /api/day (per-day bucket →
//!   per-session span reads), /api/session?id= (single session)
//! - gzip on span payloads (meta-heavy, compresses hard)
//! - binds 127.0.0.1 only (local production contract)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use data_read::{model::Session, ScanStats, Sources};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// per-source chunks: the unit of incremental rebuild
// ---------------------------------------------------------------------------

/// One Hermes profile DB or one Prime session file, with its loaded sessions
/// and the mtimes its sessions were loaded at.
#[derive(Clone)]
struct SourceChunk {
    /// stable identity: "hermes:<profile>" or "prime:<stem>"
    key: String,
    files: Vec<PathBuf>,
    /// file mtimes captured when `sessions` was loaded
    mtimes: Vec<Option<std::time::SystemTime>>,
    sessions: Vec<Session>,
    /// degradation counters from loading this chunk (spec Q4)
    skipped_sources: u64,
    skipped_sessions: u64,
    skipped_rows: u64,
}

fn load_chunk(key: &str, files: Vec<PathBuf>) -> SourceChunk {
    let stats = ScanStats::default();
    let sessions = if let Some(profile) = key.strip_prefix("hermes:") {
        // one profile DB; files[0] is the db path
        data_read::loaders::hermes::load_profile_db(
            &files[0],
            profile,
            f64::NEG_INFINITY,
            f64::INFINITY,
            &stats,
        )
        .unwrap_or_default()
    } else if key.starts_with("prime:") {
        match data_read::loaders::prime::load_file_by_stem(&files[0]) {
            Some(s) => vec![s],
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let mtimes = files
        .iter()
        .map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok())
        .collect();
    SourceChunk {
        key: key.to_string(),
        files,
        mtimes,
        sessions,
        skipped_sources: stats.skipped_sources(),
        skipped_sessions: stats.skipped_sessions(),
        skipped_rows: stats.skipped_rows(),
    }
}

/// Discover every source chunk currently on disk (cheap: readdir + stat only,
/// no data read). Key → its file(s).
fn discover_chunks(sources: &Sources) -> Vec<(String, Vec<PathBuf>)> {
    let mut out = Vec::new();
    for (profile, db) in data_read::loaders::hermes::discover_dbs(&sources.hermes_home) {
        out.push((format!("hermes:{profile}"), vec![db]));
    }
    if let Some(dir) = &sources.prime_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut files: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
                .collect();
            files.sort();
            for f in files {
                if let Some(stem) = f.file_stem().and_then(|s| s.to_str()) {
                    if !stem.is_empty() {
                        out.push((format!("prime:{stem}"), vec![f]));
                    }
                }
            }
        }
    }
    out
}

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
    /// per-source chunks this index was assembled from
    chunks: Vec<SourceChunk>,
}

impl Index {
    fn from_chunks(chunks: Vec<SourceChunk>) -> Index {
        let mut rows = Vec::new();
        let mut by_day: HashMap<String, Vec<usize>> = HashMap::new();
        for chunk in &chunks {
            for s in &chunk.sessions {
                let days = active_days(s);
                let pos = rows.len();
                for d in &days {
                    by_day.entry(d.clone()).or_default().push(pos);
                }
                rows.push(SummaryRow {
                    id: s.id.clone(),
                    title: s.title.clone(),
                    profile: s.profile.clone(),
                    source: s.source.clone(),
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
        }
        Index {
            built_at: std::time::Instant::now(),
            rows,
            by_day,
            chunks,
        }
    }

    /// Chunk keys whose files' current mtimes differ from load time, or that
    /// no longer exist on disk, or that exist on disk but not in the index.
    fn diff_chunks(&self, sources: &Sources) -> (Vec<String>, Vec<(String, Vec<PathBuf>)>) {
        let current = discover_chunks(sources);
        let known: HashMap<&str, &SourceChunk> =
            self.chunks.iter().map(|c| (c.key.as_str(), c)).collect();
        let mut changed = Vec::new();
        let mut added = Vec::new();
        for (key, files) in &current {
            match known.get(key.as_str()) {
                Some(c) => {
                    let differs = files.len() != c.files.len()
                        || files.iter().enumerate().any(|(i, f)| {
                            let m = std::fs::metadata(f).and_then(|m| m.modified()).ok();
                            m != c.mtimes.get(i).copied().flatten()
                        });
                    if differs {
                        changed.push(key.clone());
                    }
                }
                None => added.push((key.clone(), files.clone())),
            }
        }
        (changed, added)
    }

    /// Rebuild incrementally: reload only `changed` chunks, drop chunks whose
    /// files vanished, add `added` chunks, keep everything else as-is.
    fn rebuild_incremental(&self, sources: &Sources) -> Index {
        let (changed, added) = self.diff_chunks(sources);
        let current: HashMap<String, Vec<PathBuf>> = discover_chunks(sources).into_iter().collect();
        let mut chunks = Vec::with_capacity(self.chunks.len());
        for c in &self.chunks {
            if !current.contains_key(&c.key) {
                continue; // source file deleted → drop the chunk
            }
            if changed.contains(&c.key) {
                chunks.push(load_chunk(&c.key, current[&c.key].clone()));
            } else {
                chunks.push(c.clone());
            }
        }
        for (key, files) in added {
            chunks.push(load_chunk(&key, files));
        }
        Index::from_chunks(chunks)
    }
}

// ---------------------------------------------------------------------------
// shared state + serve-stale refresh
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    sources: Sources,
    index: Arc<std::sync::RwLock<Arc<Index>>>,
    /// true while a background refresh is in flight (refresh-storm guard)
    refreshing: Arc<AtomicBool>,
}

impl AppState {
    /// Serve-stale: return the current snapshot immediately; if stale, kick a
    /// background refresh (unless one is already running).
    fn snapshot(&self) -> Arc<Index> {
        let idx = self.index.read().unwrap().clone();
        let (changed, added) = idx.diff_chunks(&self.sources);
        if (!changed.is_empty() || !added.is_empty())
            && !self.refreshing.swap(true, Ordering::SeqCst)
        {
            let sources = self.sources.clone();
            let slot = self.index.clone();
            let refreshing = self.refreshing.clone();
            let stale = idx.clone();
            tokio::task::spawn_blocking(move || {
                let fresh = Arc::new(stale.rebuild_incremental(&sources));
                *slot.write().unwrap() = fresh;
                refreshing.store(false, Ordering::SeqCst);
            });
        }
        idx
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

async fn api_index(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(p): Query<IndexParams>,
) -> Response {
    let idx = app.snapshot();
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
    // cheap change signature: identical sig ⇒ UI can skip re-render entirely
    let sig = format!(
        "{}:{}:{:.3}",
        sessions.len(),
        sessions.iter().map(|s| s.span_count).sum::<usize>(),
        sessions.iter().map(|s| s.t_end).fold(0.0_f64, f64::max)
    );
    let accepts_gzip = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("gzip"))
        .unwrap_or(false);
    let (sk_s, sk_se, sk_r) = idx.chunks.iter().fold((0u64, 0u64, 0u64), |(s, se, r), c| {
        (
            s + c.skipped_sources,
            se + c.skipped_sessions,
            r + c.skipped_rows,
        )
    });
    json_response(
        serde_json::json!({
            "sessions": sessions,
            "sig": sig,
            "index_age_s": idx.built_at.elapsed().as_secs_f64(),
            "skipped": {
                "sources": sk_s,
                "sessions": sk_se,
                "rows": sk_r,
            },
        }),
        accepts_gzip, // 3.4MB → ~0.3MB; every browser accepts gzip
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
    let idx = app.snapshot();
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

impl Index {
    /// Exposed for the perf tool (issue #8): which chunks changed vs disk.
    pub fn diff_chunks_pub(&self, sources: &Sources) -> (Vec<String>, Vec<(String, Vec<PathBuf>)>) {
        self.diff_chunks(sources)
    }

    /// Exposed for the perf tool: incremental rebuild timing.
    pub fn rebuild_incremental_pub(&self, sources: &Sources) -> Index {
        self.rebuild_incremental(sources)
    }

    pub fn rows_len(&self) -> usize {
        self.rows.len()
    }

    pub fn days_len(&self) -> usize {
        self.by_day.len()
    }
}

/// Build a full index from sources (exposed for tests and tools).
pub fn test_index(sources: &Sources) -> Index {
    let chunks = discover_chunks(sources)
        .into_iter()
        .map(|(key, files)| load_chunk(&key, files))
        .collect();
    Index::from_chunks(chunks)
}

/// Testable app builder over a pre-built index (startup warm).
pub fn app_with_index(sources: Sources, index: Index) -> Router {
    let state = AppState {
        sources,
        index: Arc::new(std::sync::RwLock::new(Arc::new(index))),
        refreshing: Arc::new(AtomicBool::new(false)),
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
    let index = test_index(&sources);
    println!(
        "index: {} sessions, {} days, {} sources (built in {:.1}s)",
        index.rows.len(),
        index.by_day.len(),
        index.chunks.len(),
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
