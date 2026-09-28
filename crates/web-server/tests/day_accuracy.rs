//! Accuracy tests for the /api/day fast path (perf PR #21):
//! day-window span clipping, parallel-load determinism, fingerprint cache
//! correctness. All fixtures are synthetic — no real session data.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use data_read::Sources;
use tower::ServiceExt; // .oneshot

#[path = "../../data-read/tests/fixtures.rs"]
mod fixtures;

use std::sync::atomic::{AtomicU64, Ordering};

fn base_dir(tag: &str) -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("day-acc-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("hermes/profiles")).unwrap();
    base
}

/// A two-day Hermes session: turns on both days + an overnight idle crossing
/// midnight. Timestamps anchored to a fixed local-midnight-aligned epoch so
/// day boundaries are deterministic: D0 = 1000000000, day length 86400.
fn write_two_day_fixture(home: &std::path::Path) -> (f64, f64) {
    // Pick t values relative to a real local midnight so the date math is
    // stable across timezones: take "now", floor to local midnight, use the
    // resulting day and the next.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as f64;
    let (day0, day1) = local_midnight_pair(now);
    // day 1 activity: 10:00 user, 10:01 assistant
    let a0 = day0 + 10.0 * 3600.0;
    let a1 = a0 + 60.0;
    // overnight idle: 23:00 day1 → 08:00 day2
    let n0 = day0 + 23.0 * 3600.0;
    let n1 = day1 + 8.0 * 3600.0;
    // day 2 activity: 08:00 user, 08:01 assistant
    let b0 = n1;
    let b1 = b0 + 60.0;

    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "cross",
            Some("two-day session"),
            None,
            Some("tui"),
            a0,
            b1,
        )],
        &[
            fixtures::msg_row(1, "user", "day one q", a0, None, None, None),
            fixtures::msg_row(2, "assistant", "day one a", a1, None, None, None),
            fixtures::msg_row(3, "user", "day two q", b0, None, None, None),
            fixtures::msg_row(4, "assistant", "day two a", b1, None, None, None),
        ],
    );
    fixtures::msg_session_id(&home.join("state.db"), "cross");
    (day0, day1)
}

/// (t0 of the local day containing t, t0 of the NEXT local day)
fn local_midnight_pair(t: f64) -> (f64, f64) {
    let date = web_server::test_day_bounds_date(t);
    let (t0, t1) = web_server::test_day_bounds(&date);
    (t0, t1)
}

fn sources_for(tag: &str) -> (Sources, std::path::PathBuf) {
    let base = base_dir(tag);
    let home = base.join("hermes");
    write_two_day_fixture(&home);
    (
        Sources {
            hermes_home: home,
            prime_dir: None,
        },
        base,
    )
}

async fn day_json(
    router: &axum::Router,
    date: &str,
) -> (StatusCode, serde_json::Value) {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/day?date={date}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let gzipped = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        == Some("gzip");
    let bytes = axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    let plain = if gzipped {
        let mut d = flate2::read::GzDecoder::new(&bytes[..]);
        let mut out = Vec::new();
        use std::io::Read;
        d.read_to_end(&mut out).unwrap();
        out
    } else {
        bytes.to_vec()
    };
    (
        status,
        serde_json::from_slice(&plain).unwrap_or(serde_json::Value::Null),
    )
}

