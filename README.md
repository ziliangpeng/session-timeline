# session-timeline

Reconstruct and visualize **where the time went** in an AI-agent session: model
inference vs tool execution vs waiting-for-user. Pure external tool — reads the
harness's existing on-disk artifacts (SQLite state DBs, JSONL session logs), never
instruments the harness.

Two converters today:

- **Hermes Agent** — per-profile `state.db` (auto-discovered).
- **Prime Agent** — append-only per-session JSONL.

Both feed one **span model**; everything downstream (summaries, day bucketing, the
web UI) is harness-agnostic.

## Quick start (web UI)

```bash
pip install -e ".[web]"
python -m session_timeline.web.server
# open http://127.0.0.1:8766
```

The server discovers data sources at startup; nothing is bundled. Configuration
is env-only (no paths are hardcoded — see AGENTS.md HARD RULE #1):

| Env var | Default | Meaning |
|---|---|---|
| `SESSION_TIMELINE_HERMES_HOME` | `~/.hermes` | dir with `state.db` + `profiles/*/state.db` |
| `SESSION_TIMELINE_PRIME_DIR` | `~/.prime/agent/sessions` | dir with per-session `*.jsonl` (set empty to disable) |
| `SESSION_TIMELINE_PORT` | `8766` | listen port |
| `SESSION_TIMELINE_WINDOW_DAYS` | `92` | how far back to scan |
| `SESSION_TIMELINE_TTL_S` | `120` | cache TTL; background rebuild keeps requests instant |

## What the UI shows

- **Month → day → session drill-down** on one page (no navigation jumps).
- Day view: every session that had activity that day, aligned on a shared
  00:00–24:00 clock; rows sorted by most recent activity.
- Session view: the session's own extent fills the screen; hover any span for
  details; click a tool span for its arguments and result (what actually entered
  the model's context).
- Filters (defaults hide subagent and cron sessions): subagents, cron, oneshots,
  `<5min` sessions.
- Overnight idle (a session left open from the previous day) is ghosted.

## Span derivation model

```
user msg → assistant msg (≤ gap cap)   = INFERENCE
assistant tool_call → tool result      = TOOL      (parallel calls timed individually)
silence ended by a user msg            = IDLE
silence longer than the gap cap        = IDLE (long silence), never inference
```

Sessions appear on **every day they have spans on**, not just their start day —
long-lived chat sessions stay visible on each day you actually worked in them.

## Status

v0 prototype → first structured drop. The web app ships as a general server: it
reads only from discovered databases at runtime, opens them read-only, and never
writes or caches data to disk. No session data, no machine paths, no personal
context may ever be committed (see AGENTS.md).

## Non-goals

- Harness instrumentation (OpenTelemetry, code changes in Hermes/Prime).
- Perfect attribution — this is best-effort reconstruction from timestamps; the
  derivation model above documents every inference made.

## Repo layout

```
src/session_timeline/           package code
  spans.py                      span model + union math + summary line
  web/server.py                 FastAPI live server (read-only DB access)
  web/app_shell.html            single-page UI (no bundled data)
tests/                          pytest suite (synthetic fixtures only)
```
