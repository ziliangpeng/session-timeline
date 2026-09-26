//! Prime Agent loader: per-session append-only JSONL → unified sessions.
//!
//! Event model: rows of `{type: "message", message: {role, content, …},
//! timestamp: ISO-8601}`; tool calls appear as assistant content parts of type
//! `toolCall` and results as `toolResult` rows keyed by `toolCallId`.

use std::path::Path;

use crate::model::{Meta, Session, SessionKind, Span, SpanKind, GAP_CAP_S};

fn ts(iso: &str) -> Option<f64> {
    // minimal ISO-8601 → epoch for the shapes Prime writes:
    // 2026-09-24T11:30:39.123Z and 2026-09-24T11:30:39.123456+00:00
    let s = iso.trim();
    let (date, rest) = s.split_once('T')?;
    let rest = rest.strip_suffix('Z').unwrap_or(rest);
    let (time, frac_off) = {
        let cut = rest.find(['+', '-']).unwrap_or(rest.len());
        // keep only a leading '+'/'-' that is clearly a timezone (has ':' later)
        let (t, off) = rest.split_at(cut);
        let off = if off.starts_with('+') || (off.starts_with('-') && off.contains(':')) {
            off
        } else {
            ""
        };
        (t, off)
    };
    let mut parts = time.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let (sec, frac): (f64, f64) = match parts.next() {
        Some(s) => {
            if let Some((i, f)) = s.split_once('.') {
                (i.parse().ok()?, format!("0.{f}").parse().ok()?)
            } else {
                (s.parse().ok()?, 0.0)
            }
        }
        None => (0.0, 0.0),
    };
    let mut d = date.split('-');
    let y: i32 = d.next()?.parse().ok()?;
    let mo: i32 = d.next()?.parse().ok()?;
    let day: i32 = d.next()?.parse().ok()?;
    // days from civil era (Howard Hinnant's algorithm)
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let mp = ((mo + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + (day as i64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era as i64 * 146097 + doe - 719468;
    let epoch = days as f64 * 86400.0 + h * 3600.0 + m * 60.0 + sec + frac;
    // timezone offset: ±HH:MM
    let off = if frac_off.is_empty() {
        0.0
    } else {
        let sign = if frac_off.starts_with('-') { -1.0 } else { 1.0 };
        let mut p = frac_off[1..].split(':');
        let oh: f64 = p.next().unwrap_or("0").parse().unwrap_or(0.0);
        let om: f64 = p.next().unwrap_or("0").parse().unwrap_or(0.0);
        sign * (oh * 3600.0 + om * 60.0)
    };
    Some(epoch - off)
}

fn preview(s: &str) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > crate::model::PREVIEW_N {
        collapsed.chars().take(crate::model::PREVIEW_N).collect()
    } else {
        collapsed
    }
}

fn ptext(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Load all sessions overlapping `[t0, t1)` from a directory of per-session
/// `*.jsonl` files. Files too old (mtime) are skipped cheaply.
pub fn scan_dir(dir: &Path, t0: f64, t1: f64) -> Result<Vec<Session>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    let mut files: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        // cheap mtime prefilter: file untouched before window start minus a day
        if let Ok(meta) = std::fs::metadata(&f) {
            if let Ok(mtime) = meta.modified() {
                let m = mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                if m < t0 - 86400.0 {
                    continue;
                }
            }
        }
        if let Some(sess) = load_file(&f, t0, t1) {
            out.push(sess);
        }
    }
    Ok(out)
}

/// Load one Prime session file if it overlaps `[t0, t1)`.
/// Public entry for parallel callers; one malformed line never kills the file.
pub fn load_file_pub(f: &Path, t0: f64, t1: f64) -> Option<Session> {
    load_file(f, t0, t1)
}

/// Cheap probe: read only the first line; returns the first event's timestamp.
fn first_line_timestamp(f: &Path) -> Option<f64> {
    use std::io::{BufRead, Read};
    let file = std::fs::File::open(f).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut first = String::new();
    // first line is the small session header; cap the read defensively
    let _ = reader.by_ref().take(4096).read_line(&mut first);
    let ev = serde_json::from_str::<serde_json::Value>(first.trim()).ok()?;
    let iso = ev.get("timestamp").and_then(|t| t.as_str())?;
    ts(iso)
}

/// Cheap probe: seek near EOF and read the last complete line's timestamp.
/// Append-only files make this the session's last event time.
fn last_line_timestamp(f: &Path) -> Option<f64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(f).ok()?;
    let len = file.metadata().ok()?.len();
    if len == 0 {
        return None;
    }
    let window = 8192.min(len);
    file.seek(SeekFrom::End(-(window as i64))).ok()?;
    let mut buf = String::new();
    file.by_ref().take(window).read_to_string(&mut buf).ok()?;
    // last complete line: drop a possible partial line at the start of buf
    let start = if len > window {
        buf.find('\n').map(|i| i + 1).unwrap_or(0)
    } else {
        0
    };
    let tail = &buf[start..];
    let last_line = tail.lines().last()?.trim();
    let ev = serde_json::from_str::<serde_json::Value>(last_line).ok()?;
    let iso = ev.get("timestamp").and_then(|t| t.as_str())?;
    ts(iso)
}

