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
