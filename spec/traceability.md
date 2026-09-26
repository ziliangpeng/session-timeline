# data-read ↔ spec traceability matrix

How to verify the component honors the spec, and that tests cover it.
Every row: a normative spec statement → where it is implemented → the test(s)
that lock it. A missing cell = a coverage hole. Regenerate evidence with
`cargo test`, `cargo llvm-cov`, `cargo mutants` (see "Verification commands").

## Overview spec (spec/overview.md)

### Architecture — loaders are the only harness-specific seam

| # | Spec statement | Implementation | Locking tests |
|---|----------------|----------------|---------------|
| A1 | Loaders emit the unified session schema; server/UI never see a harness name outside display labels | `model.rs` (Session/Span/Meta, no harness types); loaders return `Vec<Session>` | all component tests (they consume only `Session`) |
| A2 | Unified schema: `Session { id, title, profile, source, kind, t_start, t_end, spans[] }` | `model.rs::Session` | q3_hermes_ids_are_namespaced, q3_prime_ids_are_namespaced |
| A3 | Only loaders are harness-specific (Hermes SQLite / Prime JSONL) | `loaders/hermes.rs`, `loaders/prime.rs`; nothing above them imports harness concepts | code review: `lib.rs` has no harness-specific logic |

### Span derivation semantics (spec/overview.md "Span derivation")

| # | Spec statement | Implementation | Locking tests |
|---|----------------|----------------|---------------|
| S1 | user msg → assistant msg (≤ gap cap) = INFERENCE | hermes.rs/prime.rs assistant branch, `GAP_CAP_S` in model.rs | q7_user_then_assistant_is_inference, prime assistant-only (5s inference) |
| S2 | assistant tool_call → tool result = TOOL, parallel calls timed individually | `pending` map keyed by call_id; per-call spans | q7_tool_call_pairing_by_call_id, q7_parallel_tools_are_timed_individually, prime_tool_call_span_from_jsonl |
| S3 | silence ended by a user msg = IDLE | user branch: prev→t idle span | q7_silence_over_gap_cap (360s idle), prime user-only (no idle before first msg) |
| S4 | silence > gap cap = IDLE (long silence), never inference | gap ≤ GAP_CAP_S check in both loaders | q7_silence_over_gap_cap_is_idle_not_inference, prime_long_silence_is_idle_not_inference |

### Correctness invariants (spec/overview.md, 6 invariants)

| # | Spec statement | Implementation | Locking tests |
|---|----------------|----------------|---------------|
| I1 | Sessions appear on every day they have activity on | **server concern** (day bucketing) — loader provides full extent + spans so any day touched is derivable | q2_session_overlapping_window_is_returned_unchanged (extent unclipped: spans reaching outside the window are preserved) |
| I2 | Session extent from span/message timestamps, never row order | `build_session` extent = min/max over spans (fallback: message ts) | q7_extent_from_span_extremes_not_row_order |
| I3 | Harness-injected user rows never count as human input | hermes: `platform_message_id IS NULL` filter for human count | q5_injected_user_messages_do_not_make_a_session_human |
| I4 | Silence above gap cap is idle, never inference | same as S4 | q7_silence_over_gap_cap, prime_long_silence |
| I5 | One malformed record never loses a profile's other sessions | row/session/source-level try-skip + `ScanStats` counters (Q4) | q4_broken_source_is_skipped_and_counted, q4_malformed_tool_calls_row_is_counted_not_fatal, q4_corrupt_prime_line_is_skipped_not_fatal, cli_broken_hermes_db_is_warning_not_failure |
| I6 | Compaction dead history excluded | `active=1` filter in messages query | q7_active0_rows_are_excluded |

### Open-question rulings folded in from grill sessions

