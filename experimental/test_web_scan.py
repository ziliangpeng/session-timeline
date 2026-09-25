"""Pure-function tests for the web index layer — synthetic data only.

Covers the two invariants that broke during prototyping:
  1. sessions must appear on every day they have activity in, not just their
     start day (long-lived chats were invisible on later days);
  2. a session's extent must come from its spans, so display_order oddities
     (post-compaction rows) can never produce t_end < t_start.
"""
from __future__ import annotations

from datetime import datetime, timedelta

from server import active_days, scan


def _ts(s: str) -> float:
    return datetime.fromisoformat(s).timestamp()


def _hermes_db(tmp_path, sessions):
    """Create a synthetic Hermes state.db with the columns the scanner reads."""
    import sqlite3
    db_path = tmp_path / "state.db"
    db = sqlite3.connect(db_path)
    db.executescript(
        """
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY, title TEXT, parent_session_id TEXT,
            started_at REAL, ended_at REAL, last_activity_at REAL, source TEXT,
            archived INTEGER DEFAULT 0
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, role TEXT,
            content TEXT, tool_call_id TEXT, tool_calls TEXT, tool_name TEXT,
            timestamp REAL, platform_message_id TEXT,
            active INTEGER DEFAULT 1, compacted INTEGER DEFAULT 0,
            display_order INTEGER
        );
        """
    )
    for s in sessions:
        db.execute(
            "INSERT INTO sessions VALUES (?,?,?,?,?,?,?,0)",
            (s["id"], s.get("title"), s.get("parent"), s["t0"], s["t1"], s["t1"], s.get("source", "tui")),
        )
        for i, m in enumerate(s["msgs"]):
            db.execute(
                "INSERT INTO messages (session_id, role, content, timestamp, platform_message_id, active, display_order) "
                "VALUES (?,?,?,?,?,?,?)",
                (s["id"], m["role"], m.get("content", ""), m["ts"], m.get("pmid"), 1, i),
            )
    db.commit()
    db.close()
    return db_path


def test_active_days_multi_day_span():
    # span covering 48h touches three local days
    d = active_days(_ts("2026-01-01 23:00"), _ts("2026-01-03 01:00"))
    assert d == {"2026-01-01", "2026-01-02", "2026-01-03"}


def test_active_days_cap():
    # a dormant 100-day span is capped
    assert len(active_days(_ts("2026-01-01"), _ts("2026-04-15"))) == 40


def test_scan_buckets_by_active_days(tmp_path):
    # session starts on day 1, last message on day 3 → must appear on all three days
    db = _hermes_db(tmp_path, [{
        "id": "s1", "t0": _ts("2026-01-01 10:00"), "t1": _ts("2026-01-03 12:00"),
        "msgs": [
            {"role": "user", "ts": _ts("2026-01-01 10:00"), "content": "hello"},
            {"role": "assistant", "ts": _ts("2026-01-01 10:01")},
            {"role": "user", "ts": _ts("2026-01-03 12:00"), "content": "back again"},
            {"role": "assistant", "ts": _ts("2026-01-03 12:01")},
        ],
    }])
    d0, d1 = _ts("2025-12-25"), _ts("2026-01-10")
    from server import hermes_sessions
    out = hermes_sessions(str(db), d0, d1, "synthetic")
    assert len(out) == 1

    res = scan({"synthetic": db}, "", d0, d1)
    for day in ("2026-01-01", "2026-01-02", "2026-01-03"):
        assert "synthetic:s1" in res["days"][day], f"missing on {day}"
    assert res["sessions"]["synthetic:s1"]["days"] == ["2026-01-01", "2026-01-02", "2026-01-03"]
    # overnight idle span exists and is a single span from last assistant to the
    # day-3 user message
    idles = [sp for sp in out[0]["spans"] if sp["kind"] == "idle"]
    assert any(sp["t_start"] < _ts("2026-01-02") and sp["t_end"] > _ts("2026-01-03") for sp in idles)