fn load_file(f: &Path, t0: f64, t1: f64) -> Option<Session> {
    use std::io::BufRead;
    // cheap window probes before any full read (append-only files):
    //   first event after window end  → cannot overlap, skip
    //   last event before window start → cannot overlap, skip
    //   anything else → the file MIGHT overlap (long session), must read
    if let Some(start) = first_line_timestamp(f) {
        if start > t1 {
            return None;
        }
    }
    if let Some(end) = last_line_timestamp(f) {
        if end < t0 {
            return None;
        }
    }
    let file = std::fs::File::open(f).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut msgs: Vec<(String, f64, serde_json::Value)> = Vec::new(); // (role, ts, message)
    for rdr in reader.lines() {
        let ln = rdr.unwrap_or_default();
        let ev = match serde_json::from_str::<serde_json::Value>(&ln) {
            Ok(v) => v,
            Err(_) => continue, // invariant: one malformed line never kills the file
        };
        if ev.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let Some(m) = ev.get("message") else { continue };
        let Some(iso) = ev.get("timestamp").and_then(|t| t.as_str()) else {
            continue;
        };
        let Some(t) = ts(iso) else { continue };
        let role = m
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();
        msgs.push((role, t, m.clone()));
    }
    if msgs.is_empty() {
        return None;
    }
    let first_ts = msgs.first().map(|(_, t, _)| *t).unwrap();
    let last_ts = msgs.last().map(|(_, t, _)| *t).unwrap();
    if last_ts < t0 || first_ts > t1 {
        return None;
    }
    // second cheap filter: last message before window start → skip after read
    // (we already paid the read; this only avoids span construction)

    let human_msgs = msgs.iter().filter(|(r, _, _)| r == "user").count();
    let kind = if human_msgs == 0 {
        SessionKind::Sub
    } else {
        SessionKind::Human
    };

    let mut spans: Vec<Span> = Vec::new();
    let mut pending: std::collections::HashMap<String, (f64, String, String)> =
        std::collections::HashMap::new();
    let mut last: Option<f64> = None;

    for (role, t, m) in &msgs {
        match role.as_str() {
            "user" => {
                if let Some(prev) = last {
                    if *t > prev {
                        spans.push(Span {
                            kind: SpanKind::Idle,
                            t_start: prev,
                            t_end: *t,
                            label: "idle".into(),
                            meta: Some(Meta {
                                next_user: Some(preview(&ptext(
                                    m.get("content").unwrap_or(&serde_json::Value::Null),
                                ))),
                                ..Default::default()
                            }),
                        });
                    }
                }
                pending.clear();
                last = Some(*t);
            }
            "assistant" => {
                if let Some(prev) = last {
                    if *t > prev {
                        let gap = *t - prev;
                        let (kind, label) = if gap <= GAP_CAP_S {
                            (SpanKind::Inference, "model")
                        } else {
                            (SpanKind::Idle, "idle (long silence)")
                        };
                        spans.push(Span {
                            kind,
                            t_start: prev,
                            t_end: *t,
                            label: label.into(),
                            meta: Some(Meta {
                                output: Some(preview(&ptext(
                                    m.get("content").unwrap_or(&serde_json::Value::Null),
                                ))),
                                ..Default::default()
                            }),
                        });
                    }
                }
                last = Some(*t);
                if let Some(serde_json::Value::Array(parts)) = m.get("content") {
                    for p in parts {
                        if p.get("type").and_then(|t| t.as_str()) == Some("toolCall") {
                            let id = p.get("id").and_then(|i| i.as_str()).unwrap_or("");
                            let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                            let args = match p.get("arguments") {
                                Some(serde_json::Value::String(s)) => s.clone(),
                                Some(v) => v.to_string(),
                                None => String::new(),
                            };
                            pending.insert(id.to_string(), (*t, name.to_string(), args));
                        }
                    }
                }
            }
            "toolResult" => {
                let cid = m
                    .get("toolCallId")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                let (start, name, args) = match pending.remove(&cid) {
                    Some(v) => v,
                    None => (
                        last.unwrap_or(*t),
                        m.get("toolName")
                            .and_then(|n| n.as_str())
                            .unwrap_or("tool")
                            .to_string(),
                        String::new(),
                    ),
                };
                if *t >= start {
                    spans.push(Span {
                        kind: SpanKind::Tool,
                        t_start: start,
                        t_end: *t,
                        label: name,
                        meta: Some(Meta {
                            args: if args.is_empty() {
                                None
                            } else {
                                Some(preview(&args))
                            },
                            result: Some(preview(&ptext(
                                m.get("content").unwrap_or(&serde_json::Value::Null),
                            ))),
                            ..Default::default()
                        }),
                    });
                }
                last = Some(last.map_or(*t, |l| l.max(*t)));
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
        (first_ts, last_ts)
    };
    if t_end < t_start {
        return None;
    }

    let title = msgs
        .iter()
        .find(|(r, _, _)| r == "user")
        .map(|(_, _, m)| preview(&ptext(m.get("content").unwrap_or(&serde_json::Value::Null))))
        .filter(|s| !s.is_empty());

    let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
    Some(Session {
        id: format!("prime:{stem}"),
        kind,
        t_start,
        t_end,
        title,
        profile: Some("prime".into()),
        source: Some("prime".into()),
        spans,
    })
}

/// Load ONE session by file stem (no window filter). None if unreadable/empty.
pub fn load_file_by_stem(f: &Path) -> Option<Session> {
    load_file(f, f64::NEG_INFINITY, f64::INFINITY)
}
