# Server architecture (production)

Status: DECIDED (first-principles review 2026-09-26; rulings by delegated
decision-maker Fable, measurements on real data — 30 Hermes DBs + 246 Prime
files, ~13.4k sessions/yr). The Python prototype informed the measurements but
is NOT a source of truth.

## Production contract

- **Local single binary** on the user's laptop, reading live harness data
  (`~/.hermes`, `~/.prime/agent/sessions`). No auth, no CORS, **binds
  127.0.0.1 only**. Remote deployment is a deliberate future decision, never
  scope creep. (Ruling 5; escalation pending Ziliang's one-line confirmation.)
- Timezone: server-local time (libc `localtime_r`), no parameter. Correct by
  definition for a local tool. (Ruling 4)

## Caching rule (amended 2026-09-26)

**No response caching. A persistent in-memory session INDEX with mtime-based
invalidation is permitted; all span content is read from disk on demand per
request.** Disk is truth; the index is an acceleration structure, not a data
copy. This amends the earlier "pure on-demand, no cache" ruling with measured
evidence: full 31-day scan is 4.0s / 1.4MB — too slow for every-request, while
an mtime sweep over all ~276 source files costs <1ms. (Ruling 6)

## In-memory index (built once at startup, kept warm)

- Contents: `Vec<SessionSummary>` (id, kind, title, profile, source, extent,
  span count) + **interval map** `session_id → (t_start, t_end)` + per-day
  counts (active-day bucketing, invariant I1).
- Built at server startup: the 4s build happens before any browser connects;
  first page load is ~0ms. (Ruling 1)
- Invalidation: every `/api/index` request stats all source files; any mtime
  change triggers a rebuild of ONLY the changed sources' contribution
  (per-source partial rebuild keyed by file path). No TTL — TTL is a proxy for
  "did the data change"; mtime answers the real question. (stat sweep: ~276
  files, <1ms)

## Endpoints

- `GET /api/index?days=N` — session summaries + active days from the in-memory
  index. Fast path: no disk reads beyond the stat sweep.
- `GET /api/day?date=YYYY-MM-DD` — sessions active that day. Implementation:
  filter the interval map for `[day_start, day_end)` overlap → read spans only
  for those session IDs. Cross-day sessions (measured ~1% of a day's rows) are
  caught exactly; the prototype-era 32-day-window heuristic is deleted.
  Payload keeps span meta inline (68% of payload by bytes — compresses hard;
  gzip enabled; ~5-6MB → well under 1MB on the wire). No per-span hover
  endpoint: request storms and hover jank to save localhost bandwidth isn't a
  trade worth making. (Ruling 2)
- `GET /api/session?id=...` — one session's full spans by id. O(1) via the
  interval map; same response shape as day spans. Keeps `/api/day` from
  doubling as a session fetcher. (Ruling 3)

## Non-goals (production v1)

- Remote deployment / auth / CORS (see contract).
- Multi-user.
- Response caching of any kind.
- Timezone parameterization.

## Spec conventions

- Decided rules carry their justifying measurement inline in a parenthetical
  (numbers live WITH the decision; a separate performance appendix decouples
  them and rots). `ui.md` carries only user-visible numbers (payload sizes,
  load times).