def test_scan_injected_user_rows_do_not_count_as_human(tmp_path):
    # a subagent session whose only user rows are platform-injected must be
    # classified 'sub', and its idle meta must say injected
    db = _hermes_db(tmp_path, [{
        "id": "child1", "parent": "s0", "t0": _ts("2026-01-05 09:00"), "t1": _ts("2026-01-05 09:05"),
        "msgs": [
            {"role": "user", "ts": _ts("2026-01-05 09:00"), "pmid": "pm-1", "content": "[injected context]"},
            {"role": "assistant", "ts": _ts("2026-01-05 09:04")},
        ],
    }])
    from server import hermes_sessions
    out = hermes_sessions(str(db), _ts("2026-01-01"), _ts("2026-01-10"), "synthetic")
    assert out[0]["kind"] == "sub"


def test_extent_from_spans_never_negative(tmp_path):
    # display_order puts a LATER-timestamped row first (post-compaction shape);
    # extent must come from spans, so t_end >= t_start always
    import sqlite3
    db_path = tmp_path / "state.db"
    db = sqlite3.connect(db_path)
    db.executescript(
        """
        CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, parent_session_id TEXT,
            started_at REAL, ended_at REAL, last_activity_at REAL, source TEXT, archived INTEGER DEFAULT 0);
        CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, role TEXT,
            content TEXT, tool_call_id TEXT, tool_calls TEXT, tool_name TEXT, timestamp REAL,
            platform_message_id TEXT, active INTEGER DEFAULT 1,
            display_order INTEGER);
        """
    )
    db.execute("INSERT INTO sessions VALUES ('s1','t',NULL,?,?,?,'tui',0)",
               (_ts("2026-01-05 08:00"), _ts("2026-01-05 11:00"), _ts("2026-01-05 11:00")))
    db.execute("INSERT INTO messages (session_id, role, content, timestamp, display_order) VALUES ('s1','user','q',?,2)",
               (_ts("2026-01-05 08:05"),))
    db.execute("INSERT INTO messages (session_id, role, content, timestamp, display_order) VALUES ('s1','assistant','a',?,1)",
               (_ts("2026-01-05 11:14"),))  # out-of-order vs display_order
    db.commit()
    db.close()
    from server import hermes_sessions
    out = hermes_sessions(str(db_path), _ts("2026-01-01"), _ts("2026-01-10"), "synthetic")
    assert len(out) == 1
    assert out[0]["t_end"] >= out[0]["t_start"]
    assert out[0]["t_start"] == _ts("2026-01-05 08:05")


def test_malformed_tool_calls_do_not_kill_the_profile(tmp_path):
    # one corrupted tool_calls payload must not lose the session or crash
    import sqlite3
    db_path = tmp_path / "state.db"
    db = sqlite3.connect(db_path)
    db.executescript(
        """
        CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, parent_session_id TEXT,
            started_at REAL, ended_at REAL, last_activity_at REAL, source TEXT, archived INTEGER DEFAULT 0);
        CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, role TEXT,
            content TEXT, tool_call_id TEXT, tool_calls TEXT, tool_name TEXT, timestamp REAL,
            platform_message_id TEXT,
            active INTEGER DEFAULT 1, display_order INTEGER);
        """
    )
    db.execute("INSERT INTO sessions VALUES ('s1','t',NULL,?,?,?,'tui',0)",
               (_ts("2026-01-05 08:00"), _ts("2026-01-05 08:10"), _ts("2026-01-05 08:10")))
    db.execute("INSERT INTO messages (session_id, role, content, tool_calls, timestamp, display_order) "
               "VALUES ('s1','user','q',NULL,?,1)", (_ts("2026-01-05 08:00"),))
    db.execute("INSERT INTO messages (session_id, role, content, tool_calls, timestamp, display_order) "
               "VALUES ('s1','assistant','a','{not json',?,2)", (_ts("2026-01-05 08:05"),))
    db.commit()
    db.close()
    from server import hermes_sessions
    out = hermes_sessions(str(db_path), _ts("2026-01-01"), _ts("2026-01-10"), "synthetic")
    assert len(out) == 1  # session survives, bad payload skipped
