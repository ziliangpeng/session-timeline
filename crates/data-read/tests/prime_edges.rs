//! Edge-case tests for the Prime loader: timestamp parsing variants, scan_dir
//! directory semantics, orphan toolResults, long silence, empty-span fallback,
//! malformed event rows. Synthetic fixtures only.

mod fixtures;

use data_read::loaders::prime::scan_dir;
use data_read::model::{SessionKind, SpanKind};
use data_read::{scan_stats, ScanStats, Sources};
use fixtures::{prime_dir, write_jsonl, T0, T1};

fn srcs(dir: &std::path::Path) -> Sources {
    Sources {
        hermes_home: std::path::PathBuf::from("/tmp/nonexistent-hermes"),
        prime_dir: Some(dir.to_path_buf()),
    }
}

// ---------- ts() variants (exercised through session file timestamps) ----------

#[test]
fn ts_fractional_seconds_and_utc_suffix() {
    // .123Z suffix and no-fraction both parse to the same second
    let dir = prime_dir("ts_frac");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40.123Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:40.123Z","message":{"role":"user","content":"q"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:45","message":{"role":"assistant","content":[{"type":"text","text":"a"}]}}"#,
        ],
    );
    let ss = scan_stats(&srcs(&dir), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    assert!(
        (ss[0].t_start - 100.123).abs() < 1e-6,
        "got {}",
        ss[0].t_start
    );
    assert!((ss[0].t_end - 105.0).abs() < 1e-6);
}

#[test]
fn ts_numeric_timezone_offset() {
    // +00:00 → same as Z; epoch unchanged
    let dir = prime_dir("ts_tz");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T01:01:40+01:00"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T01:01:40+01:00","message":{"role":"user","content":"q"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T01:01:45+01:00","message":{"role":"assistant","content":[{"type":"text","text":"a"}]}}"#,
        ],
    );
    let ss = scan_stats(&srcs(&dir), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    assert!(
        (ss[0].t_start - 100.0).abs() < 1e-6,
        "offset shifts epoch back: got {}",
        ss[0].t_start
    );
}

#[test]
fn ts_garbage_timestamp_skips_message_not_file() {
    let dir = prime_dir("ts_bad");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"type":"message","timestamp":"not a date","message":{"role":"user","content":"bad"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:41Z","message":{"role":"user","content":"good"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:50Z","message":{"role":"assistant","content":[{"type":"text","text":"a"}]}}"#,
        ],
    );
    let ss = scan_stats(&srcs(&dir), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1, "bad timestamp skips one message, not the file");
    // the bad row is gone: only one usable user turn remains, and since it is
    // the first usable row there is no idle span; the title proves it is "good"
    assert_eq!(ss[0].title.as_deref(), Some("good"));
    assert!(
        ss[0].spans.iter().all(|s| s.kind != SpanKind::Idle),
        "no idle: the usable user turn starts the session"
    );
    assert_eq!(ss[0].union_duration(SpanKind::Inference), 9.0, "101→110");
}

// ---------- scan_dir directory semantics ----------

#[test]
fn scan_dir_ignores_non_jsonl_and_missing_dir() {
    let dir = prime_dir("scan_dir");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:40Z","message":{"role":"user","content":"q"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:50Z","message":{"role":"assistant","content":[{"type":"text","text":"a"}]}}"#,
        ],
    );
    std::fs::write(dir.join("readme.txt"), "not a session").unwrap();
    std::fs::write(dir.join("s2.jsonl.bak"), "backup").unwrap();
    let out = scan_dir(&dir, T0, T1).expect("dir readable");
    assert_eq!(out.len(), 1, "only .jsonl files count");

    let missing = scan_dir(&dir.join("nope"), T0, T1);
    assert!(missing.is_err(), "missing dir → Err, not panic");
}

// ---------- orphan toolResult / pending toolCall ----------

#[test]
fn orphan_tool_result_uses_fallback_bounds() {
    let dir = prime_dir("orphan_tr");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:40Z","message":{"role":"user","content":"q"}}"#,
            // toolResult with no matching assistant toolCall
            r#"{"type":"message","timestamp":"1970-01-01T00:02:00Z","message":{"role":"toolResult","toolCallId":"ghost","toolName":"shell","content":[{"type":"text","text":"late"}]}}"#,
        ],
    );
    let ss = scan_stats(&srcs(&dir), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    let tools: Vec<_> = ss[0]
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::Tool)
        .collect();
    assert_eq!(tools.len(), 1, "orphan result still produces a span");
    assert_eq!(tools[0].label, "shell");
    assert_eq!(
        tools[0].meta.as_ref().unwrap().result.as_deref(),
        Some("late")
    );
}

