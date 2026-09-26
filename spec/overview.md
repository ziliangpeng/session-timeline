# session-timeline — Overview

Status: SETTLED core (span derivation, architecture, invariants — all decided
and test-locked). This file is the entry point; details live in companion docs.

## Problem

Heavy agent users run many sessions across many harnesses and profiles —
interactive chats, bot-mode chats, cron jobs, subagent fan-outs. After the
fact, the wall-clock cost of a session is opaque:

- A "quick" request that felt like 5 minutes actually took 30. Why — model
  inference, tool execution, or waiting for the user to reply?
- Information overload: the transcript shows *what* happened but not *when* or
  *how long each phase took*.
- No way to see parallelism: which sessions were active at the same time, which
  work happened while the user was away.

There is no instrumentation to read — the answer must be reconstructed from
whatever the harness already persists.

## Goal

A tool that reconstructs and visualizes **where the time went** in AI-agent
sessions:

- Per session: inference vs tool execution vs waiting-for-user, with wall-clock
  alignment.
- Per day: all sessions on one shared clock, showing what the user actually
  worked on, in parallel, hour by hour.
- Drill-down to individual spans, including what a tool call did (name, args,
  result) — and what of it entered the model's context.

## Architecture (decided)

**Rust workspace, web server + web UI, querying source data on demand.**
Components: `crates/data-read` (loaders + CLI), `crates/web-server` (axum API +
embedded UI), `crates/perf` (benchmark tool, issue #8). The Python prototype in
`experimental/` is frozen as a historical reference, not a source of truth.

**Three layers, one harness-specific seam:**

```
┌─────────────────────────────────────────────────────────────┐
│  Web UI — single-page app, harness-agnostic                 │
│  (month → day → session drill-down, spans, filters)         │
├─────────────────────────────────────────────────────────────┤
│  Web server — JSON API, harness-agnostic                    │
│  (in-memory index, on-demand span reads, gzip)              │
├─────────────────────────────────────────────────────────────┤
│  Unified session schema — THE contract                      │
│  Session { id, title, profile, source, kind, t_start,       │
│            t_end, spans[ { kind, t_start, t_end,            │
│            label, meta } ] }                                │
├─────────────────────────────────────────────────────────────┤
│  Data loaders — the ONLY harness-specific layer             │
│  Hermes loader (SQLite state.db)   Prime loader (JSONL)     │
└─────────────────────────────────────────────────────────────┘
```

- After a loader pulls from a Hermes DB or a Prime DB, everything above the
  line is a **unified data object**. Server and UI never see a harness name
  outside of a display label.
- This maximizes reusability: server logic, UI components, tests, and future
  features (search, aggregation, export) are written once against the unified
  schema. Supporting a new harness = writing one loader.

**Span derivation (the unified schema's core semantics):**

```
user msg → assistant msg (≤ gap cap)   = INFERENCE
assistant tool_call → tool result      = TOOL      (parallel calls timed individually)
silence ended by a user msg            = IDLE
silence longer than the gap cap        = IDLE (long silence), never inference
```

## Correctness invariants (each locked by a test)

1. Sessions appear on **every day they have activity on**, not just their start
   day (long-lived chats must stay visible on later days).
2. Session extent derives from span/message timestamps, never row order
   (post-compaction rows can be out of order → negative durations).
3. Harness-injected user rows never count as human input (subagent sessions
   classify correctly; "waiting-for-user" is honest).
4. Silence above the gap cap is idle, never inference (self-driving periods
   must not be attributed to the model).
5. One malformed record never loses a profile's other sessions.
6. Compaction dead history (superseded rows) is excluded.

## Non-goals

- Instrumenting or modifying any harness.
- Perfect attribution — this is best-effort reconstruction; the derivation
  model documents every inference made.
- Real-time monitoring. This is a postmortem tool with a 5-minute refresh.

## Settled questions (formerly "open")

- **Q1 — Caching**: settled in `architecture.md` (in-memory index, mtime
  invalidation, serve-stale; span content always from disk).
- **Q2 — Loader interface**: settled in `loader-interface.md` (candidate A,
  coarse whole-session contract).
- **Q3 — Schema fields**: settled (minimal schema; `title` optional, degrade
  gracefully). Field semantics live in `data-model.md` if a second consumer
  ever needs them — not written until then (YAGNI).
- **Q4 — Detail payload boundaries**: settled in `architecture.md` (meta
  inline in day payloads, gzip; no per-span hover endpoint; 220-char previews).
- **Q5 — Live behavior**: UI polls every **5 minutes** (was 60s; user call
  2026-09-26 — too chatty). Responses carry a change signature; unchanged data
  causes zero DOM work. Push (SSE/websocket) remains a non-goal until a need
  appears.

## Companion docs

- `architecture.md` — server internals: production contract, index, endpoints,
  perf model
- `loader-interface.md` — the loader contract (DECIDED)
- `ui.md` — views, interactions, filters (user-visible behavior)
- `traceability.md` — spec → implementation → test matrix
- `adapters/hermes.md`, `adapters/prime.md` — per-source derivation details and
  edge cases (to be written when a loader change needs documentation; the
  loaders' doc-comments + edge-case tests carry this today)
