# Research backlog

Priority: Q = journey quality, P = performance. Ordered by expected impact / cost.

| ID | Pri | Idea | Rationale | Status |
| --- | --- | --- | --- | --- |
| B1 | Q | Headway-aware static boarding cost (expected wait = f(headway in query window)) | Phase 1 cost is pure ride+walk+penalty; infrequent lines look as good as frequent ones, so the 24 retained corridors can be dominated by impractical options and Phase 2 never sees the practical one | todo |
| B2 | Q | Replace LABELS_PER_STOP / structure-hash bag with true Pareto bags (cost, rides, walk) per round (McRAPTOR-style) | Current bag truncation is heuristic; may drop low-walk or low-transfer options | todo |
| B3 | P | Dense integer stop/pattern indices instead of `HashMap<BoardingPointId,_>` and String-ID clones in the hot loop | Classic RAPTOR uses contiguous arrays (Delling et al. 2012); hashing + cloning Strings dominates small graphs | todo |
| B4 | P | Remove `format!("{:?}", steps)` sort keys in `select` / `corridor_order_key` | Allocation per comparison | todo |
| B5 | P | Replace SipHash `DefaultHasher` fingerprints with FxHash/ahash | Hot path hashing | todo |
| B6 | P | Avoid O(n^2) structure dedup in `insert` / `scan_pattern` | Small n but executed per stop visit | todo |
| B7 | Q | Range-RAPTOR over a departure window in Phase 2 for timetable-backed modes (Metro/Rail) | Better departure choice, journey diversity across departures | todo |
| B8 | Q | Compare against Google/Apple Maps on the scenario set (daytime) | Find missed journeys | todo |
| B9 | P | Profile Phase 2 (evaluation) and reconstruction — likely network-bound (Valhalla/TDX), find redundant requests | End-to-end latency is what users see | todo |
| B10 | Q | Target pruning slack (50% + 600 s) validation | May prune good low-transfer corridors on long trips or keep too many on short | todo |
| B11 | Q | ULTRA-style multimodal shortcuts for bike-share legs | Literature: Baum et al. 2019 ULTRA | todo |
