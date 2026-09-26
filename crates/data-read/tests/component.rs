//! Unit tests for the data-read component: synthetic fixtures only (no real
//! DBs, no personal data). Each test names the spec decision it locks.

mod fixtures;

use data_read::model::{SessionKind, SpanKind, PREVIEW_N};
use data_read::{scan_stats, ScanStats, Sources};
use fixtures::{hermes_home, prime_dir, write_jsonl, T0, T1};

// ---------- Q2: window semantics ----------

#[test]
fn q2_session_overlapping_window_is_returned_unchanged() {
    // session [10, 400] vs window [100, 300): overlaps → returned, NOT clipped
    let dir = prime_dir("q2_overlap");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:00:10Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:00:10Z","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:06:40Z","message":{"role":"assistant","content":[{"type":"text","text":"yo"}]}}"#,
        ],
    );
    let ss = scan_stats(&sources(&dir), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    let s = &ss[0];
    assert!(
        (s.t_start - 10.0).abs() < 1e-9 && (s.t_end - 400.0).abs() < 1e-9,
        "extent from spans, not window: got [{}, {}]",
        s.t_start,
        s.t_end
    );
}

#[test]
fn q2_session_ending_before_window_is_excluded() {
    let dir = prime_dir("q2_before");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:00:01Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:00:01Z","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:00:05Z","message":{"role":"assistant","content":[{"type":"text","text":"yo"}]}}"#,
        ],
    );
    let ss = scan_stats(&sources(&dir), T0, T1, &ScanStats::default());
    assert!(ss.is_empty(), "ended at t=5 < t0=100");
}

#[test]
fn q2_session_starting_after_window_is_excluded() {
    let dir = prime_dir("q2_after");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:10:00Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:10:00Z","message":{"role":"user","content":"hi"}}"#,
        ],
    );
    let ss = scan_stats(&sources(&dir), T0, T1, &ScanStats::default());
    assert!(ss.is_empty(), "starts at t=600 > t1=300");
}

// ---------- Q3: identity ----------

#[test]
fn q3_hermes_ids_are_namespaced() {
    // hermes ids: hermes:<profile>:<sid>; prime ids: prime:<file-stem>
    let home = hermes_home("q3");
    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "s1",
            Some("my title"),
            None,
            Some("tui"),
            100.0,
            200.0,
        )],
        &[
            fixtures::msg_row(1, "user", "hello", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "hi", 150.0, None, None, None),
        ],
    );
    let ss = scan_stats(&sources_p(&home), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    assert_eq!(ss[0].id, "hermes:default:s1");
    assert_eq!(ss[0].profile.as_deref(), Some("default"));
    assert_eq!(ss[0].source.as_deref(), Some("tui"));
}

#[test]
fn q3_prime_ids_are_namespaced() {
    let dir = prime_dir("q3_prime");
    write_jsonl(
        &dir,
        "abc123.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:40Z","message":{"role":"user","content":"hi"}}"#,
        ],
    );
    let ss = scan_stats(&sources(&dir), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    assert_eq!(ss[0].id, "prime:abc123");
}

// ---------- Q4: degradation ----------

#[test]
fn q4_broken_source_is_skipped_and_counted() {
    // one good profile + one garbage "db" → good one still loads; skip counted
    let home = hermes_home("q4_src");
    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "s1",
            None,
            None,
            Some("tui"),
            100.0,
            200.0,
        )],
        &[
            fixtures::msg_row(1, "user", "hello", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "hi", 150.0, None, None, None),
        ],
    );
    let bad = home.join("profiles/broken/state.db");
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    std::fs::write(&bad, b"this is not sqlite").unwrap();
    let stats = ScanStats::default();
    let ss = scan_stats(&sources_p(&home), T0, T1, &stats);
    assert_eq!(ss.len(), 1, "good profile still loads");
    assert_eq!(stats.skipped_sources(), 1, "broken DB counted");
    assert_eq!(stats.skipped_sessions(), 0);
}

