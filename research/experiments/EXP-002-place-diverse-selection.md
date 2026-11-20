# EXP-002 — Place-diverse corridor selection

- Branch: `research/exp-002-place-diverse-selection` (on top of EXP-001)
- Status: offline-positive, **pending live Phase 2 comparison** (daytime, ≤1 TDX plan/min)
- Date: 2026-11-21

## Problem
Taipei/New Taipei bus GTFS assigns per-route stop IDs to one physical stop (捷運文德站(碧湖公園) = TPE23229, TPE23473,
TPE11973, …). Equivalent legs (same mode/board/alight stop ID) therefore never merge across lines, and `select`'s
pattern-sequence pass treats each line over the same stop pair as a new corridor. On the 2026-11-20 artifact 34 % of
retained slots (mean 23.1 retained, 15.2 distinct by station names) duplicate another corridor's ride sequence.

## Hypothesis
Grouping stop IDs into places (same stop name, ≤100 m, union-find) and preferring corridors that are new in both
pattern sequence and place sequence gives Phase 2 more distinct options without losing the per-ride-count best and
per-mode-set picks.

## Implementation
- `DenseTopology::place(stop)`: same-name stops within 100 m share a place index.
- `select`: after ride-count and mode-set passes, a pass that takes candidates new in pattern sequence **and** place
  sequence; then the original pattern-sequence pass; then near-best fill.

## Offline results (100 scenarios, 2026-11-20 artifact)
| | place-unique corridors / query | distinct first rides | mode sets | p50 ms |
| --- | --- | --- | --- | --- |
| EXP-001 | 15.22 | 7.41 | 2.29 | 85 |
| EXP-002 | 21.15 | 9.84 | 2.29 | 90 (noise) |
82/100 scenarios change their retained set. `cargo test` passes (selection test needed the
pattern∧place ordering — a place-only pass picked boarding-stop variants of one line first).

## Risks (why not yet retained)
- Alternate lines over the same stop pair were the only way Phase 2 could pick "whichever bus comes first"
  (reconstruction requires boarding at the anchor's exact stop ID, `reconstruct/transit.rs:37`). Fewer of them can
  mean a later departure on the last-mile hop.
- Freed slots partly go to commuter-only services (內科22(上午), 南軟雙和(上午)) which Phase 2 drops when they don't run —
  wasted evaluation but no wrong answer. B1 (headway-aware cost) addresses this.

## Root-cause alternative (needs owner decision)
Let a Transit edge carry legs from co-located stops of the same place (anchor = place, not stop ID), so Phase 2 boards
the first arriving line. Requires anchor/continuity changes in evaluation and reconstruction.

## Next step
Daytime live comparison: `via-route --json` for the named scenarios, EXP-001 vs EXP-002 binaries, one plan per minute,
compare best arrival, transfers, walking, and option diversity.
