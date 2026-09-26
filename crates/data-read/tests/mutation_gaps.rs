//! Tests targeting gaps found by cargo-mutants (202 mutants: 157 caught,
//! 45 missed at the time of the run). Each test kills a specific surviving
//! mutant or documents why it is equivalent.

mod fixtures;

use data_read::model::SpanKind;
use data_read::{scan, scan_stats, ScanStats, Sources};
use fixtures::{hermes_home, msg_row, prime_dir, sess_row, write_db, write_jsonl, T0, T1};

fn srcs(home: &std::path::Path) -> Sources {
    Sources {
        hermes_home: home.to_path_buf(),
        prime_dir: None,
    }
}

// kills "replace scan -> Vec<Session> with vec![]" (scan() no-stats variant untested)
#[test]
fn scan_without_stats_returns_sessions() {
    let home = hermes_home("m_scan");
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "a", 150.0, None, None, None),
        ],
    );
    let ss = scan(&srcs(&home), T0, T1);
    assert_eq!(ss.len(), 1, "scan() itself (not just scan_stats) must work");
    assert_eq!(ss[0].id, "hermes:default:s1");
}

// kills "skipped_sources/sessions/rows -> constant" mutants: a fully healthy
// scan must report exactly 0 skips everywhere.
#[test]
fn healthy_scan_reports_zero_skips() {
    let home = hermes_home("m_zero_skips");
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "a", 150.0, None, None, None),
        ],
    );
    let stats = ScanStats::default();
    let ss = scan_stats(&srcs(&home), T0, T1, &stats);
    assert_eq!(ss.len(), 1);
    assert_eq!(stats.skipped_sources(), 0, "no broken DBs → 0");
    assert_eq!(stats.skipped_sessions(), 0, "no broken sessions → 0");
    assert_eq!(stats.skipped_rows(), 0, "no bad rows → 0");
}

// kills "inc_skipped_sessions with ()" and "skipped_sessions -> 0":
// a DB whose messages TABLE is missing → every session skipped at the
// per-session level, counted in skipped_sessions.
#[test]
fn missing_messages_table_counts_skipped_sessions() {
    let home = hermes_home("m_no_msgs_table");
    let db = home.join("state.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, parent_session_id TEXT,
                source TEXT, archived INTEGER DEFAULT 0, started_at REAL, ended_at REAL, last_activity_at REAL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('s1', NULL, NULL, 'tui', 0, 100.0, 200.0, 200.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('s2', NULL, NULL, 'tui', 0, 110.0, 220.0, 220.0)",
            [],
        )
        .unwrap();
    }
    let stats = ScanStats::default();
    let ss = scan_stats(&srcs(&home), T0, T1, &stats);
    assert!(ss.is_empty());
    assert_eq!(
        stats.skipped_sessions(),
        2,
        "both sessions skipped, counted"
    );
    assert_eq!(
        stats.skipped_sources(),
        0,
        "DB opened fine; sessions table fine"
    );
}

// kills union_duration mutants (a <= ce with true; += with -/*; - with +//):
// two DISJOINT same-kind spans — union must be the sum of parts, never the
// enclosing extent.
#[test]
fn union_of_disjoint_same_kind_spans_is_sum_not_extent() {
    let home = hermes_home("m_union");
    let tc_a = r#"[{"function":{"name":"a","arguments":"{}"},"call_id":"a"}]"#;
    let tc_b = r#"[{"function":{"name":"b","arguments":"{}"},"call_id":"b"}]"#;
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 900.0)],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "go", 110.0, None, Some(tc_a), None),
            fixtures::msg_row(3, "tool", "ra", 120.0, None, None, Some("a")),
            fixtures::msg_row(4, "assistant", "go2", 200.0, None, Some(tc_b), None),
            fixtures::msg_row(5, "tool", "rb", 210.0, None, None, Some("b")),
        ],
    );
    let ss = scan_stats(&srcs(&home), T0, T1, &ScanStats::default());
    let s = &ss[0];
    // [110,120] and [200,210]: union = 20, NOT 110 (extent)
    assert_eq!(
        s.union_duration(SpanKind::Tool),
        20.0,
        "union sums disjoint parts"
    );
    // and inference: [100,110] + [120,200]? no — 120→200 is assistant→assistant.
    // Keep the assertion to the tool kind; inference already covered elsewhere.
    assert_eq!(
        s.spans.iter().filter(|x| x.kind == SpanKind::Tool).count(),
        2
    );
}

