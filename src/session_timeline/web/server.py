#!/usr/bin/env python3
"""Live web UI for session timelines.

Serves the single-page app and a JSON API over span data rebuilt from the
harness's existing on-disk artifacts. Never writes to those artifacts; opens
every SQLite DB read-only. Zero personal data ships with this file: all paths
are discovered or configured via environment variables, and this module must
never contain machine-specific identifiers (see AGENTS.md HARD RULE #1).

Env:
  SESSION_TIMELINE_HERMES_HOME   dir containing profile subdirs with state.db
                                 (default: ~/.hermes — profiles/<name>/state.db,
                                 plus a top-level state.db as profile "default")
  SESSION_TIMELINE_PRIME_DIR     dir containing per-session *.jsonl
                                 (default: ~/.prime/agent/sessions; empty disables)
  SESSION_TIMELINE_PORT          listen port (default 8766)
  SESSION_TIMELINE_WINDOW_DAYS   how many days back to scan (default 92)
  SESSION_TIMELINE_TTL_S         cache TTL seconds (default 120)
"""
from __future__ import annotations

import glob
import json
import os
import sqlite3
import threading
import time
import traceback
from datetime import datetime, timedelta
from pathlib import Path

from fastapi import FastAPI, HTTPException, Query
from fastapi.responses import HTMLResponse, JSONResponse

from ..spans import GAP_CAP_S  # noqa: F401  (re-exported for convenience)

KINDS = ["idle", "inference", "tool"]
PREVIEW_N = 220

# ── configuration (env-driven, no machine-specific defaults beyond ~ schemes) ──

HERMES_HOME = Path(os.environ.get(
    "SESSION_TIMELINE_HERMES_HOME",
    os.path.expanduser("~/.hermes"),
))
_PRIME_ENV = os.environ.get("SESSION_TIMELINE_PRIME_DIR", "")
PRIME_DIR = os.path.expanduser(_PRIME_ENV) if _PRIME_ENV else os.path.expanduser("~/.prime/agent/sessions")
WINDOW_DAYS = int(os.environ.get("SESSION_TIMELINE_WINDOW_DAYS", "92"))
TTL_S = float(os.environ.get("SESSION_TIMELINE_TTL_S", "120"))


def hermes_profile_dbs(home: Path) -> dict[str, Path]:
    """Discover profile DBs: <home>/<profile>/state.db plus <home>/state.db as
    profile 'default'. Absent files are skipped silently so the same config
    works on any machine."""
    out: dict[str, Path] = {}
    top = home / "state.db"
    if top.exists():
        out["default"] = top
    profiles = home / "profiles"
    if profiles.is_dir():
        for d in sorted(profiles.iterdir()):
            db = d / "state.db"
            if d.is_dir() and db.exists():
                out[d.name] = db
    return out


def _prev(s) -> str:
    if not s:
        return ""
    return " ".join(str(s).split())[:PREVIEW_N]


