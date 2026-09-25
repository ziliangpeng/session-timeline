#!/usr/bin/env python3
"""Render spans JSON file(s) → one self-contained HTML timeline page.

Usage: python render_html.py sess_a.json sess_b.json ... out.html
"""
from __future__ import annotations

import json
import sys
from datetime import datetime
from html import escape
from pathlib import Path

KIND_COLOR = {
    "inference": "#6c8ef5",
    "tool": "#3fb28f",
    "idle": "#8a8f98",
    "subagent": "#c78af0",
    "gap": "#3a3f47",
}
ROW_ORDER = ["idle", "inference", "tool", "gap"]
TICK_STEPS_S = [60, 120, 300, 600, 900, 1800, 3600, 7200, 10800, 21600, 43200, 86400, 172800]


def _ticks(t0: float, t1: float) -> list[tuple[float, str]]:
    """(ts, label) pairs at a 'nice' interval — ≤ ~9 ticks across the range."""
    dur = t1 - t0
    step = TICK_STEPS_S[-1]
    for s in TICK_STEPS_S:
        if dur / s <= 9:
            step = s
            break
    first = int(t0 // step) * step + step
    out = []
    t = first
    while t < t1 - step * 0.02:
        dt = datetime.fromtimestamp(t)
        if step < 3600:
            label = dt.strftime("%H:%M")
        elif dt.hour == 0:
            label = dt.strftime("%m-%d")  # midnight tick carries the date
        else:
            label = dt.strftime("%H:%M")
        out.append((t, label))
        t += step
    return out


def main() -> None:
    out = sys.argv[-1]
    inputs = sys.argv[1:-1]
    blocks, scripts = [], []
    for idx, p in enumerate(inputs):
        d = json.loads(Path(p).read_text())
        spans = d["spans"]
        t0 = min(s["t_start"] for s in spans)
        t1 = max(s["t_end"] for s in spans)
        dur = t1 - t0
        title = escape(Path(p).stem)
        summary = escape(d.get("summary", ""))
        ticks = _ticks(t0, t1)

        def pct(t: float) -> float:
            return (t - t0) / dur * 100

        # gridlines + tick labels shared by every lane of this session
        grid = "".join(f'<div class="gl" style="left:{pct(t):.4f}%"></div>' for t, _ in ticks)
        axis = "".join(
            f'<span class="tick" style="left:{pct(t):.4f}%">{escape(l)}</span>' for t, l in ticks
        )

        rows = ""
        for kind in ROW_ORDER:
            ks = [s for s in spans if s["kind"] == kind]
            if not ks:
                continue
            lane = "".join(
                f'<div class="seg {kind}" style="left:{(s["t_start"]-t0)/dur*100:.4f}%;'
                f'width:{max((s["t_end"]-s["t_start"])/dur*100, 0.08):.4f}%" '
                f'title="{escape(s["label"])} · {_fmt(s["duration"])}"></div>'
                for s in ks
            )
            rows += (
                f'<div class="lane"><span class="lbl">{kind}</span>'
                f'<div class="track">{grid}{lane}</div></div>\n'
            )

        blocks.append(f"""
<section class="sess">
  <h2>{title}</h2>
  <p class="summary">{summary}</p>
  <p class="meta">{_ts_fmt(d.get("started_at"))} → {_ts_fmt(d.get("ended_at"))} · {len(spans)} spans · total {_fmt(dur)}</p>
  {rows}
  <div class="lane axisrow"><span class="lbl"></span><div class="track axis">{axis}</div></div>
</section>""")
        scripts.append(f"DATA[{idx}] = {json.dumps(d)};")

    html = f"""<!doctype html>
<html><head><meta charset="utf-8"><title>session timelines</title>
<style>
 body {{ font: 13px/1.45 -apple-system, sans-serif; margin: 24px; background: #14161a; color: #d7dae0; }}
 h1 {{ font-size: 16px; }}
 .sess {{ margin-bottom: 36px; }}
 h2 {{ font-size: 14px; margin: 0 0 2px; }}
 .summary {{ color: #9fd0ff; margin: 2px 0; font-family: ui-monospace, monospace; }}
 .meta {{ color: #7c828d; margin: 2px 0 8px; font-size: 12px; }}
 .lane {{ display: flex; align-items: center; margin: 3px 0; }}
 .lbl {{ width: 70px; color: #8a8f98; font-size: 11px; text-align: right; padding-right: 10px; }}
 .track {{ position: relative; flex: 1; height: 18px; background: #1e2127; border-radius: 4px; }}
 .seg {{ position: absolute; top: 1px; bottom: 1px; border-radius: 3px; min-width: 1px; }}
 .seg.inference {{ background: {KIND_COLOR["inference"]}; }}
 .seg.tool {{ background: {KIND_COLOR["tool"]}; }}
 .seg.idle {{ background: {KIND_COLOR["idle"]}; opacity: .55; }}
 .seg.gap {{ background: {KIND_COLOR["gap"]}; }}
 .seg:hover {{ outline: 1px solid #fff; }}
 .gl {{ position: absolute; top: 0; bottom: 0; width: 1px; background: #ffffff14; }}
 .axisrow {{ margin-top: 1px; }}
 .axis {{ height: 16px; background: none; }}
 .tick {{ position: absolute; top: 2px; transform: translateX(-50%); color: #7c828d; font-size: 10px; white-space: nowrap; }}
 .legend {{ margin-bottom: 20px; font-size: 12px; color: #8a8f98; }}
 .legend span {{ display: inline-block; width: 10px; height: 10px; border-radius: 2px; margin: 0 4px 0 12px; vertical-align: -1px; }}
</style></head><body>
<h1>Agent session timelines</h1>
<div class="legend">
  <span style="background:{KIND_COLOR['inference']}"></span>inference
  <span style="background:{KIND_COLOR['tool']}"></span>tools
  <span style="background:{KIND_COLOR['idle']}"></span>waiting for you
  <span style="background:{KIND_COLOR['gap']}"></span>gap &gt;5min (unattributed)
</div>
{''.join(blocks)}
<script>var DATA = []; {' '.join(scripts)}</script>
</body></html>"""
    Path(out).write_text(html)
    print(f"wrote {out} ({len(inputs)} sessions)")


def _fmt(sec: float) -> str:
    if sec >= 3600:
        return f"{sec/3600:.1f}h"
    if sec >= 60:
        return f"{sec/60:.0f}min"
    return f"{sec:.0f}s"


def _ts_fmt(ts) -> str:
    if not ts:
        return "?"
    return datetime.fromtimestamp(ts).strftime("%m-%d %H:%M")


if __name__ == "__main__":
    main()
