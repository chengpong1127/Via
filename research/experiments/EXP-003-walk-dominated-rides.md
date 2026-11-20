# EXP-003 — Prune walk-dominated rides in Phase 1

- Branch: `research/exp-003-walk-dominated-rides` (code discarded; benchmark change kept)
- Status: **discarded** (marginal)
- Date: 2026-11-21

## Problem
22 % of multi-ride retained corridors (496/2241, EXP-002 output) contain a ride of ≤60 s, e.g. a 2 s bus hop
市政府(松智)→市政府(松壽).

## Hypothesis
Skip an arrival label when the crow-fly walk (×1.3 detour, 1.25 m/s) between boarding and alighting stop is no longer
than ride seconds + the boarding's overhead (same-stop transfer + transfer penalty).

## Results (100 scenarios, vs EXP-002)
- Micro-ride corridors 496 → 426; median states 144.9 k → 135.5 k; latency unchanged.
- Best corridor (static seconds + 300 s/transfer): unchanged in 96, worse in 1 (+196 s), better in 0.
- 25 corridor unit tests fail: fixtures place stops ~1 m apart, so every fixture ride is "walk dominated".

## Why discarded
Most surviving micro-rides are first rides or hops of a few hundred metres, where the static cost has no boarding
wait at all — a 22 s ride always beats a 6-min walk. The root cause is the missing wait/headway term (B1), not walking
geometry. Pruning by geometry treats the symptom and needs fixture rewrites.

## Follow-up
B1 headway-aware boarding cost. Blocker found: Taipei City buses (254, 669, 通勤7, 內科22…) have **0 GTFS scheduled
trips** in the TDX GTFS feed (realtime-only), so GTFS cannot supply headways for the lines that cause the problem.
TDX `Bus/Schedule` has per-route headways, but AGENTS.md: "GTFS is the only static transit source" → needs owner decision.
