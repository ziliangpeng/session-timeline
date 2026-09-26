//! Hermes loader: per-profile SQLite `state.db` → unified sessions.
//!
//! Invariants carried from the validated prototype (each locked by a test):
//!   1. active=1 rows only (compaction dead history excluded)
//!   2. injected user rows (platform_message_id set) never count as human input
//!   3. malformed tool_calls payloads skip the row, never the profile
//!   4. session extent from span/message timestamps, never row order
//!   5. silence above GAP_CAP_S is idle, never inference
//!   6. a broken DB is skipped with a warning, never a panic

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::model::{Meta, Session, SessionKind, Span, SpanKind, GAP_CAP_S, PREVIEW_N};

/// Discover profile DBs under a Hermes home: `<home>/state.db` (profile
/// "default") plus `<home>/profiles/*/state.db`. Missing files are skipped so
/// the same code runs on any machine.
pub fn discover_dbs(home: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let top = home.join("state.db");
    if top.is_file() {
        out.push(("default".to_string(), top));
    }
    let profiles = home.join("profiles");
    if let Ok(entries) = std::fs::read_dir(&profiles) {
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.join("state.db").is_file())
            .collect();
        dirs.sort();
        for p in dirs {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                out.push((name.to_string(), p.join("state.db")));
            }
        }
    }
    out
}

fn preview(s: &str) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    truncated(&collapsed)
}

fn truncated(s: &str) -> String {
    if s.chars().count() <= PREVIEW_N {
        s.to_string()
    } else {
        s.chars().take(PREVIEW_N).collect()
    }
}

