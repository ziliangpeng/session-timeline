//! data-read: harness-specific loaders emitting the unified session model.
//! Library + `data-read` CLI (spec: loader is an individual component).

pub mod loaders;
pub mod model;

use std::path::PathBuf;

/// Degradation counters (spec Q4): every skip is counted, never fatal.
#[derive(Debug, Default)]
pub struct ScanStats {
    skipped_sources: std::sync::atomic::AtomicU64,
    skipped_sessions: std::sync::atomic::AtomicU64,
    skipped_rows: std::sync::atomic::AtomicU64,
}

impl ScanStats {
    pub fn inc_skipped_sources(&self) {
        self.skipped_sources
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn inc_skipped_sessions(&self) {
        self.skipped_sessions
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn inc_skipped_rows(&self) {
        self.skipped_rows
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn skipped_sources(&self) -> u64 {
        self.skipped_sources
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn skipped_sessions(&self) -> u64 {
        self.skipped_sessions
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn skipped_rows(&self) -> u64 {
        self.skipped_rows.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Where to read from. Defaults mirror the scheme-level paths in the spec;
/// every field is overridable by the caller (CLI flags / env in the server).
#[derive(Debug, Clone)]
pub struct Sources {
    pub hermes_home: PathBuf,
    pub prime_dir: Option<PathBuf>,
}

impl Default for Sources {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        Sources {
            hermes_home: PathBuf::from(format!("{home}/.hermes")),
            prime_dir: Some(PathBuf::from(format!("{home}/.prime/agent/sessions"))),
        }
    }
}

/// Scan every source in parallel and return unified sessions overlapping
/// `[t0, t1)`. Broken sources are skipped with a warning, never fatal.
pub fn scan(sources: &Sources, t0: f64, t1: f64) -> Vec<model::Session> {
    scan_stats(sources, t0, t1, &ScanStats::default())
}

/// Same, with degradation counters surfaced (spec Q4).
pub fn scan_stats(sources: &Sources, t0: f64, t1: f64, stats: &ScanStats) -> Vec<model::Session> {
    use rayon::prelude::*;
    let mut chunks: Vec<Vec<model::Session>> = Vec::new();

    // Hermes: one chunk per profile DB (parallel inside hermes::scan_all_stats)
    chunks.push(loaders::hermes::scan_all_stats(
        &sources.hermes_home,
        t0,
        t1,
        stats,
    ));

    // Prime: parallel across session files
    if let Some(dir) = &sources.prime_dir {
        if dir.is_dir() {
            let files: Vec<PathBuf> = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
                .collect();
            let loaded: Vec<Option<model::Session>> = files
                .par_iter()
                .filter_map(|f| loaders::prime::load_file_pub(f, t0, t1))
                .map(Some)
                .collect();
            chunks.push(loaded.into_iter().flatten().collect());
        }
    }
    chunks.into_iter().flatten().collect()
}

/// Same as [`scan`] but also returns timing/counts per source for the CLI
/// smoke output and future benchmarks.
pub struct ScanReport {
    pub sessions: Vec<model::Session>,
    pub hermes_profiles: usize,
    pub prime_files: usize,
    pub elapsed_s: f64,
    pub stats: ScanStats,
}

pub fn scan_reported(sources: &Sources, t0: f64, t1: f64) -> ScanReport {
    let start = std::time::Instant::now();
    let hermes_dbs = loaders::hermes::discover_dbs(&sources.hermes_home);
    let prime_files = sources
        .prime_dir
        .as_ref()
        .and_then(|d| std::fs::read_dir(d).ok())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
                .count()
        })
        .unwrap_or(0);
    let stats = ScanStats::default();
    let sessions = scan_stats(sources, t0, t1, &stats);
    ScanReport {
        sessions,
        hermes_profiles: hermes_dbs.len(),
        prime_files,
        elapsed_s: start.elapsed().as_secs_f64(),
        stats,
    }
}

/// Load one full session by its unified id (`hermes:<profile>:<sid>` or
/// `prime:<stem>`). Reads only the sources that could own the id — O(1)-ish
/// on the interval-index pattern (spec/architecture.md ruling 3).
pub fn load_session_by_id(sources: &Sources, id: &str) -> Option<model::Session> {
    if let Some(rest) = id.strip_prefix("hermes:") {
        let (profile, sid) = rest.split_once(':')?;
        let home = &sources.hermes_home;
        if !home.join("state.db").is_file()
            && !home
                .join("profiles")
                .join(profile)
                .join("state.db")
                .is_file()
        {
            return None;
        }
        return loaders::hermes::load_session_by_id(home, profile, sid);
    }
    if let Some(stem) = id.strip_prefix("prime:") {
        let dir = sources.prime_dir.as_ref()?;
        let f = dir.join(format!("{stem}.jsonl"));
        if !f.is_file() {
            return None;
        }
        return loaders::prime::load_file_by_stem(&f);
    }
    None
}
