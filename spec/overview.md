# session-timeline — Overview

Status: DRAFT (incremental spec; this file is the entry point, details land in
companion docs as they are settled)

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

**Web server + web UI, querying source data on demand.** No pre-pull, no baked
artifacts, no scheduled exports: the server reads from the harness databases at
request time and serves the UI from live queries.

**Three layers, one harness-specific seam:**

```
┌─────────────────────────────────────────────────────────────┐
│  Web UI — single-page app, harness-agnostic                 │
│  (month → day → session drill-down, spans, filters)         │
├─────────────────────────────────────────────────────────────┤
│  Web server — JSON API, harness-agnostic                    │
│  (on-demand queries, day bucketing, summaries, detail)      │
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

## Correctness invariants (each validated in the prototype by a test or a bug)

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
- Real-time monitoring (< ~2 min staleness). This is a postmortem tool.

## Open questions

### Q1 — How strict is "no pre-pull"? (needs a ruling)

Scanning every DB on every request is seconds-slow (a full window scan took
13–24s in the prototype). The prototype compromise: a TTL snapshot cache
(rebuild in background every ~120s, requests always serve the current snapshot
instantly, nothing persisted to disk, no scheduled jobs). Is a short-lived
in-memory cache acceptable under "on-demand, no pre-pull", or must every
request hit the DBs directly (and we invest in per-DB incremental queries /
narrower time windows to make that fast)?

### Q2 — Where exactly is the loader seam?

Candidate A: loader = "give me all sessions overlapping [t0, t1]" (whole
session objects, one shot). Candidate B: loader = finer-grained query interface
(sessions index separately from span detail), letting the server ask for a
day's detail without materializing everything. The prototype shipped A with a
server-side index/detail split on top. Is the split part of the unified schema
contract, or a server-internal optimization that loaders don't know about?

### Q3 — Unified schema: required fields and their meaning

Current shape: `id, title, profile, source (tui/cli/cron/…), kind (human/sub),
t_start, t_end, spans[]`. Grills: is `title` required (Prime has no title —
prototype derives one from the first user message)? Should `kind` classification
(human vs subagent) be a loader responsibility or a server rule? Do we need
`profile` as a first-class field (Hermes-specific concept) or a generic
"origin" string?

### Q4 — Detail payload boundaries

Tool args/results can be huge. Prototype: index payload has no meta strings;
per-day detail shards carry span labels + args/result previews (220-char cap).
Questions: is 220 chars the right preview cap? Should full results be a
per-span on-demand query (new endpoint) rather than shipped in day shards? Do
we ever show content beyond what the harness itself fed the model?

### Q5 — Live behavior

UI polls every 60s. Is polling fine, or do we want push (SSE/websocket) later?
Does a session currently in progress need to appear before it ends?

## Companion docs (to be written)

- `data-model.md` — unified session schema fields, precision, invariants
- `ui.md` — views, interactions, filters
- `architecture.md` — server internals, caching, packaging
- `adapters/hermes.md`, `adapters/prime.md` — per-source derivation details and
  edge cases
