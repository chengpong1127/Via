# Promising approaches

- EXP-001 dense-index corridor search: p50 418→85 ms, p95 989→189 ms, identical output. PR https://github.com/chengpong1127/Via/pull/15 (branch `perf/dense-corridor-search`), awaiting review.

## EXP-004 static headway waits (parked 2026-11-21)

Branch `research/exp-004-headway-waits`. Offline: corridors on lines not running at the query time fall 92–98%; planning is faster. Live (40 paired Saturday plans): 5 earlier / 4 later / 31 same vs base, with no clear quality gain. Revisit after B15 (combined equivalent-line frequency) and EXP-002 place dedup; ship "no-service only" first if anything ships.
