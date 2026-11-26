# EXP-016: where the plan latency goes, and prefetching walking cells during the TDX wait

Date: 2026-11-26 evening. Zero TDX requests (replay with a 300 ms injected response delay).

## Profile (span trace `VIA_CHROME_TRACE`, set A, 20 plans, concurrent scopes, 300 ms delay; tracing adds overhead)
p50 per phase: Candidate Discovery 103 ms, Corridor Search 148, Snapshot Loading (TDX) 319, Corridor Evaluation 305, Reconstruction 123.
About 48 Valhalla Route calls per plan, p50 14 ms each; Evaluate Corridor spans (23 per plan, p50 83 ms) overlap, so the Valhalla time is
spread behind the 8-way corridor concurrency. B33 (merge request waves) is withdrawn: with concurrent scopes a plan has one TDX wave
(plan p50 623 ms at a 300 ms delay is about one round trip plus compute).

## Idea
`walking_cell(anchor[i], anchor[i+1])` for every non-retained-walking corridor edge does not depend on timing, so start those
walks together with the TDX snapshot request and let the evaluator hit the request cache. (Prototype toggle `PREFETCH_WALKING`
on `research/exp-013-post-scale-check`; shipped form is PR #56.)

## Result (58 plans x 2 repetitions, n=116, separate process per variant, cache TTL 1 s, 300 ms delay; `results/exp016/`)
| variant | p50 | p90 | p95 | mean |
| --- | --- | --- | --- | --- |
| sequential scopes | 891 | 1066 | 1190 | 883 |
| sequential + prefetch | 840 | 967 | 1113 | 805 |
| concurrent scopes (#55) | 627 | 752 | 787 | 625 |
| concurrent + prefetch | 506 | 620 | 661 | 527 |
Results identical in all variants. Valhalla Route spans over set A: 957 with and without prefetch (no extra street load seen).
Unexplained: prefetch alone saves only 6 percent although the TDX wait (about 600-900 ms sequential) is longer than the evaluation
walks; not investigated (suspects: local Valhalla concurrency, evaluation CPU time that prefetch cannot hide).

## Decision
PR #56 `perf/prefetch-walking-cells`: modest, stacked on #55. Production TDX and Valhalla round trips are unmeasured.
