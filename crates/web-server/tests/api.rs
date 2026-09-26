//! web-server integration tests: in-process axum router against synthetic
//! fixtures (temp-dir Hermes DB + Prime JSONL). No real data, no live port.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use data_read::Sources;
use tower::ServiceExt; // .oneshot

// reuse the data-read fixtures module (path-relative include)
#[path = "../../data-read/tests/fixtures.rs"]
mod fixtures;

fn test_sources() -> (Sources, std::path::PathBuf) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("webserver-test-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("hermes");
    let prime = base.join("prime");
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    std::fs::create_dir_all(&prime).unwrap();

    // timestamps anchored to the current day so --days windows catch them:
    // a hermes session with 2 turns + a prime session with a tool call
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as f64;
    let t0 = now - 3600.0;
    let t1 = now - 3500.0;
    let t2 = now - 3400.0;

    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "hs1",
            Some("hermes session"),
            None,
            Some("tui"),
            t0,
            t2,
        )],
        &[
            fixtures::msg_row(1, "user", "hello", t0, None, None, None),
            fixtures::msg_row(2, "assistant", "hi", t1, None, None, None),
        ],
    );
    fixtures::msg_session_id(&home.join("state.db"), "hs1");

    let ts = |t: f64| {
        // epoch → ISO UTC (minimal civil-from-days; UTC = local ± offset, but
        // for day-bucketing tests exactness within one day is not needed —
        // we assert relative structure, not specific dates)
        let secs = t as i64;
        let days = secs.div_euclid(86400);
        let rem = secs.rem_euclid(86400);
        let z = days + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    };
    std::fs::write(
        prime.join("ps1.jsonl"),
        format!(
            "{{\"type\":\"session\",\"timestamp\":\"{ts0}\"}}\n\
             {{\"type\":\"message\",\"timestamp\":\"{ts0}\",\"message\":{{\"role\":\"user\",\"content\":\"prime q\"}}}}\n\
             {{\"type\":\"message\",\"timestamp\":\"{ts1}\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"prime a\"}}]}}}}\n",
            ts0 = ts(t0),
            ts1 = ts(t1),
        ),
    )
    .unwrap();

    (
        Sources {
            hermes_home: home,
            prime_dir: Some(prime),
        },
        base,
    )
}

fn app() -> axum::Router {
    let (sources, _base) = test_sources();
    web_server::app(sources)
}

// the binary crate exposes app(); tests link against it via the lib target
// (see lib.rs: we keep main.rs thin and put logic in the bin — for testing we
// spawn the binary? No: we add a lib target exposing app()).

async fn get_json(uri: &str) -> (StatusCode, serde_json::Value) {
    let response = app()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn index_returns_sessions_with_days() {
    let (status, json) = get_json("/api/index?days=2").await;
    assert_eq!(status, StatusCode::OK);
    let sessions = json["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 2, "hermes + prime fixtures: {json}");
    let ids: Vec<&str> = sessions
        .iter()
        .map(|s| s["id"].as_str().unwrap_or_default())
        .collect();
    assert!(ids.contains(&"hermes:default:hs1"), "ids: {ids:?}");
    assert!(ids.contains(&"prime:ps1"), "ids: {ids:?}");
    for s in sessions {
        let days = s["days"].as_array().expect("days array");
        assert!(!days.is_empty(), "active-day bucketing filled: {s}");
    }
    // elapsed + skipped present
    assert!(json["elapsed_s"].is_number());
    assert!(json["skipped"]["sources"].is_number());
}

#[tokio::test]
async fn day_returns_full_sessions_active_that_day() {
    // figure out "today" local date from the index, then ask the day endpoint
    let (_, idx) = get_json("/api/index?days=1").await;
    let some_day = idx["sessions"][0]["days"][0]
        .as_str()
        .expect("a day")
        .to_string();
    let (status, json) = get_json(&format!("/api/day?date={some_day}")).await;
    assert_eq!(status, StatusCode::OK);
    let sessions = json["sessions"].as_array().expect("sessions");
    assert!(!sessions.is_empty(), "day has sessions");
    // full session objects: spans included
    let with_spans = sessions.iter().any(|s| {
        s["spans"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    });
    assert!(with_spans, "spans present in day detail: {json}");
}

#[tokio::test]
async fn day_bad_date_is_400() {
    let (status, _) = get_json("/api/day?date=not-a-date").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status2, _) = get_json("/api/day?date=2026-13-45").await;
    // mktime may normalize out-of-range; contract is just non-500
    assert_ne!(status2, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn index_clamps_days() {
    // days=0 → clamped to 0.1; days=10000 → clamped to 366; both must be 200
    let (s1, _) = get_json("/api/index?days=0").await;
    assert_eq!(s1, StatusCode::OK);
    let (s2, _) = get_json("/api/index?days=99999").await;
    assert_eq!(s2, StatusCode::OK);
}

#[tokio::test]
async fn shell_serves_html() {
    let response = app()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let ct = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(ct.contains("html"), "content-type: {ct}");
}
