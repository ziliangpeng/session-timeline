# UI — views, interactions, filters

Status: DESCRIPTIVE (documents shipped behavior; changes go through a PR that
updates this file). User-visible numbers live here; server internals live in
`architecture.md`.

## Views (drill-down chain)

1. **Months** — one button per month with session counts (top-level ·
   subagent). Click → days.
2. **Days** — one button per day with counts; only days with activity appear.
   Click → day view.
3. **Day view** — all sessions active that day on one shared 24h clock, one
   lane per session, sorted by latest activity within the day (latest first).
   Spans render as colored segments: idle (gray) / inference (blue) / tool
   (green). Idle spanning from a previous day renders translucent ("carryover"
   overnight idle). Click a row → session view.
4. **Session view** — the full session on its own clock, one lane per span
   kind, with tick marks and grid lines.

On first load the UI auto-selects the latest day that has data.

## Interactions

- **Hover any span** → tooltip: kind, duration, absolute times, and content —
  tool: name + args preview; inference: model output preview; idle: what the
  user said next (or a note).
- **Click a tool span** → detail panel: full args and the tool result — what
  actually entered the model's context (previews capped at 220 chars per
  Q6; the harness may truncate further).
- **Click a session row** → session view below.

## Filters (top bar; off = hidden)

| filter | default | hides |
|---|---|---|
| subagent | off | subagent sessions (kind=sub) |
| cron | off | cron-sourced sessions |
| oneshot | off | oneshot-sourced sessions (hidden by default per user call 2026-09-26) |
| &lt;5min sessions | on* | sessions shorter than 5 minutes |

*short-session filter follows the prototype default: shown. Sub/cron/oneshot
are hidden by default — the timeline is for YOUR working sessions; machine
chatter stays out of the way.

## Live behavior

Polls `/api/index` every **5 minutes**. The response carries a change
signature; if nothing changed, the DOM is untouched (hover state survives).
When data changed, only days whose sessions grew lose their cache.

## Data contract the UI relies on

- `/api/index?days=N` → `{ sessions: [{id, title, profile, source, kind,
  t_start, t_end, span_count, days[]}], sig, ... }` (gzipped)
- `/api/day?date=YYYY-MM-DD` → `{ date, sessions: [full Session objects] }`
  (gzipped)
- `/api/session?id=...` → `{ session }` (gzipped)
- Session ids: `hermes:<profile>:<sid>` / `prime:<stem>` (Q3).
