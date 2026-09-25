# experimental/ — prototype playground

Everything here is prototype-grade on purpose: simple, standalone, runnable in
development mode. Structure is deliberately flat — no packages, no install step.

## Run the live web prototype (dev mode)

```bash
python experimental/server.py
# open http://127.0.0.1:8766
```

Requirements (into any venv): `pip install -r experimental/requirements-server.txt`
(fastapi, uvicorn). Data sources are discovered at runtime and opened read-only:

- `SESSION_TIMELINE_HERMES_HOME` (default `~/.hermes`) — `state.db` + `profiles/*/state.db`
- `SESSION_TIMELINE_PRIME_DIR` (default `~/.prime/agent/sessions`) — per-session `*.jsonl`

## Run the tests

```bash
cd experimental && pytest test_web_scan.py
```

## Other entry points (older prototypes)

- `build_app.py` — bake a static `app.html` (index + per-day detail shards) from DBs
- `hermes_extract.py` / `prime_extract.py` — single-session spans JSON extractors
- `render_html.py` — spans JSON(s) → self-contained HTML page

## Rules

- **No real session data in git.** `sess_*.json`, generated HTML, `detail/`,
  logs, and any `*.db` are gitignored. If a real artifact is needed for
  debugging, keep it in `/tmp/` — it never enters the tree (AGENTS.md #1).
# protection smoke test
