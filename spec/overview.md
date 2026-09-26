# session-timeline — Overview

Status: DRAFT (incremental spec; this file is the entry point, details land in
companion docs as they are settled)

## Problem

Heavy agent users run many sessions across many harnesses and profiles
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

## Approach (settled by prototyping)

**Pure external tool.** No harness changes, no OpenTelemetry, no code changes to
Hermes or Prime. We read existing on-disk artifacts only:

- Hermes Agent: per-profile SQLite `state.db` (messages, timestamps, tool
  calls), discovered at runtime.
- Prime Agent: append-only per-session JSONL event logs.

**Unified span protocol.** Every adapter converts its source into one shape:

```
user msg → assistant msg (≤ gap cap)   = INFERENCE
assistant tool_call → tool result      = TOOL      (parallel calls timed individually)
silence ended by a user msg            = IDLE
silence longer than the gap cap        = IDLE (long silence), never inference
```

Everything downstream (summaries, day bucketing, the web UI) is
harness-agnostic. Supporting a new harness = writing one adapter.

**Web app.** Live server reads DBs read-only on demand; single-page UI with
month → day → session drill-down, per-span hover, tool-call inspection, and
filters (subagents, cron, oneshots, short sessions).

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

## Open questions (to settle incrementally)

- [ ] Primary audience: personal tool for a single power user vs general
      open-source tool for anyone running these harnesses? (affects packaging,
      config surface, auth, multi-machine)
- [ ] v1 scope: Hermes + Prime adapters only, or design the adapter API first
      and ship one adapter?
- [ ] Deployment shape: local dev command only, or also a long-running service?
- [ ] Where does the historical prototype under `experimental/` get promoted
      into the real package layout?

## Companion docs (to be written)

- `data-model.md` — span protocol fields, precision, schema
- `ui.md` — views, interactions, filters
- `architecture.md` — adapters, server, caching, packaging
- `adapters/hermes.md`, `adapters/prime.md` — per-source derivation details and
  edge cases
