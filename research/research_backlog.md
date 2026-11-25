# Research backlog

Priority: Q = journey quality, P = performance. Ordered by expected impact / cost.

| ID | Pri | Idea | Rationale | Status |
| --- | --- | --- | --- | --- |
| B1 | Q | Headway-aware static boarding cost (expected wait = f(headway)) — owner approved TDX Bus/Schedule headways 2026-11-21; EXP-004 | Phase 1 cost is pure ride+walk+penalty; infrequent lines look as good as frequent ones, so the 24 retained corridors can be dominated by impractical options and Phase 2 never sees the practical one | parked (EXP-004): neutral live, revisit after B15 |
| B2 | Q | Replace LABELS_PER_STOP / structure-hash bag with true Pareto bags (cost, rides, walk) per round (McRAPTOR-style) | Current bag truncation is heuristic; may drop low-walk or low-transfer options | todo |
| B3 | P | (done: EXP-001) Dense integer stop/pattern indices instead of `HashMap<BoardingPointId,_>` and String-ID clones in the hot loop | Classic RAPTOR uses contiguous arrays (Delling et al. 2012); hashing + cloning Strings dominates small graphs | todo |
| B4 | P | Remove `format!("{:?}", steps)` sort keys in `select` / `corridor_order_key` | Allocation per comparison | todo |
| B5 | P | (done: EXP-001) Replace SipHash `DefaultHasher` fingerprints with FxHash/ahash | Hot path hashing | todo |
| B6 | P | Avoid O(n^2) structure dedup in `insert` / `scan_pattern` | Small n but executed per stop visit | todo |
| B7 | Q | Range-RAPTOR over a departure window in Phase 2 for timetable-backed modes (Metro/Rail) | Better departure choice, journey diversity across departures | todo |
| B8 | Q | Compare against Google/Apple Maps on the scenario set (daytime) | Find missed journeys | todo |
| B9 | P | Profile Phase 2 (evaluation) and reconstruction — likely network-bound (Valhalla/TDX), find redundant requests | End-to-end latency is what users see | todo |
| B10 | Q | Target pruning slack (50% + 600 s) validation | May prune good low-transfer corridors on long trips or keep too many on short | todo |
| B12 | Q | Stop-place equivalence: Taipei bus GTFS uses per-route stop IDs for one physical stop (捷運文德站(碧湖公園) = TPE23229/TPE23473/TPE11973/...). Equivalent legs key on exact stop ID, so N bus lines over the same stop pair consume N of 24 corridor slots | Observed in EXP-001 output: banqiao_to_neihu has 8 corridors differing only in the 63-79 s last bus line; taipei_main_to_xinyi has 4 copies of Metro R differing only in access stop ID | root-cause variant (place-level edges) = EXP-008 |
| B13 | Q | (EXP-003, discarded) Micro-ride suppression: 2-11 s bus rides used as walking substitutes (市政府(松智)>市政府(松壽) 2 s) because transfer edges are capped at 12 outgoing / 500 m | Ride dominated by crow-fly walk should not create a label | todo |
| B11 | Q | ULTRA-style multimodal shortcuts for bike-share legs | Literature: Baum et al. 2019 ULTRA | todo |
| B15 | Q | Combine equivalent-line frequency in the static wait (rate sum over patterns serving the same stop pair) | EXP-004 charges each pattern its own headway, so trunk segments served by many lines look as infrequent as one line | todo |
| B16 | Q | Rail/HSR static waits from per-stop departures (TRA/THSR are one island-wide route per direction) | EXP-004 leaves Rail/HSR without waits, biasing Phase 1 toward rail | todo |
| B17 | P | Exclude the static wait from the proportional target slack | EXP-004 p99 rises 247 → 302 ms because the 50% slack applies to a larger cost | todo |
| B18 | Q | Bus departures beyond the next realtime estimate for routes with no GTFS trips (TDX N1 gives only the next bus per stop) — needs owner decision: AGENTS.md forbids fabricating Bus trips from headways | EXP-005: after the parent-route fix, random_001 still walks because the one predicted 704 bus leaves before the access walk ends | owner approved 2026-11-23 (labelled estimates); measured in EXP-008 |
| B19 | P | Make `via-data fetch` retry a timed-out paced request instead of discarding the whole snapshot | 2026-11-23: two 40 min fetches lost to one 1 s DNS-bound connect timeout and one 5 s bike-page timeout | todo |
| B20 | Q | Per-leg timing label in the API (`realtime` / `scheduled` / `estimated`) | Owner requires estimates to be labelled; today only a journey-level `live_status_summary` exists | todo (part of B18 PR) |
| B21 | Q | Departure-window profile evaluation in Phase 2: a backward pass on each fixed corridor finds the latest origin departure with the same arrival (CSA/rRAPTOR profile idea, Dibbelt et al. 2018) | Today Phase 2 boards the first feasible trip and reports waits at stops; plan in designs.md | todo |
| B22 | Q | ULTRA-style YouBike shortcuts as Phase 1 transfer edges (refines B11) | Phase 1 cannot propose a mid-journey bike hop; plan in designs.md | todo |
| B23 | Q | Measure TDX N1 ETA error against later TDX observations; correct only if bias is large and stable | step 1 done on 81 evening buses (EXP-010): MAE 33 s at 0-5 min, 76 s at 5-10 min, bias small; rerun in daytime |
| B24 | Q | YouBike free-dock / bike forecast at planned pickup/dropoff time (static per-station flow rates + Poisson) | Drop-off is judged on now-availability 20+ min early; plan in designs.md | todo (needs 7-day log) |
| B25 | Q | True Pareto bags (cost, walk) per stop and round with ε-dominance (McRAPTOR/MCR; refines B2) | Current label truncation is heuristic; plan in designs.md | todo |
| B26 | Q | Place-aware fan-out of walking transfers: keep whole places nearest-first instead of the 12 shortest stops (EXP-009) | 69.5 % of stops hit the 12 cap, 74 % of kept edges go to a place already reached, 84 % of reachable pairs are dropped by the cap (Taipei core) | discarded 2026-11-25 (replay A/B on 38 scenarios: 1 better, 3 worse; EXP-009) |
| B27 | Q | Phase 1 boarding at any stop of the place reached (relaxation-level places), so one edge per place suffices | EXP-009 rule A: one edge per place hides other lines of that place; whole-place edges cost +13 % edges | idea |
| B28 | Q | Calibrate static Bus ride times: scale `derived` segments by a factor measured from TDX A2 vehicle progress (EXP-012: observed time since trip start is 2.9x the model, p10 2.1, p90 3.8, Taipei + New Taipei, 10:35) | Via claims 9 min for Sanchong to Ximen (Google 25); est/place wins depend on too-fast bus legs | measured; needs time-of-day runs, then engineer PR |
| B29 | Q | Replay harness for live A/B (`tools/tdx_record.py`, `tdx_replay.py`) | 73 of 80 plans reproduce; re-run hop/ranking/calibration changes with no TDX | done (prototype tooling) |
| B30 | Q | Express/intercity Bus coverage: Google's direct bus 948 (Banqiao bus station to Linkou Chang Gung) is not in the artifact (Via route 948 starts at Linkou) | banqiao_to_linkou +17 min vs Google | idea, check TDX InterCity/Bus routes |
| B31 | Q | (owner decision) Fill Metro lines the GTFS beta lacks (Wenhu BR has 0 trips) from a TDX Metro static source (S2STravelTime + Frequency) | 4 of 20 scenarios 60-118 min slower than Google; conflicts with 'GTFS is the only static source' (AGENTS.md) and the owner's 'do not work around upstream gaps' | not started, needs owner call |