#[test]
fn q4_malformed_tool_calls_row_is_counted_not_fatal() {
    let home = hermes_home("q4_row");
    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "s1",
            None,
            None,
            Some("tui"),
            100.0,
            200.0,
        )],
        &[
            fixtures::msg_row(1, "user", "hello", 100.0, None, None, None),
            fixtures::msg_row(
                2,
                "assistant",
                "thinking",
                110.0,
                None,
                Some("not json {{{"),
                None,
            ),
            fixtures::msg_row(3, "user", "again", 120.0, None, None, None),
        ],
    );
    let stats = ScanStats::default();
    let ss = scan_stats(&sources_p(&home), T0, T1, &stats);
    assert_eq!(ss.len(), 1, "session survives a bad row");
    assert_eq!(stats.skipped_rows(), 1);
    assert!(!ss[0].spans.is_empty());
}

#[test]
fn q4_corrupt_prime_line_is_skipped_not_fatal() {
    let dir = prime_dir("q4_prime_line");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            "garbage line {",
            r#"{"type":"message","timestamp":"1970-01-01T00:01:41Z","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:50Z","message":{"role":"assistant","content":[{"type":"text","text":"yo"}]}}"#,
        ],
    );
    let stats = ScanStats::default();
    let ss = scan_stats(&sources(&dir), T0, T1, &stats);
    assert_eq!(ss.len(), 1, "file survives a bad line");
    assert!(!ss[0].spans.is_empty());
}

// ---------- Q5: kind classification ----------

#[test]
fn q5_injected_user_messages_do_not_make_a_session_human() {
    let home = hermes_home("q5_injected");
    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "s1",
            None,
            None,
            Some("tui"),
            100.0,
            200.0,
        )],
        &[
            // only "user" rows are harness-injected (platform_message_id set)
            fixtures::msg_row(1, "user", "injected", 100.0, Some("pm1"), None, None),
            fixtures::msg_row(2, "assistant", "a", 110.0, None, None, None),
            fixtures::msg_row(3, "user", "injected2", 120.0, Some("pm2"), None, None),
            fixtures::msg_row(4, "assistant", "b", 130.0, None, None, None),
        ],
    );
    let ss = scan_stats(&sources_p(&home), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    assert_eq!(ss[0].kind, SessionKind::Sub, "injected ≠ human input");
}

#[test]
fn q5_subagent_with_parent_is_sub() {
    let home = hermes_home("q5_parent");
    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "child",
            None,
            Some("parent123"),
            Some("tui"),
            100.0,
            200.0,
        )],
        &[
            fixtures::msg_row(1, "user", "task", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "ok", 110.0, None, None, None),
        ],
    );
    fixtures::msg_session_id(&home.join("state.db"), "child");
    let ss = scan_stats(&sources_p(&home), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    assert_eq!(ss[0].kind, SessionKind::Sub, "parent_session_id set → Sub");
}

#[test]
fn q5_cron_source_is_transparent_not_a_kind() {
    let home = hermes_home("q5_cron");
    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "c1",
            None,
            None,
            Some("cron"),
            100.0,
            200.0,
        )],
        &[
            fixtures::msg_row(1, "user", "job", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "done", 110.0, None, None, None),
        ],
    );
    fixtures::msg_session_id(&home.join("state.db"), "c1");
    let ss = scan_stats(&sources_p(&home), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    // cron is a source label, not a kind: session with a real user message is Human
    assert_eq!(ss[0].kind, SessionKind::Human);
    assert_eq!(ss[0].source.as_deref(), Some("cron"));
}

// ---------- Q6: preview truncation ----------

#[test]
fn q6_previews_are_capped_and_whitespace_collapsed() {
    // note: the content must be valid JSON — raw newlines escaped as \n
    let long: String = "a\\n b   c\\n\\n d".repeat(50);
    let dir = prime_dir("q6");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:40Z","message":{"role":"user","content":"first turn"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:41Z","message":{"role":"assistant","content":[{"type":"text","text":"short"}]}}"#,
            &format!(
                r#"{{"type":"message","timestamp":"1970-01-01T00:01:50Z","message":{{"role":"user","content":"{long}"}}}}"#
            ),
        ],
    );
    let ss = scan_stats(&sources(&dir), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    let idle = ss[0]
        .spans
        .iter()
        .find(|s| s.kind == SpanKind::Idle)
        .expect("idle before the long second user turn");
    let nu = idle.meta.as_ref().unwrap().next_user.as_ref().unwrap();
    assert_eq!(nu.chars().count(), PREVIEW_N);
    assert!(
        !nu.contains('\n') && !nu.contains("  "),
        "whitespace collapsed"
    );
}

