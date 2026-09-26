//! Perf test tool (issue #8): measures the key components that determine UI
//! responsiveness, on REAL data by default (synthetic fallback via --synthetic).
//!
//! Measurement groups:
//!   1. cold index build      — startup cost, all sources, full history
//!   2. incremental rebuild   — one changed source chunk (target: ~100x < cold)
//!   3. endpoint latencies    — /api/index, /api/day, /api/session (median+p95)
//!   4. per-source load time  — which profile DB / prime file dominates
//!   5. stale-detection sweep — mtime sweep run on every request
//!
//! Usage:
//!   perf                    # real ~/.hermes + ~/.prime/agent/sessions
//!   perf --synthetic        # generated fixture tree (N profiles, M sessions)
//!   perf --iterations 20    # endpoint iteration count (default 10)

use std::time::Instant;

use data_read::{ScanStats, Sources};

// ---------------------------------------------------------------------------
// synthetic data generation (mirrors real schemas; see data-read fixtures)
// ---------------------------------------------------------------------------

mod synth {
    use rusqlite::Connection;
    use std::path::Path;

    pub struct GenSpec {
        pub profiles: usize,
        pub sessions_per_profile: usize,
        pub turns_per_session: usize,
    }

    /// One profile DB with N sessions × M user/assistant turns.
    pub fn write_profile_db(path: &Path, profile: &str, spec: &GenSpec) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY, title TEXT, parent_session_id TEXT, source TEXT,
                archived INTEGER DEFAULT 0, started_at REAL, ended_at REAL, last_activity_at REAL
            );
            CREATE TABLE messages (
                id INTEGER PRIMARY KEY, session_id TEXT, role TEXT, content TEXT,
                timestamp REAL, active INTEGER DEFAULT 1, display_order INTEGER,
                platform_message_id TEXT, tool_name TEXT, tool_call_id TEXT, tool_calls TEXT
            );",
        )
        .unwrap();
        let base = 1_700_000_000.0f64; // Nov 2023
        for s in 0..spec.sessions_per_profile {
            let sid = format!("sess_{s:05}");
            let t0 = base + (s as f64) * 1_800.0;
            let dur = spec.turns_per_session as f64 * 120.0;
            conn.execute(
                "INSERT INTO sessions (id, title, parent_session_id, source, archived, started_at, ended_at, last_activity_at)
                 VALUES (?1, ?2, NULL, 'tui', 0, ?3, ?4, ?4)",
                rusqlite::params![
                    sid,
                    format!("session {profile}/{s} — synthetic title"),
                    t0,
                    t0 + dur
                ],
            )
            .unwrap();
            for turn in 0..spec.turns_per_session {
                let t = t0 + (turn as f64) * 120.0;
                conn.execute(
                    "INSERT INTO messages (session_id, role, content, timestamp, active, display_order)
                     VALUES (?1, 'user', ?2, ?3, 1, ?4)",
                    rusqlite::params![
                        sid,
                        format!("user turn {turn} with some payload text to give the preview body real mass"),
                        t,
                        (turn * 2) as i64
                    ],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO messages (session_id, role, content, timestamp, active, display_order)
                     VALUES (?1, 'assistant', ?2, ?3, 1, ?4)",
                    rusqlite::params![
                        sid,
                        format!("assistant turn {turn} answer text lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod"),
                        t + 45.0,
                        (turn * 2 + 1) as i64
                    ],
                )
                .unwrap();
            }
        }
    }
}

fn synthetic_sources(root: &std::path::Path, spec: &synth::GenSpec) -> Sources {
    let home = root.join("hermes");
    std::fs::create_dir_all(&home).unwrap();
    // one top-level default + N profiles
    synth::write_profile_db(&home.join("state.db"), "default", spec);
    for i in 1..=spec.profiles {
        synth::write_profile_db(
            &home.join("profiles").join(format!("p{i}")).join("state.db"),
            &format!("p{i}"),
            spec,
        );
    }
    Sources {
        hermes_home: home,
        prime_dir: None,
    }
}

// ---------------------------------------------------------------------------
// measurement helpers
// ---------------------------------------------------------------------------

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn p95(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[(v.len() as f64 * 0.95) as usize % v.len()]
}

fn row(name: &str, value: &str, note: &str) {
    println!("  {name:<34} {value:>14}  {note}");
}

// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let synthetic = args.iter().any(|a| a == "--synthetic");
    let iterations: usize = args
        .iter()
        .position(|a| a == "--iterations")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);

    let (sources, label) = if synthetic {
        let root = std::env::temp_dir().join(format!("perf-synth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let spec = synth::GenSpec {
            profiles: 8,
            sessions_per_profile: 400,
            turns_per_session: 30,
        };
        let s = synthetic_sources(&root, &spec);
        (
            s,
            "synthetic (8 profiles + default, 400 sess × 30 turns each)".to_string(),
        )
    } else {
        (
            Sources::default(),
            "real ~/.hermes + ~/.prime/agent/sessions".to_string(),
        )
    };

    println!("session-timeline perf tool (issue #8)");
    println!("data: {label}\n");

    // ---- 5. stale-detection sweep (run first: it's the per-request tax) ----
    {
        let index = web_server::test_index(&sources);
        let mut times = Vec::new();
        for _ in 0..iterations.max(10) {
            let t = Instant::now();
            let _ = index.diff_chunks_pub(&sources);
            times.push(ms(t.elapsed()));
        }
        let mut t2 = times.clone();
        row(
            "stale-detection sweep",
            &format!("{:.2} ms", median(&mut t2)),
            "runs on EVERY request (mtime stat sweep)",
        );
    }

    // ---- 4. per-source load time ----
    {
        println!();
        let mut times: Vec<(String, f64, usize)> = Vec::new();
        for (profile, db) in data_read::loaders::hermes::discover_dbs(&sources.hermes_home) {
            let stats = ScanStats::default();
            let t = Instant::now();
            let sess = data_read::loaders::hermes::load_profile_db(
                &db,
                &profile,
                f64::NEG_INFINITY,
                f64::INFINITY,
                &stats,
            )
            .unwrap_or_default();
            times.push((format!("hermes:{profile}"), ms(t.elapsed()), sess.len()));
        }
        if let Some(dir) = &sources.prime_dir {
            if dir.is_dir() {
                for e in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                        let t = Instant::now();
                        let n = data_read::loaders::prime::load_file_by_stem(&p)
                            .map(|_| 1)
                            .unwrap_or(0);
                        times.push((
                            format!("prime:{}", p.file_stem().unwrap().to_str().unwrap_or("?")),
                            ms(t.elapsed()),
                            n,
                        ));
                    }
                }
            }
        }
        times.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        let total: f64 = times.iter().map(|t| t.1).sum();
        let total_sess: usize = times.iter().map(|t| t.2).sum();
        row(
            "per-source load (total)",
            &format!("{:.0} ms", total),
            &format!("{} sources, {} sessions", times.len(), total_sess),
        );
        println!("  top 5 slowest sources:");
        for (k, v, n) in times.iter().take(5) {
            println!("    {k:<32} {v:>10.1} ms  {n:>5} sessions");
        }
    }

    // ---- 1. cold index build ----
    let cold_ms;
    {
        println!();
        let t = Instant::now();
        let index = web_server::test_index(&sources);
        cold_ms = ms(t.elapsed());
        row(
            "cold index build",
            &format!("{:.0} ms", cold_ms),
            &format!("{} sessions, {} days", index.rows_len(), index.days_len()),
        );
    }

    // ---- 2. incremental rebuild (one changed chunk) ----
    {
        // touch ONE hermes profile db (rewrite same content → mtime bump)
        let dbs = data_read::loaders::hermes::discover_dbs(&sources.hermes_home);
        let (victim_profile, victim) = dbs
            .iter()
            .max_by_key(|(_, p)| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .map(|(prof, p)| (prof.clone(), p.clone()))
            .expect("at least one profile db");
        // index built BEFORE the touch (a pristine index is what a running
        // server holds when a source changes)
        let index = web_server::test_index(&sources);

        let content = std::fs::read(&victim).unwrap();
        std::fs::write(&victim, content).unwrap();

        // the sweep must notice the victim now (other sources may also have
        // changed while the tool ran — an active machine is the norm)
        let (changed, _added) = index.diff_chunks_pub(&sources);
        assert!(
            changed.iter().any(|k| k.contains(&victim_profile)),
            "victim chunk {victim_profile} must be detected as changed"
        );

        let t = Instant::now();
        let _fresh = index.rebuild_incremental_pub(&sources);
        let inc_ms = ms(t.elapsed());
        row(
            "incremental rebuild (1 chunk)",
            &format!("{:.0} ms", inc_ms),
            &format!(
                "vs {:.0} ms cold → {:.0}x cheaper",
                cold_ms,
                cold_ms / inc_ms.max(0.001)
            ),
        );
    }

    // ---- 3. endpoint latencies (in-process router) ----
    {
        println!();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use axum::body::Body;
            use http_body_util::BodyExt;
            use tower::util::ServiceExt;

            let index = web_server::test_index(&sources);
            let app = web_server::app_with_index(sources.clone(), index);

            // find a real session id and a real day for the probes
            // (use a wide window: synthetic data is dated Nov 2023)
            let r = app
                .clone()
                .oneshot(
                    http::Request::builder()
                        .uri("/api/index?days=36500")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = r.into_body().collect().await.unwrap().to_bytes();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let sid = v["sessions"][0]["id"].as_str().unwrap().to_string();
            let day = v["sessions"][0]["days"][0].as_str().unwrap().to_string();

            async fn probe(app: &axum::Router, uri: &str) -> f64 {
                let t = Instant::now();
                let r = app
                    .clone()
                    .oneshot(
                        http::Request::builder()
                            .uri(uri)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let _ = r.into_body().collect().await.unwrap().to_bytes();
                ms(t.elapsed())
            }

            let mut t_index = Vec::new();
            let mut t_day = Vec::new();
            let mut t_session = Vec::new();
            for _ in 0..iterations {
                t_index.push(probe(&app, "/api/index?days=30").await);
                t_day.push(probe(&app, &format!("/api/day?date={day}")).await);
                t_session.push(probe(&app, &format!("/api/session?id={sid}")).await);
            }
            let mut a = t_index.clone();
            let mut b = t_day.clone();
            let mut c = t_session.clone();
            row(
                "/api/index?days=30",
                &format!("{:.1} ms", median(&mut a)),
                &format!("p95 {:.1} ms", p95(&mut t_index)),
            );
            row(
                &format!("/api/day?date={day}"),
                &format!("{:.1} ms", median(&mut b)),
                &format!("p95 {:.1} ms", p95(&mut t_day)),
            );
            row(
                "/api/session?id=<one>",
                &format!("{:.1} ms", median(&mut c)),
                &format!("p95 {:.1} ms", p95(&mut t_session)),
            );
        });
    }

    println!("\ndone.");
}
