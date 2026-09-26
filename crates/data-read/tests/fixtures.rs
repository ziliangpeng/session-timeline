//! Synthetic fixtures for data-read tests: temp-dir Hermes homes (SQLite) and
//! Prime session dirs (JSONL). No real DBs, no personal data — ever.
//! Shared by several test binaries; not every helper is used by every binary.
#![allow(dead_code)]

use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Fixed window [100, 300) for all tests.
pub const T0: f64 = 100.0;
pub const T1: f64 = 300.0;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dataread-test-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn hermes_home(tag: &str) -> PathBuf {
    let home = tmp(tag);
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    home
}

pub fn prime_dir(tag: &str) -> PathBuf {
    tmp(&format!("prime-{tag}"))
}

pub fn write_jsonl(dir: &Path, name: &str, lines: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), lines.join("\n")).unwrap();
}

pub struct Sess<'a> {
    pub id: &'a str,
    pub title: Option<&'a str>,
    pub parent: Option<&'a str>,
    pub source: Option<&'a str>,
    pub started: f64,
    pub ended: f64,
}

pub struct Msg {
    pub id: i64,
    pub role: &'static str,
    pub content: &'static str,
    pub ts: f64,
    pub platform_message_id: Option<&'static str>,
    pub tool_calls: Option<&'static str>,
    pub tool_call_id: Option<&'static str>,
}

pub fn sess_row<'a>(
    id: &'a str,
    title: Option<&'a str>,
    parent: Option<&'a str>,
    source: Option<&'a str>,
    started: f64,
    ended: f64,
) -> Sess<'a> {
    Sess {
        id,
        title,
        parent,
        source,
        started,
        ended,
    }
}

pub fn msg_row(
    id: i64,
    role: &'static str,
    content: &'static str,
    ts: f64,
    platform_message_id: Option<&'static str>,
    tool_calls: Option<&'static str>,
    tool_call_id: Option<&'static str>,
) -> Msg {
    Msg {
        id,
        role,
        content,
        ts,
        platform_message_id,
        tool_calls,
        tool_call_id,
    }
}

/// Create a minimal Hermes-shaped state.db. Schema mirrors the columns the
/// loader queries (subset of the real one): sessions + messages.
pub fn write_db(path: &Path, sessions: &[Sess<'_>], msgs: &[Msg]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            title TEXT,
            parent_session_id TEXT,
            source TEXT,
            archived INTEGER DEFAULT 0,
            started_at REAL,
            ended_at REAL,
            last_activity_at REAL
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY,
            session_id TEXT,
            role TEXT,
            content TEXT,
            timestamp REAL,
            active INTEGER DEFAULT 1,
            display_order INTEGER,
            platform_message_id TEXT,
            tool_name TEXT,
            tool_call_id TEXT,
            tool_calls TEXT
        );",
    )
    .unwrap();
    for s in sessions {
        conn.execute(
            "INSERT INTO sessions (id, title, parent_session_id, source, archived, started_at, ended_at, last_activity_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?6)",
            rusqlite::params![s.id, s.title, s.parent, s.source, s.started, s.ended],
        )
        .unwrap();
    }
    for (i, m) in msgs.iter().enumerate() {
        conn.execute(
            "INSERT INTO messages (id, session_id, role, content, timestamp, active, display_order, platform_message_id, tool_name, tool_call_id, tool_calls)
             VALUES (?1, 's1', ?2, ?3, ?4, 1, ?5, ?6, NULL, ?7, ?8)",
            // NOTE: session_id hardwired 's1' — fixtures only build single-session
            // DBs in tests that need messages (see msg_session_id to override).
            rusqlite::params![
                m.id,
                m.role,
                m.content,
                m.ts,
                (i + 1) as i64,
                m.platform_message_id,
                m.tool_call_id,
                m.tool_calls
            ],
        )
        .unwrap();
    }
}

/// Point every message at a different session id (for multi-session tests).
pub fn msg_session_id(path: &Path, sid: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute("UPDATE messages SET session_id = ?1", [sid])
        .unwrap();
}

/// Mark a message row active=0 (compaction dead history).
pub fn set_active(path: &Path, msg_id: i64, active: i64) {
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "UPDATE messages SET active = ?1 WHERE id = ?2",
        rusqlite::params![active, msg_id],
    )
    .unwrap();
}

/// No-op kept for API symmetry; SQLite commits are immediate without WAL.
pub fn wait_for_wal(_path: &Path) {}
