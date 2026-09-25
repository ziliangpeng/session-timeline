#!/usr/bin/env python3
"""Hermes session → spans JSON (prototype, experimental/ — real data, never committed).

Derivation model (v1):
  user msg → assistant-with-tool_calls : INFERENCE span
  assistant row → each tool result row : TOOL span (per call; parallel tools each get
      their own interval [call_ts, result_ts], union math handles overlap)
  tool result → next assistant        : INFERENCE span
  final assistant (no tool calls) → next user msg : IDLE span

Idle semantics (v1, user-directed): ANY silence with no activity is "idle" — whether
the silence ends with a user message or a background event. Idle is "nothing
happened", not "provably waiting for the user to type".

Data hygiene: after in-place compaction the messages table keeps dead rows
(active=0 / compacted=1) and the rolled-forward history re-lands with OLD
timestamps. Only active=1 rows are the live conversation; filter, then sort by
(display_order or id). Never trust raw id order post-compaction.
"""
from __future__ import annotations

import json
import sqlite3
import sys
from pathlib import Path

from session_timeline.spans import SessionTimeline, Span, SpanKind

GAP_CAP_S = 300.0  # inference silences longer than this are shown as idle, not model time


def extract(db_path: str, session_id: str) -> SessionTimeline:
    db = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    rows = db.execute(
        "SELECT id, role, tool_name, tool_call_id, tool_calls, timestamp "
        "FROM messages WHERE session_id = ? AND active = 1 "
        "ORDER BY COALESCE(display_order, id)", (session_id,)
    ).fetchall()
    if not rows:
        # older rows may predate the active column semantics — fall back to everything
        rows = db.execute(
            "SELECT id, role, tool_name, tool_call_id, tool_calls, timestamp "
            "FROM messages WHERE session_id = ? ORDER BY id", (session_id,)
        ).fetchall()
    if not rows:
        raise SystemExit(f"no messages for session {session_id}")

    spans: list[Span] = []
    pending_calls: dict[str, tuple[float, str]] = {}  # call_id -> (start_ts, tool_name)
    last_activity_ts: float | None = None
    started_at = rows[0]["timestamp"]

    for r in rows:
        role, ts = r["role"], r["timestamp"]
        if role == "user":
            if last_activity_ts is not None and ts > last_activity_ts:
                spans.append(Span(SpanKind.IDLE, last_activity_ts, ts, "waiting for user"))
            pending_calls.clear()
            last_activity_ts = ts
        elif role == "assistant":
            if last_activity_ts is not None and ts > last_activity_ts:
                kind = (
                    SpanKind.INFERENCE if ts - last_activity_ts <= GAP_CAP_S
                    else SpanKind.IDLE
                )
                spans.append(Span(kind, last_activity_ts, ts, "model" if kind is SpanKind.INFERENCE else "idle (long silence)"))
            last_activity_ts = ts
            for tc in json.loads(r["tool_calls"] or "[]"):
                fn = tc.get("function", {}) if isinstance(tc, dict) else {}
                cid = tc.get("call_id") or tc.get("id") or ""
                pending_calls[cid] = (ts, fn.get("name") or "?")
        elif role == "tool":
            cid = r["tool_call_id"] or ""
            if cid in pending_calls:
                start, name = pending_calls.pop(cid)
            else:  # unmatched (batch retry etc.) — charge from previous activity
                start, name = last_activity_ts or ts, r["tool_name"] or "tool"
            if ts >= start:
                spans.append(Span(SpanKind.TOOL, start, ts, name))
            last_activity_ts = max(last_activity_ts or ts, ts)

    ended_at = rows[-1]["timestamp"]
    return SessionTimeline(
        session_id=session_id, spans=spans, started_at=started_at, ended_at=ended_at
    )


def main() -> None:
    db_path, session_id, out = sys.argv[1], sys.argv[2], sys.argv[3]
    tl = extract(db_path, session_id)
    Path(out).write_text(json.dumps({
        "session_id": tl.session_id,
        "started_at": tl.started_at,
        "ended_at": tl.ended_at,
        "summary": tl.summary_line(),
        "spans": [
            {"kind": s.kind.value, "t_start": s.t_start, "t_end": s.t_end,
             "label": s.label, "duration": round(s.duration, 3)}
            for s in tl.spans
        ],
    }, indent=1))
    print(tl.summary_line())
    print(f"spans: {len(tl.spans)} -> {out}")


if __name__ == "__main__":
    main()
