# Server architecture (production)

Status: DECIDED (first-principles review 2026-09-26; rulings by delegated
decision-maker Fable, measurements on real data — 30 Hermes DBs + 246 Prime
files, ~13.4k sessions/yr). The Python prototype informed the measurements but
is NOT a source of truth. Amended twice after live-UX incidents (also
2026-09-26): serve-stale + incremental rebuild, then index compression +
sig-gated refresh.

## Production contract

- **Local single binary** on the user's laptop, reading live harness data
  (`~/.hermes`, `~/.prime/agent/sessions`). No auth, no CORS, **binds
  127.0.0.1 only** — confirmed by the user merging PR #6, which carries this
  contract. Remote deployment is a deliberate future decision, never scope
  creep. (Ruling 5)
- Timezone: server-local time (libc `localtime_r`), no parameter. Correct by
  definition for a local tool. (Ruling 4)

## Caching rule

**No response caching. A persistent in-memory session INDEX with mtime-based
invalidation is permitted; all span content is read from disk on demand per
request.** Disk is truth; the index is an acceleration structure, not a data
copy. (Ruling 6, amending the earlier "pure on-demand, no cache" ruling with
measured evidence: full 31-day scan is 4.0s — too slow for every-request;
an mtime sweep over all ~276 source files costs ~1ms.)

## In-memory index

- Contents: per-source chunks (one Hermes profile DB or one Prime file each)
  holding `SessionSummary` rows (id, kind, title, profile, source, extent,
  span count) + per-day bucketing (active-day invariant I1).
- Built at server startup (~12-14s on real data, 13.4k sessions): the build
  happens before any browser connects; first page load is ~0ms. (Ruling 1)
- Invalidation: every request stats source files (~1ms). A changed file
  rebuilds only its chunk (per-source partial rebuild, Fable's original
  ruling) — with one measured caveat: `hermes:default` holds 96% of sessions,
  so a change to it still costs ~6.5s (issue #8 records this; session-level
  incrementality is the known follow-up if it ever matters).
- **Serve-stale (amendment after the live-UX incident)**: a request that finds
  the index stale answers IMMEDIATELY with the current snapshot and triggers a
  background refresh (single-flight guarded). No request ever blocks on a
  rebuild. The incident: an active agent writing its own DB made every poll
  trigger a full 13s blocking rebuild — the UI hung 10-13s per click.

## Endpoints

- `GET /api/index?days=N` — session summaries + active days from the in-memory
  index. Fast path: no disk reads beyond the stat sweep. **Gzip when the
  client sends Accept-Encoding** (browsers always do; 3.4MB → ~0.7MB measured)
  and includes a **change signature** `sig = count:total_spans:max_t_end` so
  the UI can skip re-rendering entirely when nothing changed.
- `GET /api/day?date=YYYY-MM-DD` — sessions active that day: filter the
  per-day bucket → read spans only for those session IDs from disk. Cross-day
  sessions are caught exactly; the prototype-era 32-day-window heuristic is
  deleted. Payload keeps span meta inline (compresses hard; gzip). No
  per-span hover endpoint: request storms and hover jank to save localhost
  bandwidth isn't a trade worth making. (Ruling 2)
- `GET /api/session?id=...` — one session's full spans by id; same response
  shape as day spans. Keeps `/api/day` from doubling as a session fetcher.
  (Ruling 3)

## UI refresh contract

Poll `/api/index` every **5 minutes** (user call 2026-09-26; was 60s). Compare
`sig`; unchanged ⇒ zero DOM work. Changed ⇒ re-render, and evict day caches
only for days whose sessions actually grew (no wholesale cache wipes, no
destroyed hover state).

## Performance model (measured, `crates/perf`, issue #8)

| measurement | real-data result |
|---|---|
| cold index build (startup) | ~8-12s (13.4k sessions, 278 sources) |
| incremental rebuild, small chunk | ~chunk size (fast) |
| incremental rebuild, `hermes:default` chunk | ~6.5s (96% of sessions — the caveat above) |
| stale-detection sweep (every request) | ~1ms |
| /api/index (gzip) | ~10ms server-side, ~0.7MB |
| /api/day | 15-180ms by day density |
| /api/session | ~2ms |

## Non-goals (production v1)

- Remote deployment / auth / CORS (see contract).
- Multi-user.
- Response caching of any kind.
- Timezone parameterization.
- Push updates (SSE/websocket) — polling + sig is sufficient.

## Spec conventions

- Decided rules carry their justifying measurement inline in a parenthetical
  (numbers live WITH the decision; a separate performance appendix decouples
  them and rots). `ui.md` carries only user-visible numbers.
- Perf claims are regenerable: `cargo run --release -p perf` (issue #8 tool).