// ---------- Q7: span derivation invariants ----------

#[test]
fn q7_user_then_assistant_is_inference() {
    let home = hermes_home("q7_inf");
    fixtures::wait_for_wal(&home.join("state.db"));
    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "s1",
            None,
            None,
            Some("tui"),
            100.0,
            200.0,
        )],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "a", 150.0, None, None, None),
        ],
    );
    let ss = scan_stats(&sources_p(&home), T0, T1, &ScanStats::default());
    let inf: Vec<_> = ss[0]
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::Inference)
        .collect();
    assert_eq!(inf.len(), 1);
    assert!((inf[0].t_start - 100.0).abs() < 1e-9 && (inf[0].t_end - 150.0).abs() < 1e-9);
}

#[test]
fn q7_tool_call_pairing_by_call_id() {
    let home = hermes_home("q7_tool");
    let tool_calls =
        r#"[{"function":{"name":"terminal","arguments":"{\"cmd\":\"ls\"}"},"call_id":"c1"}]"#;
    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "s1",
            None,
            None,
            Some("tui"),
            100.0,
            200.0,
        )],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            fixtures::msg_row(
                2,
                "assistant",
                "running",
                110.0,
                None,
                Some(tool_calls),
                None,
            ),
            fixtures::msg_row(3, "tool", "output text", 130.0, None, None, Some("c1")),
        ],
    );
    let ss = scan_stats(&sources_p(&home), T0, T1, &ScanStats::default());
    let tools: Vec<_> = ss[0]
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::Tool)
        .collect();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].label, "terminal");
    assert!(
        (tools[0].t_start - 110.0).abs() < 1e-9,
        "starts at assistant row"
    );
    assert!((tools[0].t_end - 130.0).abs() < 1e-9);
    let m = tools[0].meta.as_ref().unwrap();
    assert_eq!(m.args.as_deref(), Some(r#"{"cmd":"ls"}"#));
    assert_eq!(m.result.as_deref(), Some("output text"));
}

#[test]
fn q7_silence_over_gap_cap_is_idle_not_inference() {
    let home = hermes_home("q7_gap");
    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "s1",
            None,
            None,
            Some("tui"),
            100.0,
            200.0,
        )],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "a", 110.0, None, None, None),
            // 6-minute silence: > GAP_CAP_S (300)
            fixtures::msg_row(3, "user", "again", 470.0, None, None, None),
        ],
    );
    let ss = scan_stats(&sources_p(&home), T0, T1, &ScanStats::default());
    let s = &ss[0];
    assert_eq!(
        s.union_duration(SpanKind::Inference),
        10.0,
        "110-100 inference only; 470-110 silence is idle"
    );
    assert_eq!(s.union_duration(SpanKind::Idle), 360.0);
}

#[test]
fn q7_parallel_tools_are_timed_individually() {
    let home = hermes_home("q7_par");
    let tc_a = r#"[{"function":{"name":"alpha","arguments":"{}"},"call_id":"a"}]"#;
    let tc_b = r#"[{"function":{"name":"beta","arguments":"{}"},"call_id":"b"}]"#;
    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "s1",
            None,
            None,
            Some("tui"),
            100.0,
            200.0,
        )],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "go", 110.0, None, Some(tc_a), None),
            fixtures::msg_row(3, "tool", "ra", 130.0, None, None, Some("a")),
            fixtures::msg_row(4, "assistant", "go2", 112.0, None, Some(tc_b), None),
            fixtures::msg_row(5, "tool", "rb", 160.0, None, None, Some("b")),
        ],
    );
    let ss = scan_stats(&sources_p(&home), T0, T1, &ScanStats::default());
    let mut tools: Vec<_> = ss[0]
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::Tool)
        .collect();
    tools.sort_by_key(|s| s.t_start as u64);
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].label, "alpha");
    assert_eq!(tools[1].label, "beta");
    // union, not sum: [110,130] ∪ [112,160] = [110,160] = 50s
    assert_eq!(ss[0].union_duration(SpanKind::Tool), 50.0);
}