// kills "> with >=" in both loaders: two messages with IDENTICAL timestamps
// must not create a zero-length span (span count stays exact).
#[test]
fn equal_timestamps_create_no_zero_length_spans() {
    let home = hermes_home("m_equal_ts");
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            // same timestamp as the user row: no inference span may appear
            fixtures::msg_row(2, "assistant", "a", 100.0, None, None, None),
            fixtures::msg_row(3, "user", "again", 150.0, None, None, None),
        ],
    );
    let ss = scan_stats(&srcs(&home), T0, T1, &ScanStats::default());
    let s = &ss[0];
    let inference: Vec<_> = s
        .spans
        .iter()
        .filter(|x| x.kind == SpanKind::Inference)
        .collect();
    assert!(
        inference.is_empty(),
        "zero-gap must not produce a span; got {inference:?}"
    );
    let idle: Vec<_> = s
        .spans
        .iter()
        .filter(|x| x.kind == SpanKind::Idle)
        .collect();
    assert_eq!(idle.len(), 1, "only the real 100→150 idle");
    assert!((idle[0].t_end - idle[0].t_start - 50.0).abs() < 1e-9);
}

// kills "delete field output from Meta" (hermes assistant output preview unchecked)
// and locks the inference preview content.
#[test]
fn hermes_inference_preview_carries_output() {
    let home = hermes_home("m_output");
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "the answer text", 150.0, None, None, None),
        ],
    );
    let ss = scan_stats(&srcs(&home), T0, T1, &ScanStats::default());
    let inf = ss[0]
        .spans
        .iter()
        .find(|s| s.kind == SpanKind::Inference)
        .expect("inference span");
    assert_eq!(
        inf.meta.as_ref().unwrap().output.as_deref(),
        Some("the answer text")
    );
}

// kills "delete field note from Meta" (injected-idle note never asserted)
#[test]
fn hermes_injected_idle_carries_note() {
    let home = hermes_home("m_note");
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &[
            fixtures::msg_row(1, "assistant", "a", 110.0, None, None, None),
            // injected user message ends the idle
            fixtures::msg_row(2, "user", "injected", 140.0, Some("pm1"), None, None),
            fixtures::msg_row(3, "assistant", "b", 150.0, None, None, None),
        ],
    );
    let ss = scan_stats(&srcs(&home), T0, T1, &ScanStats::default());
    let idle = ss[0]
        .spans
        .iter()
        .find(|s| s.kind == SpanKind::Idle && s.t_start == 110.0)
        .expect("idle ended by injection");
    let m = idle.meta.as_ref().unwrap();
    assert!(
        m.note.is_some(),
        "injected-ended idle is labeled with a note"
    );
    assert_eq!(m.next_user, None, "injected text is not next_user");
}

// kills "match guard !t.is_empty() with false" (provided non-empty title
// must win over the user-message fallback)
#[test]
fn hermes_provided_title_wins_over_fallback() {
    let home = hermes_home("m_title_wins");
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row(
            "s1",
            Some("custom title"),
            None,
            Some("tui"),
            100.0,
            200.0,
        )],
        &[
            fixtures::msg_row(
                1,
                "user",
                "user text as title candidate",
                100.0,
                None,
                None,
                None,
            ),
            fixtures::msg_row(2, "assistant", "a", 150.0, None, None, None),
        ],
    );
    let ss = scan_stats(&srcs(&home), T0, T1, &ScanStats::default());
    assert_eq!(
        ss[0].title.as_deref(),
        Some("custom title"),
        "DB title beats user-message fallback"
    );
    assert_ne!(ss[0].title.as_deref(), Some("user text as title candidate"));
}