def hermes_sessions(db_path: str | Path, d0: float, d1: float, profile: str) -> list[dict]:
    db = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    sess = db.execute(
        "SELECT id, title, parent_session_id, started_at, ended_at, last_activity_at, source FROM sessions "
        "WHERE archived=0 AND started_at IS NOT NULL AND started_at < ? AND "
        "COALESCE(ended_at, last_activity_at, started_at) > ? ORDER BY started_at", (d1, d0)
    ).fetchall()
    out = []
    n_bad = 0
    for s in sess:
        rows = db.execute(
            "SELECT role, tool_name, tool_call_id, tool_calls, content, timestamp, platform_message_id "
            "FROM messages WHERE session_id=? AND active=1 "
            "ORDER BY COALESCE(display_order, id)", (s["id"],)
        ).fetchall()
        if not rows:
            continue
        # only messages NOT injected by the harness count as human input —
        # injected user rows (platform messages) would misclassify subagent
        # sessions as human and pollute idle tooltips
        human_msgs = sum(1 for r in rows if r["role"] == "user" and r["platform_message_id"] is None)
        is_sub = s["parent_session_id"] is not None or human_msgs == 0
        spans, pending, last = [], {}, None
        for r in rows:
            role, ts, content = r["role"], r["timestamp"], r["content"] or ""
            is_human_user = role == "user" and r["platform_message_id"] is None
            if role == " ":
                continue
            if role == "user":
                if last is not None and ts > last:
                    spans.append({"kind": "idle", "t_start": last, "t_end": ts,
                                  "label": "idle",
                                  "meta": ({"next_user": _prev(content)} if is_human_user
                                           else {"note": "ended by injected message"})})
                pending.clear()
                last = ts
            elif role == "assistant":
                if last is not None and ts > last:
                    g = ts - last
                    spans.append({"kind": "inference" if g <= GAP_CAP_S else "idle",
                                  "t_start": last, "t_end": ts,
                                  "label": "model" if g <= GAP_CAP_S else "idle (long silence)",
                                  "meta": {"output": _prev(content)}})
                last = ts
                try:
                    tool_calls = json.loads(r["tool_calls"] or "[]")
                except (json.JSONDecodeError, TypeError):
                    tool_calls = []
                    n_bad += 1
                if not isinstance(tool_calls, list):
                    tool_calls = []
                    n_bad += 1
                for tc in tool_calls:
                    fn = tc.get("function", {}) if isinstance(tc, dict) else {}
                    cid = tc.get("call_id") or tc.get("id") or ""
                    pending[cid] = (ts, fn.get("name") or "?", fn.get("arguments") or "")
            elif role == "tool":
                cid = r["tool_call_id"] or ""
                if cid in pending:
                    start, name, args = pending.pop(cid)
                else:
                    start, name, args = last or ts, r["tool_name"] or "tool", ""
                if ts >= start:
                    spans.append({"kind": "tool", "t_start": start, "t_end": ts,
                                  "label": name,
                                  "meta": {"args": _prev(args), "result": _prev(content)}})
                last = max(last or ts, ts)
        if spans:
            # derive extent from spans — display_order (post-compaction) can put
            # a late-timestamped row first, which once produced t_end < t_start
            t_start = min(sp["t_start"] for sp in spans)
            t_end = max(sp["t_end"] for sp in spans)
        else:
            # no spans (e.g. single-message session): extent from message
            # timestamps, NOT display_order — row order is unreliable there too
            t_start = min(r["timestamp"] for r in rows)
            t_end = max(r["timestamp"] for r in rows)
        out.append({
            "id": f"{profile}:{s['id']}", "src": "hermes", "profile": profile,
            "orig_id": s["id"], "kind": "sub" if is_sub else "human",
            "source": str(s["source"] or ""),
            "title": _prev(s["title"] or rows[0]["content"] or s["id"])[:60],
            "t_start": t_start, "t_end": t_end,
            "spans": spans,
        })
    if n_bad:
        print(f"[warn] {profile}: skipped {n_bad} malformed tool_calls payloads", flush=True)
    db.close()
    return out


def _ts(iso: str) -> float:
    return datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp()


def _ptext(content) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return " ".join(p.get("text", "") for p in content if isinstance(p, dict))
    return ""