#[test]
fn q7_active0_rows_are_excluded() {
    let home = hermes_home("q7_active");
    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "s1",
            None,
            None,
            Some("tui"),
            100.0,
            200.0,
        )],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "dead-history", 150.0, None, None, None),
        ],
    );
    // mark the assistant row inactive (compaction dead history)
    fixtures::set_active(&home.join("state.db"), 2, 0);
    let ss = scan_stats(&sources_p(&home), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    assert!(
        ss[0]
            .spans
            .iter()
            .all(|s| s.kind != SpanKind::Inference || s.label != "model"),
        "no inference span from the dead-history row"
    );
    assert!(!ss[0]
        .spans
        .iter()
        .any(|s| s.meta.as_ref().and_then(|m| m.output.as_deref()) == Some("dead-history")));
}

#[test]
fn q7_extent_from_span_extremes_not_row_order() {
    // tool result rows can carry earlier timestamps than their assistant row
    // (out-of-order display_order after compaction): extent must come from
    // spans min/max, never from first/last row.
    let home = hermes_home("q7_extent");
    let tc = r#"[{"function":{"name":"t","arguments":"{}"},"call_id":"c"}]"#;
    fixtures::write_db(
        &home.join("state.db"),
        &[fixtures::sess_row(
            "s1",
            None,
            None,
            Some("tui"),
            100.0,
            999.0,
        )],
        &[
            fixtures::msg_row(1, "user", "q", 100.0, None, None, None),
            fixtures::msg_row(2, "assistant", "go", 120.0, None, Some(tc), None),
            fixtures::msg_row(3, "tool", "r", 130.0, None, None, Some("c")),
            // user row with an EARLIER timestamp than the tool result, but later
            // display_order: extent must still be 100..130
            fixtures::msg_row(4, "user", "later-display", 105.0, None, None, None),
        ],
    );
    let ss = scan_stats(&sources_p(&home), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    assert!(
        (ss[0].t_start - 100.0).abs() < 1e-9 && (ss[0].t_end - 130.0).abs() < 1e-9,
        "extent from spans, got [{}, {}]",
        ss[0].t_start,
        ss[0].t_end
    );
}

// ---------- Prime loader specifics ----------

#[test]
fn prime_tool_call_span_from_jsonl() {
    let dir = prime_dir("prime_tool");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:40Z","message":{"role":"user","content":"q"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:50Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"x1","name":"shell","arguments":{"cmd":"ls"}}]}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:02:10Z","message":{"role":"toolResult","toolCallId":"x1","toolName":"shell","content":[{"type":"text","text":"out"}]}}"#,
        ],
    );
    let ss = scan_stats(&sources(&dir), T0, T1, &ScanStats::default());
    let tools: Vec<_> = ss[0]
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::Tool)
        .collect();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].label, "shell");
    assert!((tools[0].t_start - 110.0).abs() < 1e-9);
    assert!((tools[0].t_end - 130.0).abs() < 1e-9);
}

#[test]
fn prime_empty_and_garbage_files_are_ignored() {
    let dir = prime_dir("prime_empty");
    write_jsonl(&dir, "empty.jsonl", &[]);
    write_jsonl(&dir, "trash.jsonl", &["{", "not json", ""]);
    let ss = scan_stats(&sources(&dir), T0, T1, &ScanStats::default());
    assert!(ss.is_empty());
}

// ---------- helpers ----------

fn sources(prime: &std::path::Path) -> Sources {
    Sources {
        hermes_home: std::path::PathBuf::from("/tmp/nonexistent-hermes"),
        prime_dir: Some(prime.to_path_buf()),
    }
}

fn sources_p(home: &std::path::Path) -> Sources {
    Sources {
        hermes_home: home.to_path_buf(),
        prime_dir: None,
    }
}
