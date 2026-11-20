# Failed experiments

Ideas tried and discarded. Do not retry without new evidence.

(none yet)

## EXP-003 — Prune walk-dominated rides (2026-11-21)
Micro-ride corridors only 496→426, best corridors unchanged, 25 fixture tests break (stops ~1 m apart). Root cause is the
missing boarding-wait term in static cost (B1), not geometry. Do not retry geometric pruning; revisit only if B1 lands and
micro-rides remain. Record: experiments/EXP-003-walk-dominated-rides.md
