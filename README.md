# session-timeline

Reconstruct and visualize **where the time went** in an AI-agent session: model
inference vs tool execution vs waiting-for-user. Pure external tool — reads the
harness's existing on-disk artifacts (SQLite state DBs, JSONL session logs, INFO
logs), never instruments the harness.

Two converters today:

- **Hermes Agent** — profile `state.db` (messages timestamps) + `agent.log`
  (per-call latency/duration enrichment, best-effort).
- **Prime Agent** — append-only per-session JSONL under `~/.prime/agent/sessions/`.

Output: a unified **span model** → Perfetto trace-event JSON (open at
<https://ui.perfetto.dev>) + a one-line time-attribution summary.

```
session 32min = inference 11min / tools 14min (terminal 9min) / waiting-for-you 7min
```

## Status

v0 skeleton — design settled (see AGENTS.md), converters not yet implemented.

## Non-goals

- Harness instrumentation (OpenTelemetry, code changes in Hermes/Prime).
- Live dashboards. Postmortem first; decide later from real usage.
- Any UI beyond the Perfetto interchange format (a self-contained HTML renderer
  may come later).

## Repo layout

```
src/session_timeline/   package code (span model, converters, renderers)
tests/                  pytest suite
```
