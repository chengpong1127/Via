# Failed experiments

Ideas tried and discarded. Do not retry without new evidence.

(none yet)

## EXP-003 — Prune walk-dominated rides (2026-11-21)
Micro-ride corridors only 496→426, best corridors unchanged, 25 fixture tests break (stops ~1 m apart). Root cause is the
missing boarding-wait term in static cost (B1), not geometry. Do not retry geometric pruning; revisit only if B1 lands and
micro-rides remain. Record: experiments/EXP-003-walk-dominated-rides.md

## EXP-009 rule B: whole-place fan-out of walking transfers (B26)
Replay A/B on 38 recorded live scenarios (2026-11-25): place+est 1 better / 3 worse by >30 s (cathay_to_dazhi +29 min), +5.5 % edges. Offline static-cost gains (15-19 better) did not carry over. Do not retry unless Phase 2 evaluates more corridors.
