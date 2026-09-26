# Loader interface (Q2) — DECIDED

Status: **DECIDED (2026-09-25, grill Q1-Q10)** — candidate A, coarse
whole-session contract, locked by tests (see traceability.md). This doc keeps
the candidates and evidence that led to the decision; the decision itself is
no longer open.

## The decision

**Candidate A — whole sessions by time window.** Loaders emit complete unified
`Session` objects for every session overlapping the window. The server-side
index/detail split (in-memory index vs on-demand span reads) is a server
implementation detail that loaders stay blind to.

Rationale: trivial contract (~70 lines per loader), easy exhaustive testing
(one function, synthetic fixtures), matches "start simple" (Q3 ruling), and
the prototype validated it. Revisit triggers: a third harness whose sources
can't serve A cheaply, or index-only residency proving meaningfully cheaper.

## The Rust contract (as implemented)

```rust
pub fn scan(&Sources, t0: f64, t1: f64) -> Vec<Session>
pub fn scan_stats(&Sources, t0: f64, t1: f64, &ScanStats) -> Vec<Session>
pub fn load_session_by_id(&Sources, id: &str) -> Option<Session>   // server-support addition
// per-source: hermes::load_profile_db(db, profile, t0, t1, &ScanStats)
//             prime::load_file_by_stem(path)
```

- `scan`/`scan_stats`: sessions overlapping `[t0, t1)` (half-open, unclipped),
  full spans + meta. Parallelism (rayon) is internal.
- `load_session_by_id` + per-source loaders: added when the server needed
  on-demand single-session reads; the same coarse object shape, narrower
  scope. Profile identity is passed EXPLICITLY (never derived from paths —
  regression-locked by `by_id_profile_matches_scan_profile`).
- Degradation is never fatal: skips counted in `ScanStats` (sources/sessions/
  rows), CLI exits 0 on broken sources (Q4, Q10).

## Candidates considered

- **B — fine-grained (index + detail queries)**: aligned with UI access
  pattern, but N+1 patterns, per-day detail across profiles is awkward, Prime
  has no random access, harder to test. Rejected for v1.
- **C — capability negotiation**: honest about heterogeneous sources, but two
  code paths per server feature erodes "one unified schema". Rejected.

## Evidence that informed the decision (measured, 2026-09-25)

- Full-window scan (both loaders, all sources): benchmarked 1d/0.17s/24MB →
  92d/7.2s/366MB after Prime streaming + first/tail probes.
- Prime has no random access: any day query degenerates to a file scan;
  streaming + probes keep whole-file reads cheap enough for the coarse
  contract.
- The server's on-demand span reads (api_day/api_session) hit SQLite by
  session id and single Prime files — cheap enough that fine-grained loader
  APIs weren't needed.
