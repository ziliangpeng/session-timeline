//! Prime autonomy-attribution tests: spans driven by heartbeats (scheduled
//! prompts) and refinements (self-tuning passes) carry `meta.driver`, so the
//! UI can recolor/hide agent-self activity vs user-driven turns. Synthetic
//! JSONL fixtures only.

use std::io::Write;

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("prime-drv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_session(dir: &std::path::Path, lines: &[String]) -> std::path::PathBuf {
    let f = dir.join("s1.jsonl");
    let mut fh = std::fs::File::create(&f).unwrap();
    for l in lines {
        writeln!(fh, "{l}").unwrap();
    }
    fh.flush().unwrap();
    f
}

fn msg(role: &str, text: &str, ts: &str) -> String {
    format!(
        r#"{{"type":"message","timestamp":"{ts}","message":{{"role":"{role}","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

fn tool_call(name: &str, id: &str, ts: &str) -> String {
    format!(
        r#"{{"type":"message","timestamp":"{ts}","message":{{"role":"assistant","content":[{{"type":"toolCall","id":"{id}","name":"{name}","arguments":"{{}}"}}]}}}}"#
    )
}

fn tool_result(id: &str, ts: &str) -> String {
    format!(
        r#"{{"type":"message","timestamp":"{ts}","message":{{"role":"toolResult","toolCallId":"{id}","content":[{{"type":"text","text":"ok"}}]}}}}"#
    )
}

fn heartbeat(ts: &str) -> String {
    format!(
        r#"{{"type":"custom_message","customType":"heartbeat_prompt","timestamp":"{ts}","content":"beat: check the thing"}}"#
    )
}

fn refinement(ts: &str) -> String {
    format!(
        r#"{{"type":"custom","customType":"prime-agent.refinement","timestamp":"{ts}","data":{{"id":"refine_1"}}}}"#
    )
}

/// user turn → tool+inference spans have NO driver; a later heartbeat burst
/// (no user message) → its spans carry driver="heartbeat".
#[test]
fn heartbeat_spans_tagged_user_spans_untagged() {
    let dir = tmpdir("hb");
    let f = write_session(
        &dir,
        &[
            msg("user", "hi there", "2026-01-01T10:00:00Z"),
            tool_call("ipython", "c1", "2026-01-01T10:00:10Z"),
            tool_result("c1", "2026-01-01T10:00:20Z"),
            msg("assistant", "done", "2026-01-01T10:00:30Z"),
            // 2h later: heartbeat fires with NO user message
            heartbeat("2026-01-01T12:00:00Z"),
            tool_call("ipython", "c2", "2026-01-01T12:00:05Z"),
            tool_result("c2", "2026-01-01T12:00:15Z"),
            msg("assistant", "beat ok", "2026-01-01T12:00:25Z"),
        ],
    );
    let s = data_read::loaders::prime::load_file_pub(&f, f64::NEG_INFINITY, f64::INFINITY)
        .expect("session loads");
    let mut user_tools = 0;
    let mut hb_tools = 0;
    let mut hb_inf = 0;
    let mut user_inf = 0;
    for sp in &s.spans {
        let driver = sp.meta.as_ref().and_then(|m| m.driver.as_deref());
        match (sp.kind, driver) {
            (data_read::model::SpanKind::Tool, Some("heartbeat")) => hb_tools += 1,
            (data_read::model::SpanKind::Tool, None) => user_tools += 1,
            (data_read::model::SpanKind::Inference, Some("heartbeat")) => hb_inf += 1,
            (data_read::model::SpanKind::Inference, None) => user_inf += 1,
            _ => {}
        }
    }
    assert_eq!(user_tools, 1, "user-driven tool span untagged");
    assert_eq!(hb_tools, 1, "heartbeat tool span tagged");
    assert!(user_inf >= 1, "user inference present");
    assert!(hb_inf >= 1, "heartbeat inference tagged");
    // user-arm idle (waiting for the user) must NOT be tagged — hiding autobeats
    // must not erase real waiting-for-user time
    let idle_tagged = s
        .spans
        .iter()
        .any(|sp| sp.kind == data_read::model::SpanKind::Idle
            && sp.meta.as_ref().and_then(|m| m.driver.as_deref()).is_some());
    assert!(!idle_tagged, "user-arm idle spans never carry a driver");
}

/// after the user returns, spans go back to untagged even though a heartbeat
/// marker exists earlier in the file.
#[test]
fn user_return_clears_heartbeat_attribution() {
    let dir = tmpdir("ret");
    let f = write_session(
        &dir,
        &[
            msg("user", "start", "2026-01-01T09:00:00Z"),
            msg("assistant", "ok", "2026-01-01T09:00:05Z"),
            heartbeat("2026-01-01T10:00:00Z"),
            tool_call("ipython", "b1", "2026-01-01T10:00:05Z"),
            tool_result("b1", "2026-01-01T10:00:15Z"),
            msg("assistant", "beat done", "2026-01-01T10:00:25Z"),
            // user comes back 3h later
            msg("user", "back again", "2026-01-01T13:00:00Z"),
            tool_call("ipython", "c1", "2026-01-01T13:00:05Z"),
            tool_result("c1", "2026-01-01T13:00:15Z"),
            msg("assistant", "welcome back", "2026-01-01T13:00:25Z"),
        ],
    );
    let s = data_read::loaders::prime::load_file_pub(&f, f64::NEG_INFINITY, f64::INFINITY)
        .expect("session loads");
    // spans after 13:00 must be untagged
    for sp in &s.spans {
        if sp.t_start >= 1767272399.0 {
            let d = sp.meta.as_ref().and_then(|m| m.driver.as_deref());
            assert_ne!(d, Some("heartbeat"), "post-user-return span still tagged: {sp:?}");
        }
    }
    // and the 10:00 burst IS tagged
    let hb = s
        .spans
        .iter()
        .filter(|sp| sp.meta.as_ref().and_then(|m| m.driver.as_deref()) == Some("heartbeat"))
        .count();
    assert!(hb >= 2, "heartbeat burst tagged: {hb}");
}

/// refinement events (custom type) tag only nearby spans (±300s), not the
/// whole rest of the session.
#[test]
fn refinement_tags_only_nearby_spans() {
    let dir = tmpdir("ref");
    let f = write_session(
        &dir,
        &[
            msg("user", "do work", "2026-01-01T10:00:00Z"),
            tool_call("ipython", "w1", "2026-01-01T10:00:05Z"),
            tool_result("w1", "2026-01-01T10:00:15Z"),
            msg("assistant", "work done", "2026-01-01T10:00:30Z"),
            // refinement 4 minutes later tags only spans within 300s after it
            refinement("2026-01-01T10:04:00Z"),
            msg("user", "next thing", "2026-01-01T15:00:00Z"),
            tool_call("ipython", "w2", "2026-01-01T15:00:05Z"),
            tool_result("w2", "2026-01-01T15:00:15Z"),
            msg("assistant", "done 2", "2026-01-01T15:00:30Z"),
        ],
    );
    let s = data_read::loaders::prime::load_file_pub(&f, f64::NEG_INFINITY, f64::INFINITY)
        .expect("session loads");
    let ref_spans = s
        .spans
        .iter()
        .filter(|sp| sp.meta.as_ref().and_then(|m| m.driver.as_deref()) == Some("refinement"))
        .count();
    // the 15:00 turn must NOT be refinement-tagged (hours after the marker)
    for sp in &s.spans {
        if sp.t_start >= 1767279599.0 {
            let d = sp.meta.as_ref().and_then(|m| m.driver.as_deref());
            assert_ne!(d, Some("refinement"), "far-from-marker span tagged: {sp:?}");
        }
    }
    // nothing in this fixture is within 300s after the marker with a span start,
    // so zero or few refinement tags is correct; assert no heartbeat tags at all
    let hb = s
        .spans
        .iter()
        .filter(|sp| sp.meta.as_ref().and_then(|m| m.driver.as_deref()) == Some("heartbeat"))
        .count();
    assert_eq!(hb, 0);
    assert!(ref_spans <= 2, "refinement tagging stays local: {ref_spans}");
}


/// assistant-arm long-silence gaps between beats (no user message between
/// them) ARE tagged heartbeat — toggling autobeats off hides the entire
/// autonomous block, internal waiting included.
#[test]
fn long_silence_between_beats_is_tagged() {
    let dir = tmpdir("gap");
    let f = write_session(
        &dir,
        &[
            msg("user", "start", "2026-01-01T09:00:00Z"),
            msg("assistant", "ok", "2026-01-01T09:00:05Z"),
            heartbeat("2026-01-01T10:00:00Z"),
            tool_call("ipython", "b1", "2026-01-01T10:00:05Z"),
            tool_result("b1", "2026-01-01T10:00:15Z"),
            msg("assistant", "beat 1 ok", "2026-01-01T10:00:25Z"),
            // next beat 90 min later — the gap is assistant-arm long silence
            heartbeat("2026-01-01T11:30:00Z"),
            tool_call("ipython", "b2", "2026-01-01T11:30:05Z"),
            tool_result("b2", "2026-01-01T11:30:15Z"),
            msg("assistant", "beat 2 ok", "2026-01-01T11:30:25Z"),
        ],
    );
    let s = data_read::loaders::prime::load_file_pub(&f, f64::NEG_INFINITY, f64::INFINITY)
        .expect("session loads");
    let mut tagged_idle = 0;
    for sp in &s.spans {
        if sp.kind == data_read::model::SpanKind::Idle {
            let d = sp.meta.as_ref().and_then(|m| m.driver.as_deref());
            if d == Some("heartbeat") {
                tagged_idle += 1;
            }
        }
    }
    assert!(
        tagged_idle >= 1,
        "long-silence idle between beats carries driver=heartbeat"
    );
}
