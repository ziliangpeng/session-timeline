//! Edge-case tests for the Hermes loader: empty-title fallback, tool-name
//! fallbacks, preview truncation of tool output, sessions table filter paths,
//! and the scan_all entry point. Synthetic fixtures only.

mod fixtures;

use data_read::model::{SessionKind, SpanKind, PREVIEW_N};
use data_read::{scan_stats, ScanStats, Sources};
use fixtures::{hermes_home, sess_row, T0, T1};

fn srcs(home: &std::path::Path) -> Sources {
    Sources {
        hermes_home: home.to_path_buf(),
        prime_dir: None,
    }
}

fn user_asst() -> Vec<fixtures::Msg> {
    vec![
        fixtures::msg_row(1, "user", "hello world", 100.0, None, None, None),
        fixtures::msg_row(2, "assistant", "answer", 150.0, None, None, None),
    ]
}

#[test]
fn hermes_title_falls_back_to_first_user_message() {
    let home = hermes_home("h_title");
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &[
            fixtures::msg_row(1, "user", "first words", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "a", 150.0, None, None, None),
        ],
    );
    let ss = scan_stats(&srcs(&home), T0, T1, &ScanStats::default());
    assert_eq!(ss[0].title.as_deref(), Some("first words"));
}

#[test]
fn hermes_empty_title_string_falls_back() {
    // empty-string title (not NULL) must also fall back to user message
    let home = hermes_home("h_title_empty");
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", Some(""), None, Some("tui"), 100.0, 200.0)],
        &user_asst(),
    );
    let ss = scan_stats(&srcs(&home), T0, T1, &ScanStats::default());
    assert_eq!(ss[0].title.as_deref(), Some("hello world"));
}

#[test]
fn hermes_tool_row_without_call_id_uses_tool_name_column() {
    // orphan tool row (no matching assistant tool_calls, no call_id)
    let home = hermes_home("h_orphan_tool");
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            // assistant row WITHOUT tool_calls JSON, then a tool row arrives
            fixtures::msg_row(2, "assistant", "go", 110.0, None, None, None),
            fixtures::msg_row(3, "tool", "output", 130.0, None, None, None),
        ],
    );
    // set tool_name for row 3 via direct sqlite (fixture has no helper for this)
    {
        let conn = rusqlite::Connection::open(home.join("state.db")).unwrap();
        conn.execute("UPDATE messages SET tool_name='terminal' WHERE id=3", [])
            .unwrap();
    }
    let ss = scan_stats(&srcs(&home), T0, T1, &ScanStats::default());
    let tools: Vec<_> = ss[0]
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::Tool)
        .collect();
    assert_eq!(tools.len(), 1, "orphan tool row still spans");
    assert_eq!(tools[0].label, "terminal", "tool_name column fallback");
    assert_eq!(tools[0].meta.as_ref().unwrap().args, None, "no args known");
}

#[test]
fn hermes_long_tool_result_is_truncated_in_preview() {
    let long: String = "x".repeat(PREVIEW_N * 3);
    let home = hermes_home("h_long_result");
    let tc = r#"[{"function":{"name":"t","arguments":"{}"},"call_id":"c"}]"#;
    let long2 = long.clone();
    let _ = long2;
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "go", 110.0, None, Some(tc), None),
        ],
    );
    // append the tool row with a long result via sqlite (content is 'static in fixture)
    {
        let conn = rusqlite::Connection::open(home.join("state.db")).unwrap();
        conn.execute(
            "INSERT INTO messages (id, session_id, role, content, timestamp, active, display_order, tool_call_id)
             VALUES (3, 's1', 'tool', ?1, 130.0, 1, 3, 'c')",
            [&long],
        )
        .unwrap();
    }
    let ss = scan_stats(&srcs(&home), T0, T1, &ScanStats::default());
    let tool = ss[0]
        .spans
        .iter()
        .find(|s| s.kind == SpanKind::Tool)
        .expect("tool span");
    let result = tool.meta.as_ref().unwrap().result.as_deref().unwrap();
    assert_eq!(result.chars().count(), PREVIEW_N, "result preview capped");
}