def prime_sessions(prime_dir: str, d0: float, d1: float) -> list[dict]:
    out = []
    if not prime_dir:
        return out
    for f in sorted(glob.glob(os.path.join(prime_dir, "*.jsonl"))):
        if os.path.getmtime(f) < d0 - 86400:
            continue
        events = []
        for ln in open(f):
            try:
                events.append(json.loads(ln))
            except json.JSONDecodeError:
                continue
        msgs = [e for e in events if e.get("type") == "message"]
        if not msgs:
            continue
        t0, t1 = _ts(msgs[0]["timestamp"]), _ts(msgs[-1]["timestamp"])
        if t1 < d0 or t0 > d1:
            continue
        spans, pending, last = [], {}, None
        for e in msgs:
            m = e["message"]
            role, ts = m.get("role"), _ts(e["timestamp"])
            if role == "user":
                if last is not None and ts > last:
                    spans.append({"kind": "idle", "t_start": last, "t_end": ts,
                                  "label": "idle", "meta": {"next_user": _prev(_ptext(m.get("content")))}})
                pending.clear()
                last = ts
            elif role == "assistant":
                if last is not None and ts > last:
                    g = ts - last
                    spans.append({"kind": "inference" if g <= GAP_CAP_S else "idle",
                                  "t_start": last, "t_end": ts,
                                  "label": "model" if g <= GAP_CAP_S else "idle (long silence)",
                                  "meta": {"output": _prev(_ptext(m.get("content")))}})
                last = ts
                for p in (m.get("content") or []):
                    if isinstance(p, dict) and p.get("type") == "toolCall":
                        pending[p.get("id") or ""] = (ts, p.get("name") or "?", p.get("arguments") or "")
            elif role == "toolResult":
                cid = m.get("toolCallId") or ""
                if cid in pending:
                    start, name, args = pending.pop(cid)
                else:
                    start, name, args = last or ts, m.get("toolName") or "tool", ""
                if ts >= start:
                    spans.append({"kind": "tool", "t_start": start, "t_end": ts,
                                  "label": name,
                                  "meta": {"args": _prev(args), "result": _prev(_ptext(m.get("content")))}})
                last = max(last or ts, ts)
        if spans:
            t_start = min(sp["t_start"] for sp in spans)
            t_end = max(sp["t_end"] for sp in spans)
        else:
            t_start, t_end = t0, t1
        out.append({
            "id": f"prime:{Path(f).stem}", "src": "prime", "profile": "prime",
            "orig_id": Path(f).stem, "kind": "prime", "source": "prime",
            "title": _prev(_ptext(msgs[0]["message"].get("content")))[:60] if msgs[0]["message"].get("role") == "user" else Path(f).stem,
            "t_start": t_start, "t_end": t_end, "spans": spans,
        })
    return out


def summarize(spans: list[dict]) -> str:
    def fmt(sec):
        return f"{sec/3600:.1f}h" if sec >= 3600 else (f"{sec/60:.0f}min" if sec >= 60 else f"{sec:.0f}s")

    def union(iv):
        tot, cs, ce = 0.0, None, None
        for a, b in sorted(iv):
            if cs is None or a > ce:
                if ce is not None:
                    tot += ce - cs
                cs, ce = a, b
            else:
                ce = max(ce, b)
        return tot + (ce - cs if cs is not None else 0)

    inf = union([(s["t_start"], s["t_end"]) for s in spans if s["kind"] == "inference"])
    tools = [s for s in spans if s["kind"] == "tool"]
    tol = union([(s["t_start"], s["t_end"]) for s in tools])
    idl = union([(s["t_start"], s["t_end"]) for s in spans if s["kind"] == "idle"])
    top: dict[str, float] = {}
    for s in tools:
        top[s["label"]] = top.get(s["label"], 0.0) + (s["t_end"] - s["t_start"])
    top_tool = max(top, key=lambda k: top[k]) if top else ""
    det = f" ({top_tool} {fmt(top[top_tool])})" if top_tool else ""
    return (f"inference {fmt(inf)} / tools {fmt(tol)}{det} / idle {fmt(idl)}")


def active_days(t0: float, t1: float, cap: int = 40) -> set[str]:
    """Local calendar days a span touches (capped to avoid pathological zombies)."""
    out: set[str] = set()
    d = datetime.fromtimestamp(t0).date()
    last = datetime.fromtimestamp(t1).date()
    while d <= last and len(out) < cap:
        out.add(d.isoformat())
        d += timedelta(days=1)
    return out