// kills "delete field next_user from Meta" in the hermes idle branch
#[test]
fn hermes_real_idle_carries_next_user() {
    let home = hermes_home("m_next_user");
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &[
            fixtures::msg_row(1, "user", "first", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "a", 110.0, None, None, None),
            fixtures::msg_row(3, "user", "what came next", 160.0, None, None, None),
        ],
    );
    let ss = scan_stats(&srcs(&home), T0, T1, &ScanStats::default());
    let idle = ss[0]
        .spans
        .iter()
        .find(|s| s.kind == SpanKind::Idle)
        .expect("idle");
    assert_eq!(
        idle.meta.as_ref().unwrap().next_user.as_deref(),
        Some("what came next")
    );
}

// kills sub-kind display mutants in the CLI human output: a sub session must
// print "sub: 1". Runs the real binary.
#[test]
fn cli_human_output_counts_sub_sessions() {
    use std::process::Command;
    let dir = prime_dir("m_cli_sub");
    // assistant-only session → sub kind, anchored near NOW so --days 1 catches it
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // epoch → ISO (UTC), minimal civil-from-days
    let iso = |t: u64| {
        let days = (t / 86400) as i64;
        let secs = t % 86400;
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
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    };
    write_jsonl(
        &dir,
        "sub.jsonl",
        &[
            r#"{"type":"session","timestamp":"PLACEHOLDER"}"#.replace("PLACEHOLDER", &iso(now - 60))
                .as_str(),
            r#"{"type":"message","timestamp":"PLACEHOLDER2","message":{"role":"assistant","content":[{"type":"text","text":"autonomous"}]}}"#
                .replace("PLACEHOLDER2", &iso(now - 50))
                .as_str(),
            r#"{"type":"message","timestamp":"PLACEHOLDER3","message":{"role":"assistant","content":[{"type":"text","text":"more"}]}}"#
                .replace("PLACEHOLDER3", &iso(now - 40))
                .as_str(),
        ],
    );
    let out = Command::new(env!("CARGO_BIN_EXE_data-read"))
        .args([
            "scan",
            "--days",
            "1",
            "--hermes-home",
            "/tmp/definitely-not-here",
            "--prime-dir",
            dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("sub: 1"),
        "sub count printed; got: {stdout}"
    );
    assert!(
        stdout.contains("1 prime files"),
        "prime file count printed: {stdout}"
    );
}

#[test]
fn by_id_profile_matches_scan_profile() {
    // regression: /api/session once returned hermes:hermes:hs1 (profile guessed
    // from the db parent dir) while the index said hermes:default:hs1. The
    // by-id load MUST return the same id the scan path produced.
    use data_read::{scan_stats, ScanStats, Sources};
    let home = hermes_home("byid-profile");
    write_db(
        &home.join("state.db"),
        &[sess_row("s1", Some("t"), None, Some("tui"), 100.0, 200.0)],
        &[
            msg_row(1, "user", "hi", 100.0, None, None, None),
            msg_row(2, "assistant", "yo", 150.0, None, None, None),
        ],
    );
    let sources = Sources {
        hermes_home: home.clone(),
        prime_dir: None,
    };
    let scanned = scan_stats(&sources, 0.0, 1e9_f64, &ScanStats::default());
    assert_eq!(scanned.len(), 1);
    let by_id = data_read::load_session_by_id(&sources, &scanned[0].id)
        .expect("by-id load must succeed for a scanned id");
    assert_eq!(by_id.id, scanned[0].id, "by-id id must equal scanned id");
}