/// Load all sessions overlapping `[t0, t1)` from one Hermes profile DB.
/// Degradation is per Q4: source-level open failure → Err + counted skip;
/// session-level and row-level failures are counted and skipped, never fatal.
pub fn load_profile_db(
    db_path: &Path,
    profile: &str,
    t0: f64,
    t1: f64,
    stats: &crate::ScanStats,
) -> Result<Vec<Session>, String> {
    let conn = match Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(e) => {
            stats.inc_skipped_sources();
            return Err(format!("open {}: {e}", db_path.display()));
        }
    };

    let mut sess_stmt = match conn.prepare(
        "SELECT id, title, parent_session_id, source FROM sessions
         WHERE archived=0 AND started_at IS NOT NULL
           AND started_at < ?1 AND COALESCE(ended_at, last_activity_at, started_at) > ?2
         ORDER BY started_at",
    ) {
        Ok(s) => s,
        Err(e) => {
            stats.inc_skipped_sources();
            return Err(format!("prepare sessions: {e}"));
        }
    };

    type SessRow = (String, Option<String>, Option<String>, Option<String>);
    let rows: Vec<SessRow> = sess_stmt
        .query_map([t1, t0], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(|e| format!("query sessions: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    let mut out = Vec::new();
    for (sid, title, parent, source) in rows {
        let mut msg_stmt = match conn.prepare(
            "SELECT role, tool_name, tool_call_id, tool_calls, content, timestamp, platform_message_id
             FROM messages WHERE session_id=?1 AND active=1
             ORDER BY COALESCE(display_order, id)",
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[warn] {profile}/{sid}: messages query failed: {e}");
                stats.inc_skipped_sessions();
                continue;
            }
        };
        let msgs: Vec<MsgRow> = match msg_stmt.query_map([&sid], |r| {
            Ok(MsgRow {
                role: r.get(0)?,
                tool_name: r.get(1)?,
                tool_call_id: r.get(2)?,
                tool_calls: r.get(3)?,
                content: r.get(4)?,
                ts: r.get(5)?,
                pmid: r.get(6)?,
            })
        }) {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => {
                eprintln!("[warn] {profile}/{sid}: messages read failed: {e}");
                continue;
            }
        };
        if msgs.is_empty() {
            continue;
        }
        if let Some(sess) = build_session(profile, &sid, title, parent, source, &msgs, stats) {
            out.push(sess);
        }
    }
    Ok(out)
}

struct MsgRow {
    role: String,
    tool_name: Option<String>,
    tool_call_id: Option<String>,
    tool_calls: Option<String>,
    content: Option<String>,
    ts: f64,
    pmid: Option<String>,
}

fn build_session(
    profile: &str,
    sid: &str,
    title: Option<String>,
    parent: Option<String>,
    source: Option<String>,
    msgs: &[MsgRow],
    stats: &crate::ScanStats,
) -> Option<Session> {
    let human_msgs = msgs
        .iter()
        .filter(|m| m.role == "user" && m.pmid.is_none())
        .count();
    let kind = if parent.is_some() || human_msgs == 0 {
        SessionKind::Sub
    } else {
        SessionKind::Human
    };

    let mut spans: Vec<Span> = Vec::new();
    let mut pending: HashMap<String, (f64, String, String)> = HashMap::new();
    let mut last: Option<f64> = None;

    for m in msgs {
        match m.role.as_str() {
            "user" => {
                if let Some(prev) = last {
                    if m.ts > prev {
                        let meta = if m.pmid.is_none() {
                            Meta {
                                next_user: Some(preview(m.content.as_deref().unwrap_or(""))),
                                ..Default::default()
                            }
                        } else {
                            Meta {
                                note: Some("ended by injected message".into()),
                                ..Default::default()
                            }
                        };
                        spans.push(Span {
                            kind: SpanKind::Idle,
                            t_start: prev,
                            t_end: m.ts,
                            label: "idle".into(),
                            meta: Some(meta),
                        });
                    }
                }
                pending.clear();
                last = Some(m.ts);
            }
            "assistant" => {
                if let Some(prev) = last {
                    if m.ts > prev {
                        let gap = m.ts - prev;
                        let (kind, label) = if gap <= GAP_CAP_S {
                            (SpanKind::Inference, "model")
                        } else {
                            (SpanKind::Idle, "idle (long silence)")
                        };
                        spans.push(Span {
                            kind,
                            t_start: prev,
                            t_end: m.ts,
                            label: label.into(),
                            meta: Some(Meta {
                                output: Some(preview(m.content.as_deref().unwrap_or(""))),
                                ..Default::default()
                            }),
                        });
                    }
                }
                last = Some(m.ts);
                if let Some(tc) = &m.tool_calls {
                    if let Ok(list) = serde_json::from_str::<serde_json::Value>(tc) {
                        if let Some(arr) = list.as_array() {
                            for item in arr {
                                let name = item
                                    .get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("?");
                                let args = item
                                    .get("function")
                                    .and_then(|f| f.get("arguments"))
                                    .and_then(|a| a.as_str())
                                    .unwrap_or("");
                                let cid = item
                                    .get("call_id")
                                    .or_else(|| item.get("id"))
                                    .and_then(|i| i.as_str())
                                    .unwrap_or("");
                                pending.insert(
                                    cid.to_string(),
                                    (m.ts, name.to_string(), args.to_string()),
                                );
                            }
                        }
                    } else {
                        stats.inc_skipped_rows();
                    }
                }
            }
            "tool" => {
                let cid = m.tool_call_id.clone().unwrap_or_default();
                let (start, name, args) = match pending.remove(&cid) {
                    Some(v) => v,
                    None => (
                        last.unwrap_or(m.ts),
                        m.tool_name.clone().unwrap_or_else(|| "tool".into()),
                        String::new(),
                    ),
                };
                if m.ts >= start {
                    spans.push(Span {
                        kind: SpanKind::Tool,
                        t_start: start,
                        t_end: m.ts,
                        label: name,
                        meta: Some(Meta {
                            args: if args.is_empty() {
                                None
                            } else {
                                Some(preview(&args))
                            },
                            result: Some(preview(m.content.as_deref().unwrap_or(""))),
                            ..Default::default()
                        }),
                    });
                }
                last = Some(last.map_or(m.ts, |l| l.max(m.ts)));
            }
            _ => {}
        }
    }

    let (t_start, t_end) = if !spans.is_empty() {
        (
            spans
                .iter()
                .map(|s| s.t_start)
                .fold(f64::INFINITY, f64::min),
            spans
                .iter()
                .map(|s| s.t_end)
                .fold(f64::NEG_INFINITY, f64::max),
        )
    } else {
        (
            msgs.iter().map(|m| m.ts).fold(f64::INFINITY, f64::min),
            msgs.iter().map(|m| m.ts).fold(f64::NEG_INFINITY, f64::max),
        )
    };
    if t_end < t_start {
        return None;
    }

    let title = match title {
        Some(t) if !t.is_empty() => Some(truncated(&preview(&t))),
        _ => msgs
            .iter()
            .find(|m| m.role == "user")
            .map(|m| preview(m.content.as_deref().unwrap_or("")))
            .filter(|s| !s.is_empty()),
    };

    Some(Session {
        id: format!("hermes:{profile}:{sid}"),
        kind,
        t_start,
        t_end,
        title,
        profile: Some(profile.to_string()),
        source: source.filter(|s| !s.is_empty()),
        spans,
    })
}

/// Scan all discovered profile DBs in parallel (rayon), sessions overlapping
/// `[t0, t1)`. Broken DBs are skipped with a warning.
pub fn scan_all(home: &Path, t0: f64, t1: f64) -> Vec<Session> {
    scan_all_stats(home, t0, t1, &crate::ScanStats::default())
}

/// Same, with a shared stats collector (skipped sources/sessions/rows).
pub fn scan_all_stats(home: &Path, t0: f64, t1: f64, stats: &crate::ScanStats) -> Vec<Session> {
    use rayon::prelude::*;
    let dbs = discover_dbs(home);
    dbs.par_iter()
        .map(
            |(profile, path)| match load_profile_db(path, profile, t0, t1, stats) {
                Ok(sessions) => sessions,
                Err(e) => {
                    eprintln!("[warn] skip profile {profile}: {e}");
                    Vec::new()
                }
            },
        )
        .flatten()
        .collect()
}

/// Load ONE session (full spans) by its raw session id from a profile DB,
/// ignoring any time window. Returns None when the id is unknown.
pub fn load_session_by_id(home: &Path, profile: &str, sid: &str) -> Option<Session> {
    // Profile identity comes from the CALLER (same source of truth as the
    // scan path: discover_profile_dbs); never guessed from the db path.
    let db_path = if profile == "default" {
        home.join("state.db")
    } else {
        home.join("profiles").join(profile).join("state.db")
    };

    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let row: Option<(Option<String>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT title, parent_session_id, source FROM sessions WHERE id = ?1 AND archived = 0",
            [sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let (title, parent, source) = row?;
    let mut stmt = conn
        .prepare(
            "SELECT role, tool_name, tool_call_id, tool_calls, content, timestamp, platform_message_id
             FROM messages WHERE session_id = ?1 AND active = 1
             ORDER BY COALESCE(display_order, id)",
        )
        .ok()?;
    let msgs: Vec<MsgRow> = stmt
        .query_map([sid], |r| {
            Ok(MsgRow {
                role: r.get(0)?,
                tool_name: r.get(1)?,
                tool_call_id: r.get(2)?,
                tool_calls: r.get(3)?,
                content: r.get(4)?,
                ts: r.get(5)?,
                pmid: r.get(6)?,
            })
        })
        .ok()?
        .filter_map(|r| r.ok())
        .collect();
    if msgs.is_empty() {
        return None;
    }
    build_session(
        profile,
        sid,
        title,
        parent,
        source,
        &msgs,
        &crate::ScanStats::default(),
    )
}