def scan(hermes_dbs: dict[str, Path], prime_dir: str, d0: float, d1: float) -> dict:
    sessions = []
    for prof, path in hermes_dbs.items():
        try:
            sessions += hermes_sessions(path, d0, d1, prof)
        except Exception as exc:
            print(f"[warn] skip hermes profile {prof}: {exc}", flush=True)
    sessions += prime_sessions(prime_dir, d0, d1)

    days: dict[str, list[str]] = {}
    index_sessions: dict[str, dict] = {}
    detail_days: dict[str, dict[str, list]] = {}
    for s in sessions:
        s["summary"] = summarize(s["spans"])
        # bucket by ACTIVE days (every day a span touches), not start day —
        # long-lived bot chats would otherwise be invisible on the days you
        # actually worked in them
        sdays: set[str] = set()
        for sp in s["spans"]:
            sdays |= active_days(sp["t_start"], sp["t_end"])
        if not sdays:
            sdays = {datetime.fromtimestamp(s["t_start"]).date().isoformat()}
        s["days"] = sorted(sdays)
        index_sessions[s["id"]] = {
            "kind": s["kind"], "title": s["title"], "source": s.get("source", ""),
            "profile": s.get("profile", ""), "days": s["days"],
            "t_start": s["t_start"], "t_end": s["t_end"], "summary": s["summary"],
            "S": [[KINDS.index(sp["kind"]), round(sp["t_start"], 3), round(sp["t_end"], 3)]
                  for sp in s["spans"]],
        }
        detail = [
            {"l": sp.get("label", ""), "m": sp.get("meta") or {}} for sp in s["spans"]
        ]
        for day in s["days"]:
            days.setdefault(day, []).append(s["id"])
            detail_days.setdefault(day, {})[s["id"]] = detail
    for day in days:
        days[day].sort(key=lambda sid: index_sessions[sid]["t_start"])
    return {"days": days, "sessions": index_sessions, "K": KINDS, "detail": detail_days}


# ── live cache: TTL + background rebuild (never blocks a request) ─────────

_CACHE: dict = {"data": None, "built_at": 0.0}
_LOCK = threading.Lock()
_BUILDING = False


def get_data() -> dict:
    """Serve from cache instantly; rebuild in background when stale.

    A live harness DB is written every few seconds, so mtime-based invalidation
    would trigger a full rescan on every poll. TTL + background rebuild keeps
    request latency at ~0ms. The first build runs once (either the startup warm
    thread or the first request, never both).
    """
    global _BUILDING
    if _CACHE["data"] is None:
        with _LOCK:
            if _CACHE["data"] is None and not _BUILDING:
                _BUILDING = True
                _rebuild()
            else:
                while _CACHE["data"] is None:
                    time.sleep(0.2)
    elif time.time() - _CACHE["built_at"] > TTL_S and not _BUILDING:
        with _LOCK:
            if not _BUILDING:
                _BUILDING = True
                threading.Thread(target=_rebuild, daemon=True).start()
    assert _CACHE["data"] is not None
    return _CACHE["data"]


def _rebuild() -> None:
    global _BUILDING
    try:
        t0 = time.time()
        data = scan(hermes_profile_dbs(HERMES_HOME), PRIME_DIR,
                    datetime.now().timestamp() - WINDOW_DAYS * 86400,
                    datetime.now().timestamp())
        _CACHE["data"] = data
        _CACHE["built_at"] = time.time()
        print(f"[scan] rebuilt in {time.time()-t0:.1f}s ({len(data['sessions'])} sessions)", flush=True)
    except Exception:
        # never take the cache down with a failed rebuild; keep serving the
        # last good snapshot and log loudly
        traceback.print_exc()
        if _CACHE["data"] is None:
            _CACHE["data"] = {"days": {}, "sessions": {}, "K": KINDS, "detail": {}}
    finally:
        _BUILDING = False


app = FastAPI()


@app.get("/api/index")
def api_index() -> JSONResponse:
    d = get_data()
    return JSONResponse({"days": d["days"], "sessions": d["sessions"], "K": d["K"]})


@app.get("/api/detail")
def api_detail(day: str = Query(...)) -> JSONResponse:
    d = get_data()
    if day not in d["detail"]:
        raise HTTPException(404, f"no data for {day}")
    return JSONResponse(d["detail"][day])


@app.get("/api/refresh")
def api_refresh() -> JSONResponse:
    get_data()
    return JSONResponse({"ok": True, "built_at": _CACHE["built_at"]})


@app.get("/")
def index() -> HTMLResponse:
    shell = (Path(__file__).parent / "app_shell.html").read_text()
    return HTMLResponse(shell)


@app.on_event("startup")
def _warm() -> None:
    threading.Thread(target=_rebuild, daemon=True).start()


def main() -> None:
    import uvicorn
    uvicorn.run(app, host="127.0.0.1", port=int(os.environ.get("SESSION_TIMELINE_PORT", "8766")), log_level="warning")


if __name__ == "__main__":
    main()
