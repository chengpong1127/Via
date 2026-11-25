# VIA routing research log

Persistent state for the continuous routing-research loop. Read this first when resuming.

| File | Purpose |
| --- | --- |
| `research_backlog.md` | Prioritised ideas not yet tried |
| `explored_approaches.md` | Everything investigated, with outcome and link |
| `promising_approaches.md` | Retained / PR-worthy results |
| `failed_experiments.md` | Discarded ideas and why (do not retry) |
| `benchmark_results.md` | Baselines and per-experiment numbers, with commands |
| `scenarios.md` | Reference OD scenarios and external-comparison records |
| `experiments/EXP-NNN-*.md` | One record per experiment |
| `tools/` | Analysis scripts and the TDX record/replay harness (see EXP-011) |

## Ground rules
- TDX: at most one live request per minute, never concurrent; prefer the offline artifact.
- Offline Phase-1 benchmark: `cargo run --release --example corridor_bench -- <static.json> <out.jsonl> [iterations]`
  (`server/examples/corridor_bench.rs`, 100 deterministic Taipei OD pairs, haversine access approximation).
- Data: `server/data` in a worktree is a symlink to the main checkout's `server/data` (git-ignored, do not commit).

## Resume checklist (written 2026-11-25 14:15 Asia/Taipei)
- Replay tools: `tools/tdx_record.py <out.jsonl> 18766` (proxy; set `VIA_TDX_BASE_URL=http://127.0.0.1:18766/basic/` and the same token URL in the config), `tools/tdx_replay.py <record.jsonl> <vt_file> 18767 [miss.jsonl]`;
  `live_compare` on branch `research/exp-011-corrected-place-est` (`VIA_PIN_TIMES`, `VIA_VT_FILE`). Recordings: `results/exp011*-tdx-record.jsonl`.
- Missing time-of-day A2 runs: 15:00, 17:30, 18:30, 20:30 (`tools/a2_snap.py`; `server/docs/calibration/bus_ride_time.py collect` on the reference branch).