fn spans_of<'a>(json: &'a serde_json::Value) -> Vec<(&'a str, f64, f64)> {
    json["sessions"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|s| s["spans"].as_array())
        .map(|spans| {
            spans
                .iter()
                .map(|sp| {
                    (
                        sp["kind"].as_str().unwrap_or_default(),
                        sp["t_start"].as_f64().unwrap_or(0.0),
                        sp["t_end"].as_f64().unwrap_or(0.0),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// ACCURACY: a cross-day session appears on BOTH days, each day carrying only
/// the spans overlapping that day — the overnight idle belongs to both days,
/// keeping its ORIGINAL timestamps (carryover detection depends on t_start
/// staying pre-midnight even after clipping).
#[tokio::test]
async fn cross_day_session_clips_per_day_keeps_carryover() {
    let (sources, _base) = sources_for("clip");
    let index = web_server::test_index(&sources);
    let router = web_server::app_with_index(sources, index);

    let (day0, day1) = local_midnight_pair(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as f64);
    let d1 = web_server::test_day_bounds_date(day0 + 12.0 * 3600.0);
    let d2 = web_server::test_day_bounds_date(day1 + 12.0 * 3600.0);

    let (s1, j1) = day_json(&router, &d1).await;
    assert_eq!(s1, StatusCode::OK);
    let spans1 = spans_of(&j1);
    assert!(!spans1.is_empty(), "day-1 spans present: {j1}");
    for (kind, a, b) in &spans1 {
        assert!(*a < day1 && *b > day0, "day-1 span escapes window: {kind} {a}..{b}");
        assert!(
            *a >= day0,
            "day-1 span starts before midnight (must keep clipped-in start): {kind} {a}"
        );
    }
    // the overnight idle IS included on day 1 (it starts 23:00 that day)
    assert!(
        spans1.iter().any(|(k, a, b)| *k == "idle" && *b > day1),
        "overnight idle must be present on day 1 with t_end past midnight: {spans1:?}"
    );

    let (s2, j2) = day_json(&router, &d2).await;
    assert_eq!(s2, StatusCode::OK);
    let spans2 = spans_of(&j2);
    assert!(!spans2.is_empty(), "day-2 spans present: {j2}");
    // carryover: the overnight idle keeps its PRE-midnight t_start on day 2
    let carry: Vec<_> = spans2
        .iter()
        .filter(|(k, a, _)| *k == "idle" && *a < day1)
        .collect();
    assert_eq!(
        carry.len(),
        1,
        "exactly one carryover idle on day 2, original t_start preserved: {spans2:?}"
    );
    for (kind, a, b) in &spans2 {
        assert!(*a < day1 + 86400.0 && *b > day1, "day-2 span escapes window: {kind} {a}..{b}");
    }
    // day-2 non-carryover spans start at/after midnight
    for (kind, a, _) in &spans2 {
        if *kind == "idle" && *a < day1 {
            continue; // the carryover one
        }
        assert!(*a >= day1, "day-2 span starts before midnight: {kind} {a}");
    }
}

/// ACCURACY: the full session (all spans, unclipped) is still served by
/// /api/session — the detail view's data source after day payloads clip.
#[tokio::test]
async fn session_endpoint_keeps_full_history_for_cross_day_session() {
    let (sources, _base) = sources_for("full");
    let index = web_server::test_index(&sources);
    let router = web_server::app_with_index(sources, index);

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/session?id=hermes:default:cross")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let gzipped = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        == Some("gzip");
    let bytes = axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    let plain = if gzipped {
        let mut d = flate2::read::GzDecoder::new(&bytes[..]);
        let mut out = Vec::new();
        use std::io::Read;
        d.read_to_end(&mut out).unwrap();
        out
    } else {
        bytes.to_vec()
    };
    let json: serde_json::Value = serde_json::from_slice(&plain).unwrap();
    let spans = json["session"]["spans"]
        .as_array()
        .expect("spans array")
        .len();
    // 2-day session: day-1 turn, day-2 turn + the overnight idle between them
    // (spans built from 4 messages = 3 gaps) — all 3 must survive unclipped.
    assert_eq!(spans, 3, "full session keeps ALL spans: {json}");
}

/// ACCURACY: parallel day loading returns the same payload as serial —
/// deterministic ordering (index order), no dropped/duplicated sessions.
#[tokio::test]
async fn parallel_day_load_matches_serial() {
    use std::collections::HashSet;

    // fixture with several sessions on the same day
    let base = base_dir("par");
    let home = base.join("hermes");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as f64;
    let (day0, _day1) = local_midnight_pair(now);
    let t = day0 + 9.0 * 3600.0;
    let mut sess = Vec::new();
    let mut msgs = Vec::new();
    let sids: Vec<String> = (0..12).map(|i| format!("s{i}")).collect();
    for (i, sid) in sids.iter().enumerate() {
        sess.push(fixtures::sess_row(
            sid,
            Some("par session"),
            None,
            Some("tui"),
            t + i as f64 * 100.0,
            t + i as f64 * 100.0 + 50.0,
        ));
        msgs.push(fixtures::msg_row(
            2 * i as i64 + 1,
            "user",
            "q",
            t + i as f64 * 100.0,
            None,
            None,
            None,
        ));
        msgs.push(fixtures::msg_row(
            2 * i as i64 + 2,
            "assistant",
            "a",
            t + i as f64 * 100.0 + 50.0,
            None,
            None,
            None,
        ));
    }
    fixtures::write_db(&home.join("state.db"), &sess, &msgs);
    // write_db hardwires session_id='s1'; re-point each message pair at its
    // own session (fixture helper overrides the whole table, so do it per-id)
    {
        let conn = rusqlite::Connection::open(home.join("state.db")).unwrap();
        for (i, sid) in sids.iter().enumerate() {
            conn.execute(
                "UPDATE messages SET session_id = ?1 WHERE id IN (?2, ?3)",
                rusqlite::params![sid, 2 * i as i64 + 1, 2 * i as i64 + 2],
            )
            .unwrap();
        }
    }
    let sources = Sources {
        hermes_home: home,
        prime_dir: None,
    };

    let index = web_server::test_index(&sources);
    let date = web_server::test_day_bounds_date(t);
    let (t0, t1) = web_server::test_day_bounds(&date);
    let ids = index.ids_for_day(&date, t0, t1);
    assert_eq!(ids.len(), 12, "index found the 12 sessions");

    let mut serial = Vec::new();
    for id in &ids {
        if let Some(s) = data_read::load_session_by_id(&sources, id) {
            let mut s = s;
            s.spans.retain(|sp| sp.t_start < t1 && sp.t_end > t0);
            serial.push((s.id.clone(), s.spans.len()));
        }
    }
    let par: Vec<(String, usize)> = {
        use rayon::prelude::*;
        ids.par_iter()
            .filter_map(|id| data_read::load_session_by_id(&sources, id))
            .map(|mut s| {
                s.spans.retain(|sp| sp.t_start < t1 && sp.t_end > t0);
                (s.id.clone(), s.spans.len())
            })
            .collect()
    };

    let ser_set: HashSet<_> = serial.iter().collect();
    let par_set: HashSet<_> = par.iter().collect();
    assert_eq!(ser_set, par_set, "parallel == serial: same sessions, same span counts");
    assert_eq!(par.len(), 12, "nothing dropped in parallel load");
}

/// ACCURACY: the day fingerprint cache must NOT serve a stale body after the
/// underlying source file changes (mtime bump → fingerprint mismatch).
#[tokio::test]
async fn day_cache_invalidates_on_source_change() {
    let (sources, _base) = sources_for("cache");
    let index = web_server::test_index(&sources);
    let router = web_server::app_with_index(sources.clone(), index);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as f64;
    let (day0, _) = local_midnight_pair(now);
    let d1 = web_server::test_day_bounds_date(day0 + 12.0 * 3600.0);

    let (s1, j1) = day_json(&router, &d1).await;
    assert_eq!(s1, StatusCode::OK);
    let n1 = spans_of(&j1).len();

    // warm the cache (second request), then mutate the source DB: add a turn.
    let (_, _j2) = day_json(&router, &d1).await;
    let db = sources.hermes_home.join("state.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "INSERT INTO messages (id, session_id, role, content, timestamp, display_order, active, compacted, platform_message_id, tool_name, tool_call_id, tool_calls, _compressed_summary)
         VALUES (99, 'cross', 'user', 'late q', ?1, 3, 1, 0, NULL, NULL, NULL, NULL, 0)",
        [day0 + 20.0 * 3600.0],
    )
    .unwrap();
    drop(conn);
    // ensure mtime actually moves (HFS+ resolution): force a distinct mtime
    set_mtime_distinct(&db);

    // serve-stale: the index snapshot may lag; wait for the background rebuild
    // triggered by api_day's snapshot() to land, then re-request.
    let mut n2 = n1;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let (_, j3) = day_json(&router, &d1).await;
        n2 = spans_of(&j3).len();
        if n2 != n1 {
            break;
        }
    }
    assert_ne!(
        n2, n1,
        "cache must not serve the pre-mutation body after the source DB changed"
    );
}

fn set_mtime_distinct(path: &std::path::Path) {
    // bump mtime by +2s vs current so it differs even on coarse filesystems.
    // libc::utimes with a CString path (macOS/Linux) — and VERIFY it took:
    // a silent no-op here would fake a cache-invalidation pass.
    let md = std::fs::metadata(path).unwrap();
    let epoch = md
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 2;
    unsafe {
        let times: [libc::timeval; 2] = [
            libc::timeval { tv_sec: epoch, tv_usec: 0 },
            libc::timeval { tv_sec: epoch, tv_usec: 0 },
        ];
        let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        let rc = libc::utimes(c.as_ptr(), times.as_ptr());
        assert_eq!(rc, 0, "utimes failed");
    }
    let after = std::fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(after as i64, epoch, "mtime did not actually move");
}