#[test]
fn unmatched_pending_tool_call_produces_no_tool_span() {
    let dir = prime_dir("pending_tc");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:40Z","message":{"role":"user","content":"q"}}"#,
            // assistant announces a toolCall that never returns
            r#"{"type":"message","timestamp":"1970-01-01T00:01:41Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"nope","name":"web","arguments":{"q":"x"}}]}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:50Z","message":{"role":"user","content":"next"}}"#,
        ],
    );
    let ss = scan_stats(&srcs(&dir), T0, T1, &ScanStats::default());
    assert!(
        ss[0].spans.iter().all(|s| s.kind != SpanKind::Tool),
        "un-answered toolCall yields no tool span"
    );
}

// ---------- long silence ----------

#[test]
fn prime_long_silence_is_idle_not_inference() {
    let dir = prime_dir("long_silence");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:40Z","message":{"role":"user","content":"q"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:50Z","message":{"role":"assistant","content":[{"type":"text","text":"a"}]}}"#,
            // 10-minute silence before the next assistant row
            r#"{"type":"message","timestamp":"1970-01-01T00:11:50Z","message":{"role":"assistant","content":[{"type":"text","text":"b"}]}}"#,
        ],
    );
    let ss = scan_stats(&srcs(&dir), T0, T1, &ScanStats::default());
    let s = &ss[0];
    let long_idles: Vec<_> = s
        .spans
        .iter()
        .filter(|x| x.kind == SpanKind::Idle && x.label.contains("long silence"))
        .collect();
    assert_eq!(
        long_idles.len(),
        1,
        ">GAP_CAP silence → idle (long silence)"
    );
    assert!((long_idles[0].t_end - long_idles[0].t_start - 600.0).abs() < 1e-6);
}

// ---------- empty-spans fallback / kind ----------

#[test]
fn prime_user_only_session_has_empty_spans_and_human_kind() {
    let dir = prime_dir("user_only");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:40Z","message":{"role":"user","content":"just asking"}}"#,
        ],
    );
    let ss = scan_stats(&srcs(&dir), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    assert_eq!(ss[0].kind, SessionKind::Human);
    assert!(ss[0].spans.is_empty(), "no span pairs → empty spans");
    // extent falls back to message timestamps
    assert!((ss[0].t_start - 100.0).abs() < 1e-9);
    assert!((ss[0].t_end - 100.0).abs() < 1e-9);
    // title falls back to first user message
    assert_eq!(ss[0].title.as_deref(), Some("just asking"));
}

#[test]
fn prime_assistant_only_session_is_sub_kind() {
    let dir = prime_dir("assistant_only");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:40Z","message":{"role":"assistant","content":[{"type":"text","text":"autonomous"}]}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:45Z","message":{"role":"assistant","content":[{"type":"text","text":"still going"}]}}"#,
        ],
    );
    let ss = scan_stats(&srcs(&dir), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1);
    assert_eq!(ss[0].kind, SessionKind::Sub, "zero user rows → sub");
    assert_eq!(ss[0].title, None, "no user message → no title");
    assert_eq!(ss[0].union_duration(SpanKind::Inference), 5.0);
}

// ---------- malformed event rows ----------

#[test]
fn prime_events_missing_message_or_timestamp_are_skipped() {
    let dir = prime_dir("malformed_ev");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"timestamp":"1970-01-01T00:01:41Z","message":{"role":"user","content":"no type field"}}"#,
            r#"{"type":"message","message":{"role":"user","content":"no timestamp"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:42Z","message":{"role":"user","content":"ok"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:50Z","message":{"role":"assistant","content":[{"type":"text","text":"a"}]}}"#,
        ],
    );
    let ss = scan_stats(&srcs(&dir), T0, T1, &ScanStats::default());
    assert_eq!(ss.len(), 1, "file survives malformed events");
    // only the well-formed "ok" user row is usable → it becomes the title
    assert_eq!(ss[0].title.as_deref(), Some("ok"));
    assert_eq!(ss[0].union_duration(SpanKind::Inference), 8.0, "102→110");
}

#[test]
fn prime_toolcall_with_object_arguments_serialized() {
    // arguments may arrive as an object (not a string): must serialize for preview
    let dir = prime_dir("tc_obj_args");
    write_jsonl(
        &dir,
        "s1.jsonl",
        &[
            r#"{"type":"session","timestamp":"1970-01-01T00:01:40Z"}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:40Z","message":{"role":"user","content":"q"}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:41Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"o1","name":"search","arguments":{"query":"hello"}}]}}"#,
            r#"{"type":"message","timestamp":"1970-01-01T00:01:45Z","message":{"role":"toolResult","toolCallId":"o1","toolName":"search","content":[{"type":"text","text":"found"}]}}"#,
        ],
    );
    let ss = scan_stats(&srcs(&dir), T0, T1, &ScanStats::default());
    let tool = ss[0]
        .spans
        .iter()
        .find(|s| s.kind == SpanKind::Tool)
        .expect("tool span");
    let args = tool.meta.as_ref().unwrap().args.as_deref().unwrap();
    assert!(args.contains("query"), "object args serialized: {args}");
    assert!(args.contains("hello"));
}
