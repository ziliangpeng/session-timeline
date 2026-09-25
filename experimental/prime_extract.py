#!/usr/bin/env python3
"""Prime Agent session JSONL → spans JSON (prototype, experimental/ — never committed).

Event model: message rows with role user/assistant/toolResult; assistant content parts
may include toolCall {id, name}. toolCall→toolResult pairing by id.

Idle semantics (v1, user-directed): any silence with no activity is idle — a silence
ended by a background/harness event is still "nothing happened". Idle is first-class.
"""
from __future__ import annotations

import json
import sys
from datetime import datetime
from pathlib import Path

from session_timeline.spans import SessionTimeline, Span, SpanKind

GAP_CAP_S = 300.0  # silences longer than this are idle, not model time


def _ts(iso: str) -> float:
    return datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp()


def extract(path: str) -> SessionTimeline:
    events = []
    for ln in open(path):
        try:
            events.append(json.loads(ln))
        except json.JSONDecodeError:
            continue
    msgs = [e for e in events if e.get("type") == "message"]
    if not msgs:
        raise SystemExit("no message events")

    session_id = Path(path).stem
    spans: list[Span] = []
    pending: dict[str, tuple[float, str]] = {}
    last_ts: float | None = None
    started_at = _ts(msgs[0]["timestamp"])

    for e in msgs:
        m = e["message"]
        role, ts = m.get("role"), _ts(e["timestamp"])
        if role == "user":
            if last_ts is not None and ts > last_ts:
                spans.append(Span(SpanKind.IDLE, last_ts, ts, "waiting for user"))
            pending.clear()
            last_ts = ts
        elif role == "assistant":
            if last_ts is not None and ts > last_ts:
                kind = (
                    SpanKind.INFERENCE if ts - last_ts <= GAP_CAP_S
                    else SpanKind.IDLE
                )
                spans.append(Span(kind, last_ts, ts, "model" if kind is SpanKind.INFERENCE else "idle (long silence)"))
            last_ts = ts
            c = m.get("content")
            for p in (c if isinstance(c, list) else []):
                if isinstance(p, dict) and p.get("type") == "toolCall":
                    pending[p.get("id") or ""] = (ts, p.get("name") or "?")
        elif role == "toolResult":
            cid = m.get("toolCallId") or ""
            if cid in pending:
                start, name = pending.pop(cid)
            else:
                start, name = last_ts or ts, m.get("toolName") or "tool"
            if ts >= start:
                spans.append(Span(SpanKind.TOOL, start, ts, name))
            last_ts = max(last_ts or ts, ts)

    return SessionTimeline(
        session_id=session_id, spans=spans,
        started_at=started_at, ended_at=_ts(msgs[-1]["timestamp"])
    )


def main() -> None:
    path, out = sys.argv[1], sys.argv[2]
    tl = extract(path)
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
