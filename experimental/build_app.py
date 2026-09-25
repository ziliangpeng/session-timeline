#!/usr/bin/env python3
"""Build the hierarchical prototype app — single page, stacked sections.

Usage: python build_app.py --from 2026-06-25 --to 2026-09-25 --out experimental/app.html
Experimental only — embeds REAL session data; this directory is gitignored, never commit.
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import sqlite3
from datetime import datetime, timedelta
from pathlib import Path

GAP_CAP_S = 300.0
PREVIEW_N = 220

HERMES_HOME = os.path.expanduser("~/.hermes")
PRIME_DIR = os.path.expanduser("~/.prime/agent/sessions")


def _discover_hermes_dbs():
    """profile name -> state.db path, auto-discovered (no hardcoded profile list)."""
    out = {}
    top = os.path.join(HERMES_HOME, "state.db")
    if os.path.exists(top):
        out["default"] = top
    profiles = os.path.join(HERMES_HOME, "profiles")
    if os.path.isdir(profiles):
        for name in sorted(os.listdir(profiles)):
            db = os.path.join(profiles, name, "state.db")
            if os.path.exists(db):
                out[name] = db
    return out


def _prev(s) -> str:
    if not s:
        return ""
    return " ".join(str(s).split())[:PREVIEW_N]


# ── Hermes ──────────────────────────────────────────────────────────

def hermes_sessions(db_path: str, d0: float, d1: float, profile: str) -> list[dict]:
    db = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    sess = db.execute(
        "SELECT id, title, parent_session_id, started_at, ended_at, last_activity_at, source FROM sessions "
        "WHERE archived=0 AND started_at IS NOT NULL AND started_at < ? AND "
        "COALESCE(ended_at, last_activity_at, started_at) > ? ORDER BY started_at", (d1, d0)
    ).fetchall()
    out = []
    for s in sess:
        rows = db.execute(
            "SELECT role, tool_name, tool_call_id, tool_calls, content, timestamp "
            "FROM messages WHERE session_id=? AND active=1 "
            "ORDER BY COALESCE(display_order, id)", (s["id"],)
        ).fetchall()
        if not rows:
            continue
        user_msgs = sum(1 for r in rows if r["role"] == "user")
        is_sub = s["parent_session_id"] is not None or user_msgs == 0
        spans, pending, last = [], {}, None
        for r in rows:
            role, ts, content = r["role"], r["timestamp"], r["content"] or ""
            if role == "user":
                if last is not None and ts > last:
                    spans.append({"kind": "idle", "t_start": last, "t_end": ts,
                                  "label": "idle", "meta": {"next_user": _prev(content)}})
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
                for tc in json.loads(r["tool_calls"] or "[]"):
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
        out.append({
            "id": f"{profile}:{s['id']}", "src": "hermes", "profile": profile,
            "orig_id": s["id"], "kind": "sub" if is_sub else "human",
            "source": str(s["source"] or ""),
            "title": _prev(s["title"] or rows[0]["content"] or s["id"])[:60],
            "t_start": rows[0]["timestamp"], "t_end": rows[-1]["timestamp"],
            "spans": spans,
        })
    return out


# ── Prime ───────────────────────────────────────────────────────────

def _ts(iso: str) -> float:
    return datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp()


def _ptext(content) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return " ".join(p.get("text", "") for p in content if isinstance(p, dict))
    return ""


def prime_sessions(d0: float, d1: float) -> list[dict]:
    out = []
    for f in sorted(glob.glob(os.path.join(PRIME_DIR, "*.jsonl"))):
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
        out.append({
            "id": f"prime:{Path(f).stem}", "src": "prime", "profile": "prime",
            "orig_id": Path(f).stem, "kind": "prime", "source": "prime",
            "title": _prev(_ptext(msgs[0]["message"].get("content")))[:60] if msgs[0]["message"].get("role") == "user" else Path(f).stem,
            "t_start": t0, "t_end": t1, "spans": spans,
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
    t = max(top, key=lambda k: top[k]) if top else ""
    det = f" ({t} {fmt(top[t])})" if t else ""
    return (f"inference {fmt(inf)} / tools {fmt(tol)}{det} / idle {fmt(idl)}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--from", dest="frm", required=True)
    ap.add_argument("--to", dest="to", required=True)
    ap.add_argument("--out", default="experimental/app.html")
    a = ap.parse_args()
    d_start = datetime.fromisoformat(a.frm)
    d_end = datetime.fromisoformat(a.to) + timedelta(days=1)
    d0, d1 = d_start.timestamp(), d_end.timestamp()

    sessions = []
    for prof, path in _discover_hermes_dbs().items():
        try:
            sessions += hermes_sessions(path, d0, d1, prof)
        except Exception as exc:
            print(f"skip {prof}: {exc}")
    sessions += prime_sessions(d0, d1)

    days: dict[str, list[str]] = {}
    for s in sessions:
        day = datetime.fromtimestamp(s["t_start"]).date().isoformat()
        s["summary"] = summarize(s["spans"])
        days.setdefault(day, []).append(s["id"])

    # Index payload (app.html): sessions with span arrays ONLY — no meta strings.
    KINDS = ["idle", "inference", "tool"]
    index_sessions = {}
    detail_days: dict[str, dict[str, list]] = {}
    for s in sessions:
        day = datetime.fromtimestamp(s["t_start"]).date().isoformat()
        index_sessions[s["id"]] = {
            "kind": s["kind"], "title": s["title"], "source": s.get("source", ""),
            "profile": s.get("profile", ""),
            "t_start": s["t_start"], "t_end": s["t_end"], "summary": s["summary"],
            "S": [[KINDS.index(sp["kind"]), round(sp["t_start"], 3), round(sp["t_end"], 3)]
                  for sp in s["spans"]],
        }
        detail_days.setdefault(day, {})[s["id"]] = [
            {"l": sp.get("label", ""), "m": sp.get("meta") or {}} for sp in s["spans"]
        ]

    # Prime sessions: mark profile as 'prime'
    for s in sessions:
        if s["src"] == "prime":
            s["profile"] = "prime"

    data = {"days": days, "sessions": index_sessions, "K": KINDS}
    payload = json.dumps(data, separators=(",", ":")).replace("</", "<\\/")

    html = HTML.replace("__DATA__", payload)
    Path(a.out).write_text(html)

    detail_dir = Path(a.out).parent / "detail"
    detail_dir.mkdir(exist_ok=True)
    for day, per_sess in detail_days.items():
        js = ("window.DETAIL=window.DETAIL||{};DETAIL[%s]=%s;" %
              (json.dumps(day), json.dumps(per_sess, separators=(",", ":")).replace("</", "<\\/")))
        (detail_dir / f"detail-{day}.js").write_text(js)

    n_spans = sum(len(s["spans"]) for s in sessions)
    print(f"app: {len(sessions)} sessions / {n_spans} spans / {len(days)} days -> {a.out}")
    print(f"size: {Path(a.out).stat().st_size/1e6:.1f} MB + {len(detail_days)} day-shard detail files in {detail_dir}")


HTML = r"""<!doctype html>
<html><head><meta charset="utf-8"><title>session timelines</title>
<style>
 body { font: 13px/1.5 -apple-system, sans-serif; margin: 0; background: #14161a; color: #d7dae0; }
 header { padding: 10px 24px; border-bottom: 1px solid #262a31; position: sticky; top: 0; background: #14161acc; backdrop-filter: blur(6px); z-index: 10; }
 header h1 { font-size: 15px; margin: 0 0 4px; display: inline-block; }
 .legend { font-size: 12px; color: #8a8f98; margin-left: 18px; display: inline-block; }
 .legend i { display: inline-block; width: 10px; height: 10px; border-radius: 2px; margin: 0 4px 0 12px; vertical-align: -1px; }
 main { padding: 18px 24px 80px; }
 .sec { margin-bottom: 30px; }
 .sec > h2 { font-size: 12px; color: #7c828d; text-transform: uppercase; letter-spacing: .06em; margin: 0 0 8px; border-bottom: 1px solid #23272e; padding-bottom: 4px; }
 #filters { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin-bottom: 22px; }
 .fbtn { background: #1e2127; border: 1px solid #2c313a; border-radius: 16px; padding: 5px 12px; cursor: pointer; font-size: 12px; color: #d7dae0; }
 .fbtn.off { color: #5d636d; text-decoration: line-through; }
 .fbtn:hover { border-color: #6c8ef5; }
 .fsep { color: #3a3f47; margin: 0 2px; }
 .mrow, .drow { display: flex; flex-wrap: wrap; gap: 8px; }
 .monthbtn, .daybtn { background: #1e2127; border: 1px solid #2c313a; border-radius: 8px; padding: 8px 12px; cursor: pointer; min-width: 86px; }
 .monthbtn { min-width: 128px; }
 .monthbtn:hover, .daybtn:hover { border-color: #6c8ef5; }
 .monthbtn.sel, .daybtn.sel { border-color: #6c8ef5; background: #232941; }
 .monthbtn .d, .daybtn .d { font-weight: 600; }
 .daybtn .d { font-size: 15px; }
 .monthbtn .c, .daybtn .c { color: #7c828d; font-size: 11px; }
 .lane { display: flex; align-items: center; margin: 3px 0; }
 .lbl { width: 210px; color: #8a8f98; font-size: 11px; text-align: right; padding-right: 10px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; flex: none; cursor: pointer; }
 .lbl.sub { color: #5d636d; }
 .track { position: relative; flex: 1; height: 16px; background: #1e2127; border-radius: 4px; }
 .seg { position: absolute; top: 1px; bottom: 1px; border-radius: 3px; min-width: 1px; }
 .seg.inference { background: #6c8ef5; }
 .seg.tool { background: #3fb28f; }
 .seg.idle { background: #8a8f98; opacity: .5; }
 .sub-row .seg { opacity: .55; }
 .gl { position: absolute; top: 0; bottom: 0; width: 1px; background: #ffffff14; }
 .axisrow { margin-top: 1px; } .axis { height: 16px; background: none; }
 .tick { position: absolute; top: 2px; transform: translateX(-50%); color: #7c828d; font-size: 10px; white-space: nowrap; }
 .summary { color: #9fd0ff; margin: 1px 0 4px; font-family: ui-monospace, monospace; font-size: 12px; }
 .meta { color: #7c828d; font-size: 11px; margin: 0 0 4px; }
 .sess-block h3 { font-size: 13px; margin: 0; }
 #tip { position: fixed; background: #0f1116; border: 1px solid #3a3f47; border-radius: 8px; padding: 8px 10px; font-size: 12px; max-width: 420px; pointer-events: none; z-index: 50; display: none; box-shadow: 0 6px 24px #000a; }
 #tip .t { color: #9fd0ff; }
 #tip .m { color: #7c828d; margin-top: 3px; white-space: pre-wrap; word-break: break-word; max-height: 200px; overflow: hidden; }
 #panel { position: fixed; right: 0; top: 0; bottom: 0; width: 460px; background: #101318; border-left: 1px solid #2c313a; padding: 16px; overflow-y: auto; display: none; z-index: 40; }
 #panel h3 { margin: 0 0 6px; font-size: 14px; }
 #panel .kv { color: #7c828d; font-size: 12px; margin-bottom: 8px; }
 #panel pre { background: #0c0e12; border: 1px solid #262a31; border-radius: 6px; padding: 8px; font-size: 11px; white-space: pre-wrap; word-break: break-word; max-height: 260px; overflow: auto; }
 #panel .note { color: #5d636d; font-size: 11px; margin-top: 8px; }
 .empty-hint { color: #5d636d; font-size: 12px; }
</style></head><body>
<header>
  <h1>Agent session timelines</h1>
  <span class="legend">
    <i style="background:#6c8ef5"></i>inference
    <i style="background:#3fb28f"></i>tools
    <i style="background:#8a8f98;opacity:.5"></i>idle
    <span style="color:#5d636d">· dimmed = subagent sessions (no human input)</span>
  </span>
</header>
<main>
  <div id="filters"></div>
  <section class="sec" id="sec-months"></section>
  <section class="sec" id="sec-days"></section>
  <section class="sec" id="sec-day"></section>
  <section class="sec" id="sec-session"></section>
</main>
<div id="tip"></div>
<div id="panel"></div>
<script type="application/json" id="data">__DATA__</script>
<script>
const DATA = JSON.parse(document.getElementById('data').textContent);
const $ = s => document.querySelector(s);
const fmt = s => s >= 3600 ? (s/3600).toFixed(1)+'h' : s >= 60 ? Math.round(s/60)+'min' : Math.round(s)+'s';
const dts = t => new Date(t*1000).toLocaleString([], {month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit'});
const esc = v => String(v ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));

const state = { month: null, day: null, session: null };
const filters = { sub: true, cron: true, oneshot: true, short: true };

function passesFilters(s) {
  if (!filters.sub && s.kind === 'sub') return false;
  if (!filters.cron && s.source === 'cron') return false;
  if (!filters.oneshot && s.source === 'oneshot') return false;
  if (!filters.short && (s.t_end - s.t_start) < 300) return false;
  return true;
}

function renderFilters() {
  const n = { sub: 0, cron: 0, oneshot: 0, short: 0 };
  for (const s of Object.values(DATA.sessions)) {
    if (s.kind === 'sub') n.sub++;
    if (s.source === 'cron') n.cron++;
    if (s.source === 'oneshot') n.oneshot++;
    if ((s.t_end - s.t_start) < 300) n.short++;
  }
  const btn = (key, label) =>
    `<div class="fbtn ${filters[key] ? '' : 'off'}" data-f="${key}">${label} · ${n[key]}</div>`;
  $('#filters').innerHTML =
    btn('sub', '↳ subagents') + btn('cron', '⏰ cron') + btn('oneshot', '⚡ oneshots') +
    btn('short', '&lt;5min sessions') +
    `<span class="fsep">|</span><span style="color:#5d636d;font-size:12px">off = hidden</span>`;
  $('#filters').querySelectorAll('.fbtn').forEach(el => el.onclick = () => {
    filters[el.dataset.f] = !filters[el.dataset.f];
    renderAll();
  });
}
window.DETAIL = {};
const loadedDays = {};
function ensureDetail(day, cb) {
  if (loadedDays[day] !== undefined) { cb(loadedDays[day]); return; }
  const sc = document.createElement('script');
  sc.src = 'detail/detail-' + day + '.js';
  sc.onload = () => { loadedDays[day] = true; cb(true); };
  sc.onerror = () => { loadedDays[day] = false; cb(false); };
  document.head.appendChild(sc);
}
function spanKind(sp) { return DATA.K[sp[0]]; }
function spanStart(sp) { return sp[1]; }
function spanEnd(sp) { return sp[2]; }
function lday(t) { const d = new Date(t*1000); return d.getFullYear()+'-'+String(d.getMonth()+1).padStart(2,'0')+'-'+String(d.getDate()).padStart(2,'0'); }
function spanMeta(sId, sp) {
  const per = window.DETAIL[lday(DATA.sessions[sId].t_start)];
  if (!per || !per[sId]) return { l: '', m: {} };
  const i = DATA.sessions[sId].S.indexOf(sp);
  return per[sId][i] || { l: '', m: {} };
}

function months() {
  const m = {};
  for (const d of Object.keys(DATA.days).sort()) (m[d.slice(0,7)] = m[d.slice(0,7)]||[]).push(d);
  return m;
}

function ticks(t0, t1) {
  const steps=[60,120,300,600,900,1800,3600,7200,10800,21600,43200,86400];
  const dur=t1-t0; let step=steps[steps.length-1];
  for (const s of steps) if (dur/s<=9) { step=s; break; }
  const out=[]; let t=Math.ceil(t0/step)*step;
  while (t < t1-step*0.02) {
    const d=new Date(t*1000);
    out.push([t, step<3600||d.getHours()!==0 ? d.toTimeString().slice(0,5) : d.toLocaleDateString([], {month:'2-digit',day:'2-digit'})]);
    t+=step;
  }
  return out;
}

function tipHtml(rows) { return rows.map(([k,v])=>`<div class="t">${k}</div><div class="m">${v||''}</div>`).join(''); }
function bindTip(el, html) {
  el.addEventListener('mousemove', e => {
    e.stopPropagation();
    const t=$('#tip'); t.innerHTML=html; t.style.display='block';
    const x=Math.min(e.clientX+14, innerWidth-450), y=Math.min(e.clientY+14, innerHeight-160);
    t.style.left=x+'px'; t.style.top=y+'px';
  });
  el.addEventListener('mouseleave', ()=>$('#tip').style.display='none');
}

function renderMonths() {
  let h = '';
  for (const [m, days] of Object.entries(months())) {
    let nh=0, ns=0;
    for (const d of days) for (const id of DATA.days[d]) (DATA.sessions[id].kind==='sub'?ns++:nh++);
    h += `<div class="monthbtn ${state.month===m?'sel':''}" data-m="${m}">
      <div class="d">${m}</div><div class="c">${nh} top-level · ${ns} subagent</div></div>`;
  }
  $('#sec-months').innerHTML = `<h2>months</h2><div class="mrow">${h}</div>`;
  $('#sec-months').querySelectorAll('.monthbtn').forEach(el => el.onclick = () => {
    state.month = el.dataset.m; state.day = null; state.session = null;
    renderAll();
    $('#sec-days').scrollIntoView({behavior:'smooth'});
  });
}

function renderDays() {
  const sec = $('#sec-days');
  if (!state.month) { sec.innerHTML=''; return; }
  const days = months()[state.month] || [];
  let h = '';
  for (const d of days) {
    const ss = DATA.days[d].map(id => DATA.sessions[id]).filter(passesFilters);
    const nh = ss.filter(s=>s.kind!=='sub').length, ns = ss.filter(s=>s.kind==='sub').length;
    h += `<div class="daybtn ${state.day===d?'sel':''}" data-d="${d}">
      <div class="d">${d.slice(8)}</div><div class="c">${nh} top · ${ns} sub</div></div>`;
  }
  sec.innerHTML = `<h2>${state.month} — pick a day</h2><div class="drow">${h}</div>`;
  sec.querySelectorAll('.daybtn').forEach(el => el.onclick = () => {
    state.day = el.dataset.d; state.session = null;
    renderAll();
    $('#sec-day').scrollIntoView({behavior:'smooth'});
  });
}

function renderDayView() {
  const sec = $('#sec-day');
  if (!state.day) { sec.innerHTML=''; return; }
  const ids = (DATA.days[state.day] || []).filter(id => passesFilters(DATA.sessions[id]));
  if (!ids.length) { sec.innerHTML = `<h2>${state.day}</h2><p class="empty-hint">no sessions</p>`; return; }
  const day0 = new Date(state.day+'T00:00:00').getTime()/1000, day1 = day0+86400;
  const tk = ticks(day0, day1);
  const grid = tk.map(([t])=>`<div class="gl" style="left:${(t-day0)/86400*100}%"></div>`).join('');
  let rows = '';
  for (const id of ids) {
    const s = DATA.sessions[id];
    const segs = [...s.S].sort((a,b)=>a[1]-b[1]).map(sp => {
      const a = Math.max(spanStart(sp), day0), b = Math.min(spanEnd(sp), day1);
      if (b <= a) return '';
      return `<div class="seg ${spanKind(sp)}" data-si="${s.S.indexOf(sp)}" style="left:${(a-day0)/86400*100}%;width:${Math.max((b-a)/86400*100,0.05)}%"></div>`;
    }).join('');
    rows += `<div class="lane ${s.kind==='sub'?'sub-row':''}" data-id="${encodeURIComponent(id)}">
      <span class="lbl ${s.kind==='sub'?'sub':''}" title="${esc(s.title)}">${s.kind==='sub'?'↳ ':''}[${esc(s.profile||'?')}] ${esc(s.title)}</span>
      <div class="track">${grid}${segs}</div></div>`;
  }  const axis = tk.map(([t,l])=>`<span class="tick" style="left:${(t-day0)/86400*100}%">${l}</span>`).join('');
  sec.innerHTML = `<h2>${state.day} — all sessions on one clock</h2>
    <p class="meta">click a row to expand that session below · hover any segment for detail</p>
    ${rows}
    <div class="lane axisrow"><span class="lbl"></span><div class="track axis">${axis}</div></div>`;
  sec.querySelectorAll('.lane[data-id]').forEach(el => {
    const s = DATA.sessions[decodeURIComponent(el.dataset.id)];
    el.onclick = () => {
      state.session = s.id; renderAll();
      $('#sec-session').scrollIntoView({behavior:'smooth'});
    };
    bindTip(el, tipHtml([
      [`${s.kind==='sub'?'↳ ':''}${s.title}`, s.id],
      [`${dts(s.t_start)} → ${dts(s.t_end)} · wall ${fmt(s.t_end-s.t_start)}`, s.summary]
    ]));
  });
  // per-span hover: load this day's detail shard lazily, then bind segs
  const dayStr = state.day;
  const bindSpanTips = () => {
    sec.querySelectorAll('.lane[data-id] .seg').forEach(el => {
      const lane = el.closest('.lane[data-id]');
      const sId = decodeURIComponent(lane.dataset.id);
      const s = DATA.sessions[sId];
      const sp = s.S[+el.dataset.si];
      if (!sp) return;
      const d = spanMeta(sId, sp), m = d.m || {};
      const k = spanKind(sp);
      const title = k==='tool' ? (d.l||'tool')
        : k==='inference' ? 'model' : 'idle';
      const body = k==='tool' ? `args: ${(m.args||'').slice(0,140)}`
        : k==='inference' ? (m.output||'(tool-call only)').slice(0,140)
        : `you then said: ${(m.next_user||'').slice(0,140)}`;
      bindTip(el, tipHtml([
        [`${title} · ${fmt(spanEnd(sp)-spanStart(sp))}`, `${dts(spanStart(sp))} → ${dts(spanEnd(sp))}`],
        [body, '']
      ]));
    });
  };
  ensureDetail(dayStr, bindSpanTips);
}

function renderSessionView() {
  const sec = $('#sec-session');
  if (!state.session) { sec.innerHTML=''; return; }
  const s = DATA.sessions[state.session];
  if (!s) { sec.innerHTML=''; return; }
  const sId = state.session;
  const day = new Date(s.t_start*1000).toISOString().slice(0,10);
  const t0 = s.t_start, t1 = s.t_end, dur = Math.max(t1-t0, 1);
  const tk = ticks(t0, t1);
  const grid = tk.map(([t])=>`<div class="gl" style="left:${(t-t0)/dur*100}%"></div>`).join('');
  let rows = '';
  for (const kind of ['idle','inference','tool']) {
    const ks = s.S.filter(sp => spanKind(sp)===kind); if (!ks.length) continue;
    const segs = ks.map(sp => {
      return `<div class="seg ${kind}" data-i="${s.S.indexOf(sp)}" style="left:${(spanStart(sp)-t0)/dur*100}%;width:${Math.max((spanEnd(sp)-spanStart(sp))/dur*100,0.06)}%"></div>`;
    }).join('');
    rows += `<div class="lane"><span class="lbl">${kind}</span><div class="track">${grid}${segs}</div></div>`;
  }
  const axis = tk.map(([t,l])=>`<span class="tick" style="left:${(t-t0)/dur*100}%">${l}</span>`).join('');
  sec.innerHTML = `<div class="sess-block">
    <h2>${s.kind==='sub'?'↳ ':''}${s.title}</h2>
    <p class="summary">${s.summary}</p>
    <p class="meta">${sId} · ${dts(s.t_start)} → ${dts(s.t_end)} · ${s.S.length} spans · click a green segment for the tool call</p>
    ${rows}
    <div class="lane axisrow"><span class="lbl"></span><div class="track axis">${axis}</div></div>
  </div>`;
  const bindSegs = () => {
    sec.querySelectorAll('.seg.tool').forEach(el => {
      const sp = s.S[+el.dataset.i];
      const d = spanMeta(sId, sp), m = d.m || {};
      const name = d.l || 'tool';
      bindTip(el, tipHtml([
        [`${name} · ${fmt(spanEnd(sp)-spanStart(sp))}`, `${dts(spanStart(sp))} → ${dts(spanEnd(sp))}`],
        ['args', (m.args||'').slice(0,180)]
      ]));
      el.onclick = () => {
        const p = $('#panel');
        p.innerHTML = `<h3>${name}</h3>
          <div class="kv">${dts(spanStart(sp))} → ${dts(spanEnd(sp))} · ${fmt(spanEnd(sp)-spanStart(sp))}</div>
          <div class="kv">args</div><pre>${(m.args||'')||'(none)'}</pre>
          <div class="kv">result — this is what entered the model's context</div><pre>${(m.result||'')||'(empty)'}</pre>
          <div class="note">preview truncated at 220 chars; the harness may truncate further before it reaches the model</div>`;
        p.style.display = 'block';
      };
    });
    sec.querySelectorAll('.seg.idle').forEach(el => {
      const sp = s.S[+el.dataset.i];
      const d = spanMeta(sId, sp), m = d.m || {};
      bindTip(el, tipHtml([[`idle · ${fmt(spanEnd(sp)-spanStart(sp))}`, `you then said: ${(m.next_user||'').slice(0,160)}`]]));
    });
    sec.querySelectorAll('.seg.inference').forEach(el => {
      const sp = s.S[+el.dataset.i];
      const d = spanMeta(sId, sp), m = d.m || {};
      bindTip(el, tipHtml([[`model · ${fmt(spanEnd(sp)-spanStart(sp))}`, (m.output||'(tool-call only, no visible text)').slice(0,160)]]));
    });
  };
  ensureDetail(day, ok => bindSegs());
}

function renderAll() {
  $('#tip').style.display='none'; $('#panel').style.display='none';
  renderFilters(); renderMonths(); renderDays(); renderDayView(); renderSessionView();
}

renderAll();
</script></body></html>"""


if __name__ == "__main__":
    main()
