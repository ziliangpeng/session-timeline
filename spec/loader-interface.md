# Loader interface (Q2) — dedicated discussion doc

Status: OPEN — no decision yet. This doc exists to collect the options, the
prototype's evidence, and the decision criteria. The overview only fixes:
loaders are the single harness-specific seam; they emit the unified session
schema; everything above them is harness-agnostic.

## The question

What exactly is the loader interface — the contract between harness-specific
code and the harness-agnostic server?

## Candidate A — coarse: whole sessions by time window

```python
class Loader:
    def sessions_overlapping(self, t0: float, t1: float) -> list[Session]
```

One call returns complete unified-session objects (all spans + meta) for every
session overlapping the window.

- Prototype behavior today (Hermes + Prime loaders both implement this).
- Server-side, the prototype then splits into index/detail shards per day.
- Pros: trivial contract; a loader is ~70 lines; easy to reason about; easy
  testing (one function, synthetic fixtures).
- Cons: pulls everything in the window even if the user only looks at one day;
  the index/detail split becomes a server implementation detail that can drift
  from what loaders assume about usage.

## Candidate B — fine-grained: index + detail queries

```python
class Loader:
    def session_index(self, t0: float, t1: float) -> list[SessionSummary]  # no spans
    def session_detail(self, session_id) -> Session                        # spans + meta
    def day_detail(self, day: str) -> dict[session_id, list[SpanMeta]]     # ?
```

The server asks for summaries first, and materializes spans only for the
session/day the user actually opened.

- Pros: aligns the contract with the UI's actual access pattern (month listing
  → day view → one session); keeps memory small under Q1's iteration; each
  query is small and measurable.
- Cons: N+1 query patterns; per-day detail across profiles is awkward (a
  Hermes "day" query needs a join across all profile DBs; Prime has no day
  concept at all — it would have to scan); loaders become stateful or need
  their own caching; harder to test exhaustively.

## Candidate C — capability negotiation

Loaders advertise what they can do cheaply (e.g. `supports_day_query`,
`supports_incremental`), and the server picks a strategy per loader.

- Pros: honest about heterogeneous sources (SQLite can do indexed time-range
  queries; JSONL files can't).
- Cons: two code paths for every server feature; the "unified schema maximizes
  reusability" principle starts eroding at the loader seam.

## Evidence from the prototype (measured on a 3-month window, ~8.5k sessions)

- Full-window scan (both loaders, all sources): 13–24s, ~16MB index JSON +
  ~100MB detail. Fine for a TTL snapshot; way too slow for per-request.
- Per-day detail shard (what one day click needs): ~5ms once the snapshot
  exists; a fresh per-day pull would be dominated by opening 30 SQLite DBs
  (~0.2s each even for a tiny query).
- Prime has no random access: any "day query" degenerates to a full file scan
  per session file; caching JSONL parses in memory is the only way to make it
  fast.

## Decision criteria (what we should know before deciding)

1. Q1 iteration results: how big is the real resident set for index-only
   caching vs full snapshots?
2. Do we ever need cross-source queries the server can't compose from whole
   sessions (e.g. "all tool calls matching X across all profiles")?
3. How often do loaders get written? If new harnesses are rare, a simple
   contract beats an optimal one.

## Current lean (to be challenged)

Keep candidate A (coarse) for v1 — it is what the prototype validated, it
matches "start simple" (Q3 ruling), and the server-side index/detail split is a
server optimization that loaders stay blind to. Revisit if Q1 iteration shows
index-only residency is meaningfully cheaper, or a third harness appears whose
sources can't serve A cheaply.
