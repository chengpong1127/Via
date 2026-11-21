# Research backlog

Priority: Q = journey quality, P = performance. Ordered by expected impact / cost.

| ID | Pri | Idea | Rationale | Status |
| --- | --- | --- | --- | --- |
| B1 | Q | Headway-aware static boarding cost (expected wait = f(headway)) — owner approved TDX Bus/Schedule headways 2026-11-21; EXP-004 | Phase 1 cost is pure ride+walk+penalty; infrequent lines look as good as frequent ones, so the 24 retained corridors can be dominated by impractical options and Phase 2 never sees the practical one | in progress (EXP-004) |
| B2 | Q | Replace LABELS_PER_STOP / structure-hash bag with true Pareto bags (cost, rides, walk) per round (McRAPTOR-style) | Current bag truncation is heuristic; may drop low-walk or low-transfer options | todo |
| B3 | P | (done: EXP-001) Dense integer stop/pattern indices instead of `HashMap<BoardingPointId,_>` and String-ID clones in the hot loop | Classic RAPTOR uses contiguous arrays (Delling et al. 2012); hashing + cloning Strings dominates small graphs | todo |
| B4 | P | Remove `format!("{:?}", steps)` sort keys in `select` / `corridor_order_key` | Allocation per comparison | todo |
| B5 | P | (done: EXP-001) Replace SipHash `DefaultHasher` fingerprints with FxHash/ahash | Hot path hashing | todo |
| B6 | P | Avoid O(n^2) structure dedup in `insert` / `scan_pattern` | Small n but executed per stop visit | todo |
| B7 | Q | Range-RAPTOR over a departure window in Phase 2 for timetable-backed modes (Metro/Rail) | Better departure choice, journey diversity across departures | todo |
| B8 | Q | Compare against Google/Apple Maps on the scenario set (daytime) | Find missed journeys | todo |
| B9 | P | Profile Phase 2 (evaluation) and reconstruction — likely network-bound (Valhalla/TDX), find redundant requests | End-to-end latency is what users see | todo |
| B10 | Q | Target pruning slack (50% + 600 s) validation | May prune good low-transfer corridors on long trips or keep too many on short | todo |
| B12 | Q | Stop-place equivalence: Taipei bus GTFS uses per-route stop IDs for one physical stop (捷運文德站(碧湖公園) = TPE23229/TPE23473/TPE11973/...). Equivalent legs key on exact stop ID, so N bus lines over the same stop pair consume N of 24 corridor slots | Observed in EXP-001 output: banqiao_to_neihu has 8 corridors differing only in the 63-79 s last bus line; taipei_main_to_xinyi has 4 copies of Metro R differing only in access stop ID | EXP-002 offline done; live pending |
| B13 | Q | (EXP-003, discarded) Micro-ride suppression: 2-11 s bus rides used as walking substitutes (市政府(松智)>市政府(松壽) 2 s) because transfer edges are capped at 12 outgoing / 500 m | Ride dominated by crow-fly walk should not create a label | todo |
| B11 | Q | ULTRA-style multimodal shortcuts for bike-share legs | Literature: Baum et al. 2019 ULTRA | todo |
| B15 | Q | Combine equivalent-line frequency in the static wait (rate sum over patterns serving the same stop pair) | EXP-004 charges each pattern its own headway, so trunk segments served by many lines look as infrequent as one line | todo |
| B16 | Q | Rail/HSR static waits from per-stop departures (TRA/THSR are one island-wide route per direction) | EXP-004 leaves Rail/HSR without waits, biasing Phase 1 toward rail | todo |
| B17 | P | Exclude the static wait from the proportional target slack | EXP-004 p99 rises 247 → 302 ms because the 50% slack applies to a larger cost | todo |