#[test]
fn hermes_archived_and_null_start_sessions_filtered() {
    let home = hermes_home("h_filter");
    let conn = rusqlite::Connection::open(home.join("state.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, parent_session_id TEXT,
            source TEXT, archived INTEGER DEFAULT 0, started_at REAL, ended_at REAL, last_activity_at REAL);
         CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT, role TEXT, content TEXT,
            timestamp REAL, active INTEGER DEFAULT 1, display_order INTEGER, platform_message_id TEXT,
            tool_name TEXT, tool_call_id TEXT, tool_calls TEXT);",
    )
    .unwrap();
    // good session
    conn.execute(
        "INSERT INTO sessions VALUES ('good', NULL, NULL, 'tui', 0, 100.0, 200.0, 200.0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (id, session_id, role, content, timestamp, active, display_order)
         VALUES (1, 'good', 'user', 'hi', 100.0, 1, 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (id, session_id, role, content, timestamp, active, display_order)
         VALUES (2, 'good', 'assistant', 'yo', 150.0, 1, 2)",
        [],
    )
    .unwrap();
    // archived session (overlapping window) — must be excluded
    conn.execute(
        "INSERT INTO sessions VALUES ('arch', NULL, NULL, 'tui', 1, 100.0, 200.0, 200.0)",
        [],
    )
    .unwrap();
    // null started_at — excluded even though messages overlap
    conn.execute(
        "INSERT INTO sessions VALUES ('nul', NULL, NULL, 'tui', 0, NULL, 200.0, 200.0)",
        [],
    )
    .unwrap();
    // outside window — excluded
    conn.execute(
        "INSERT INTO sessions VALUES ('old', NULL, NULL, 'tui', 0, 1.0, 2.0, 2.0)",
        [],
    )
    .unwrap();
    drop(conn);

    let ss = scan_stats(&srcs(&home), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1, "only the good overlapping session");
    assert_eq!(ss[0].id, "hermes:default:good");
}

#[test]
fn hermes_session_with_no_active_messages_is_skipped() {
    let home = hermes_home("h_empty_msgs");
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &[],
    );
    let stats = ScanStats::default();
    let ss = scan_stats(&srcs(&home), T0, T1, &stats);
    assert!(ss.is_empty(), "no active messages → no session");
    assert_eq!(stats.skipped_sessions(), 0, "not an error, just empty");
}

#[test]
fn hermes_scan_all_entry_point_works() {
    // scan_all (no stats) is the public convenience wrapper
    let home = hermes_home("h_scan_all");
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &user_asst(),
    );
    let ss = data_read::loaders::hermes::scan_all(&home, T0, T1);
    assert_eq!(ss.len(), 1);
    assert_eq!(ss[0].kind, SessionKind::Human);
}

#[test]
fn hermes_discover_dbs_ignores_profiles_without_db() {
    let home = hermes_home("h_discover");
    std::fs::create_dir_all(home.join("profiles/no-db-here")).unwrap();
    let dbs = data_read::loaders::hermes::discover_dbs(&home);
    // state.db not yet written → nothing; write it and re-discover
    assert!(dbs.is_empty());
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &user_asst(),
    );
    let dbs = data_read::loaders::hermes::discover_dbs(&home);
    assert_eq!(dbs.len(), 1, "dir without state.db ignored");
    assert_eq!(dbs[0].0, "default");
}

#[test]
fn hermes_tool_row_earlier_than_pair_start_is_dropped() {
    // tool result timestamp < assistant tool_call timestamp → invalid span, dropped
    let home = hermes_home("h_tool_early");
    let tc = r#"[{"function":{"name":"t","arguments":"{}"},"call_id":"c"}]"#;
    fixtures::write_db(
        &home.join("state.db"),
        &[sess_row("s1", None, None, Some("tui"), 100.0, 200.0)],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "go", 120.0, None, Some(tc), None),
            // result timestamp BEFORE the call — must not produce a negative span
            fixtures::msg_row(3, "tool", "r", 115.0, None, None, Some("c")),
        ],
    );
    let ss = scan_stats(&srcs(&home), T0, T1, &ScanStats::default());
    assert!(
        ss[0].spans.iter().all(|s| s.kind != SpanKind::Tool),
        "negative-duration tool span dropped"
    );
    assert!(ss[0].t_end >= ss[0].t_start);
}