| # | Decision (date) | Implementation | Locking tests |
|---|-----------------|----------------|---------------|
| Q1 | Pure on-demand, whole-session objects; no pre-cache (benchmarks 1d/0.17s/24MB … 92d/7.2s/366MB) | `scan()` returns everything in window; no caching layer | benchmarks in PR #5 description (not unit-testable; perf regression testing is future work) |
| Q2 | Overlap semantics: `[t_start,t_end] ∩ [t0,t1) ≠ ∅` → returned; half-open; NOT clipped | hermes session SQL WHERE; prime first/last check | q2_session_overlapping_window_is_returned_unchanged, q2_session_ending_before_window_is_excluded, q2_session_starting_after_window_is_excluded |
| Q3 | Namespaced ids `hermes:<profile>:<sid>`, `prime:<stem>` | id construction in both loaders | q3_hermes_ids_are_namespaced, q3_prime_ids_are_namespaced |
| Q4 | Degrade never fatal; skips counted | `ScanStats` (atomics), per-level skip | q4_* tests (3) |
| Q5 | kind = {human, sub} only; cron/oneshot stay in `source` | `SessionKind` two-variant enum; `source` passthrough | q5_cron_source_is_transparent_not_a_kind, q5_subagent_with_parent_is_sub, prime assistant-only → sub |
| Q6 | Previews capped 220 chars, whitespace collapsed | `PREVIEW_N` + `preview()` in both loaders | q6_previews_are_capped_and_whitespace_collapsed, hermes_long_tool_result_is_truncated_in_preview |
| Q7 | Six invariants each locked by a dedicated test | this table's I1–I6 rows | see above |
| Q8 | Parallelism is the component's internal job | rayon inside `scan`/`scan_all_stats` | not directly testable (behavioral: results identical single/multi source — hermes_scan_all_entry_point_works) |
| Q9 | Minimal public API: scan, scan_stats, scan_reported, Sources, ScanStats, model types | `lib.rs` public surface | compile-time (privacy); no test needed |
| Q10 | CLI never exits non-zero on skipped sources | `main.rs` always exit 0 after scan | cli_broken_hermes_db_is_warning_not_failure, cli_scan_json_prints_counts |

### loader-interface.md (candidate A implemented)

| # | Spec statement | Implementation | Locking tests |
|---|----------------|----------------|---------------|
| L1 | `sessions_overlapping(t0, t1) -> list[Session]` coarse contract | `scan(&Sources, t0, t1) -> Vec<Session>` (+ Stats/Reported variants) | every test calling scan_stats |
| L2 | Server-side index/detail split stays a server concern; loaders blind to usage | loaders take only (window); no index/detail APIs | code review: no such APIs exist |

## Known gaps (honest list)

- **I1 day bucketing**: implemented in the (Python) prototype server; the Rust
  server component doesn't exist yet. Loader-side support (unclipped extents)
  is tested; the bucketing itself gets tested when the server lands.
- **Q1 perf regression tests**: benchmarks recorded in the PR body, not
  automated. A `cargo bench`/criterion suite is future work.
- **GAP_CAP_S boundary** (exactly 300s): untested boundary; tests use 6/10-min
  silences. Mutation testing will flag this.
- **Prime `scan_dir`'s mtime prefilter**: superseded by first/last-line probes;
  the mtime branch is dead-ish code (coverage shows partially exercised) —
  candidate for deletion when mutants confirm.

## Mutation testing (evidence, 2026-09-25 local run)

202 mutants tested before the run was stopped for time: **157 caught, 45
missed → 77.7% kill rate**. The 45 survivors were triaged:

- **Killed by new tests** (tests/mutation_gaps.rs, 10 tests): scan() no-stats
  variant; ScanStats zero-on-healthy and counting-on-broken; union_duration
  arithmetic on disjoint spans; equal-timestamp zero-length spans; hermes
  meta fields (output/next_user/note); provided-title-wins; CLI sub counts.
- **Equivalent / acceptable survivors** (documented, not chased):
  - `ts()` operator mutants (`-`→`+`, `&&`→`||`): hit only on malformed or
    exotic timestamp shapes; the exercised paths are covered by the ts-variant
    tests, the mutants alter only unreachable-error behavior.
  - `scan_reported` `==`→`!=` on a filter: cosmetic count of prime files.
  - `main.rs` `+=`→`*=` on a display counter: printing only.
  - `first_line_timestamp`/`last_line_timestamp` internal bounds: probes are
    advisory (a wrong probe only costs performance, correctness is guarded by
    the post-read window check, which IS tested).
Re-running the full suite is a CI job (manual dispatch), not a local loop.

## Verification commands

```bash
cargo test                          # 64 tests, all green
cargo clippy --all-targets -- -D warnings   # 0 warnings (CI-enforced)
cargo llvm-cov --summary-only       # ~93% lines
# mutation testing: CI manual dispatch (Actions → rust → Run workflow → mutants)
#   gh workflow run rust.yml -f mutants=true
```

CI layout (.github/workflows/rust.yml):
- **test** (every PR + main push): cargo test, clippy -D warnings, fmt --check.
- **coverage** (main pushes only): cargo-llvm-cov summary.
- **mutants** (manual dispatch only, `inputs.mutants=true`): full mutation run,
  report uploaded as artifact (14-day retention).

Manual spot-check protocol (5 minutes):
1. `cargo test` green, clippy 0 warnings (CI enforces both on every PR).
2. Pick one row in a table above at random; open the test; read the assertion;
   confirm it would actually fail if the behavior regressed.
3. When in doubt about test strength (not just existence), dispatch the CI
   mutants job and read the survivors list against the tables above.
