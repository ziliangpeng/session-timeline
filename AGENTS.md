# AGENTS.md — session-timeline

Instructions for AI coding assistants working in this repo.

## ⛔ HARD RULE #1 — NO PERSONAL OR LOCAL CONTEXT LEAKS

This repo is (or may become) public. It processes data harvested from a personal
machine — agent transcripts, file paths, model names, hostnames, project names.
**None of that may ever land in this repo.** This applies to every surface, no
exceptions, no "just this once":

- **Code**: no real session IDs, no absolute paths from the author's machine
  (default paths like `~/.hermes/...` / `~/.prime/agent/...` are fine as SCHEMES),
  no real model names tied to the author's employer/self-hosted fleet, no host
  names, no usernames. Tests use synthetic fixtures ONLY (fabricated session
  records, fake paths under `/tmp/...` or `tests/fixtures/`).
- **Commits & branch names**: neutral, no hostnames/paths/employers in messages.
- **PR titles/bodies, issues, comments, releases, tags**: describe the FEATURE,
  never the incident or the user's setup that motivated it. Never paste real
  transcript excerpts, real tool outputs, real DB rows, real logs. Reproduce
  with a synthetic example instead.
- **Docs, README, CHANGELOG, docstrings**: same rule. "My session took 30
  minutes" is fine; quoting that session's content is not.
- **Fixtures**: anything under `tests/fixtures/` must be hand-written synthetic
  data. If a real artifact is needed for debugging, keep it OUTSIDE the repo
  (e.g. `/tmp/`) — it simply never enters the tree.
- **This file and skills/memory**: don't reference the author's employers,
  project codenames, or fleet details here either.

When unsure whether something is "personal/local context", it is. Rewrite the
contribution so it stands on synthetic data. A leak discovered later = rewrite
history (this repo is young; force-push is acceptable) rather than leave it.

## What this tool is

A postmortem visualizer for AI-agent sessions: given a session's persisted
artifacts, reconstruct a timeline of spans — `inference` (model thinking +
generating), `tool` (execution of one tool call), `idle` (waiting for the user),
`subagent` (delegated child work) — and render them.

Design doctrine (settled upstream of this repo, don't relitigate without a
strong reason):

1. **Pure external tool.** Read-only over existing artifacts; never modify the
   harness, never require harness changes. Works retroactively on old sessions.
2. **Hermes converter**: profile `state.db` `messages` timestamps are the base
   truth; `logs/agent.log` INFO lines (per-API-call latency, per-tool duration)
   enrich precision when present. Rotating logs are best-effort only — NEVER a
   required input.
3. **Prime converter**: append-only per-session JSONL; durations are DERIVED
   from timestamp gaps (toolCall→toolResult = tool span; toolResult→next
   assistant = inference span). Sub-second precision is not a goal — this is
   attribution, not profiling.
4. **Parallel tool calls**: pair by tool-call id, compute intervals, take the
   UNION — never naive subtraction, which double-counts overlapping spans.
5. **`idle` is first-class**: last assistant message → next user message gap,
   its own span kind and its own color. A timeline that hides waiting-for-user
   time lies.
6. **Unified span model** in the middle; renderers know nothing about harnesses.
   `{session, kind, t_start, t_end, label, meta}`.
7. **v1 renderer = Perfetto trace-event JSON** (zero UI code; ui.perfetto.dev
   renders it). Self-contained HTML and live dashboards are future work, in that
   order. The one-line text summary is a first-class deliverable.

## Repo conventions

- Python ≥ 3.11, stdlib-first; add a dependency only when it clearly pays rent
  (no pandas/numpy for what `sqlite3` + `json` do natively).
- Package layout: `src/session_timeline/` with one module per converter
  (`hermes.py`, `prime.py`), a shared `spans.py` (model + interval math), and
  `render/` (perfetto.py, summary.py). CLI entry: `python -m session_timeline`.
- Tests: pytest, synthetic fixtures only (see HARD RULE #1). Every converter
  test asserts SPAN CONTRACTS (ordering, union of overlaps, no negative
  durations, idle gaps correct), never snapshot diffs of fixture bytes.
- Default paths resolve from the OS home dir at RUNTIME (`~/.hermes`, etc.) —
  configurable via CLI flags/env. Never hardcode any absolute path.
- Commit style: conventional commits (`feat:`, `fix:`, `docs:`, ...).
- Branch → PR → merge; no direct pushes to `main` once a second contributor
  (human or agent) exists. Until then, small direct commits to `main` are OK.

## Development

```bash
python -m venv .venv && source .venv/bin/activate
pip install -e ".[dev]"
pytest
python -m session_timeline prime --session <id>   # emits trace.json + summary
```
