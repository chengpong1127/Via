# Issue #8 — Routing algorithms for Reliable Arrival

Status: Historical Research

## Decision

Use **Multi-criteria Range RAPTOR (McRAPTOR) for the Bus/Metro timetable core, with a time-dependent graph search for Walking and YouBike access, egress, direct travel, and station-to-station bicycle movement**.

Do not put the entire Journey into one graph, and do not model current YouBike inventory as a permanent timetable edge. Build each Journey Option from versioned query-time inputs:

1. a static transit timetable;
2. a pinned Live Status snapshot containing delays, cancellations, available-bike counts, and return-slot counts;
3. Walking/YouBike graph searches that produce access, egress, direct, or allowed intermediate Legs; and
4. a bounded McRAPTOR search that produces Pareto candidates and parent data for Guidance.

Use hard feasibility rules for Reliable Arrival, then rank the remaining Journey Options. In particular, require each Transfer to satisfy the applicable minimum time plus a safety buffer, and require a YouBike pickup/return station to pass query-time inventory thresholds. Treat inventory age and prediction uncertainty as evidence affecting reliability, not as certainty about future inventory.

This recommendation is an architectural inference from the sources below. RAPTOR was designed for dynamic public transit and native Pareto output, while a graph naturally represents Walking and YouBike state transitions. OpenTripPlanner independently uses this same broad split: its source documentation separates a RAPTOR transit structure from the non-transit graph and maps access/egress into the transit search ([OTP RAPTOR package documentation](https://raw.githubusercontent.com/opentripplanner/OpenTripPlanner/dev-2.x/raptor/src/main/java/org/opentripplanner/raptor/package.md)).

## Why this is the best fit

RAPTOR directly matches the hard part of Via's problem. One round corresponds to another transit boarding, so it computes arrival-time/Transfer trade-offs without a priority queue. The original algorithm returns the Pareto set for arrival time and number of Transfers; McRAPTOR generalizes labels to additional criteria, and Range RAPTOR covers a departure window ([RAPTOR paper, pp. 1, 5–6](https://www.microsoft.com/en-us/research/wp-content/uploads/2012/01/raptor_alenex.pdf)). It uses simple, contiguous timetable data and does not depend on heavyweight preprocessing; the paper explicitly describes delays, cancellations, and service-pattern changes as dynamic inputs it can handle.

That does **not** mean every concern belongs in a McRAPTOR label. McRAPTOR keeps a bag of nondominated labels at every stop and round. The original evaluation found those bags materially more expensive, and OpenTripPlanner's implementation notes show rapid growth as criteria are added: in its illustrative benchmark, a four-criterion search took about 400 ms versus 80 ms for Range RAPTOR, and adding Walking distance raised it to about 1,000 ms ([RAPTOR paper, pp. 5, 7](https://www.microsoft.com/en-us/research/wp-content/uploads/2012/01/raptor_alenex.pdf), [OTP RAPTOR package documentation](https://raw.githubusercontent.com/opentripplanner/OpenTripPlanner/dev-2.x/raptor/src/main/java/org/opentripplanner/raptor/package.md)). Reliable Arrival should therefore be a feasibility contract plus a compact ranking signal, not a collection of independent Pareto axes.

## Comparison

| Concern | RAPTOR / McRAPTOR | Connection Scan Algorithm (CSA) | Time-dependent graph search |
|---|---|---|---|
| Live delays and cancellations | **Strong.** No heavyweight preprocessing; update trip times/cancellations in the query snapshot. | **Strong.** Known delays are easy to incorporate, but the departure-ordered connection array must remain sorted; rebuilding/sorting is the simple safe path. | **Mixed.** Edge functions naturally hold predictable time variation, but unpredictable Live Status changes are repeated reoptimization; acceleration indexes may need repair or bypass. |
| Transfer feasibility and risk | **Strong for deterministic buffers.** Rounds expose boardings; stop-, mode-, or Transfer-specific slack is direct. Stochastic failure probability needs a custom label/cost model. | **Strongest formal stochastic option.** CSA includes Minimum Expected Arrival Time (MEAT), which plans a policy against uncertain vehicle delays. It requires calibrated delay distributions and produces a contingency set, not merely one Journey Option. | **Possible but custom.** State-expanded nodes/labels can represent slack and risk, at the cost of more labels and dominance work. |
| Bus/Metro filters | **Strong.** Exclude disallowed transit patterns before scanning; Walking remains an access/egress connector. | **Adequate.** A query may skip disallowed connections, but scanning a mixed global array wastes bandwidth unless per-filter views are maintained. | **Strong.** Travel Mode is a state/edge predicate, though a larger state graph remains. |
| YouBike pickup and return constraints | **Weak inside the timetable core; strong in the recommended hybrid.** Query-time access/egress labels can be admitted only when both endpoint stations satisfy inventory rules. Arbitrary transit–YouBike–transit sequences require extra orchestration or state. | **Weak without extension.** A bicycle movement is not a scheduled connection; adding it breaks CSA's single ordered-array simplicity. | **Strong.** Rental state, pickup, bicycle movement, and return are natural graph transitions; Live Status gates pickup/return edges. |
| Pareto Journey Options | **Best native fit.** Arrival time and Transfers are native; McRAPTOR adds carefully bounded criteria; Range RAPTOR covers departure windows. | **Good.** Profile CSA has Pareto variants, including Transfers, but its simplest and fastest form is earliest-arrival only. | **Possible but most complex.** Multi-label Dijkstra/A* is needed; label growth and dominance logic are application code. |
| Update cost | **Low.** Pin an immutable schedule-plus-Live-Status view per query; no global search index is required. Watch for delay-induced trip overtaking within a transit pattern. | **Low-to-moderate.** Sorting is lightweight, and the paper reports full data-structure setup as an upper bound on update time; incremental ordering is still an engineering concern. | **Low for unaccelerated search, potentially high for indexed search.** Updated edge functions are simple; invalidated preprocessing is not. |
| Explainability / Guidance | **Strong.** Parent references naturally reconstruct Legs and Transfers by round. | **Strong.** Journey extraction uses stored entry/exit connections and footpaths. | **Strong.** Predecessor edges reconstruct a Journey, but graph-level details need more semantic mapping to produce rider-facing Guidance. |
| Practical Rust path | **Best starting point, but still custom work.** `vulture` is an Apache-2.0/BlueOak-licensed Rust RAPTOR implementation with parallel and WASM support; it is evidence that the data layout is practical, not a complete Via engine ([source repository](https://github.com/urschrei/vulture)). | **Custom implementation likely.** The core scan is small, but this research found no comparably scoped authoritative Rust implementation to adopt; profiles, MEAT, and live reordering are the real work. | **Libraries/data structures are conventional, but Via's multimodal state model would be custom.** A generic shortest-path crate does not supply GTFS semantics, Pareto policy, or YouBike reliability rules. |

### Evidence behind the comparison

The CSA paper describes a single array of elementary timetable connections scanned in time order. It supports earliest arrival, profile/Pareto queries, known delays, and MEAT for uncertain future delays. Its experiments also treat sorting/setup time as an upper bound on timetable update cost and describe journey extraction via stored connection pointers ([CSA paper](https://arxiv.org/abs/1703.05997)). This makes CSA unusually simple and gives it the best published reliability extension of the three candidates. Its mismatch is multimodal composition: Walking is handled as footpaths, but query-time bicycle rental state and street search are not elementary scheduled connections.

A time-dependent shortest-path model assigns each graph edge an arrival-time function. Its useful polynomial-time properties depend on FIFO: entering an edge later cannot lead to arriving earlier. The theory distinguishes predictable time-dependent functions from frequent unpredictable updates, which are a sequence of related reoptimization problems ([Dean, *Shortest Paths in FIFO Time-Dependent Networks*, pp. 1, 4–5](https://people.csail.mit.edu/bdean/tdsp.pdf)). This model is excellent for streets and explicit rental state, but it gives Via no native advantage for timetable rounds or Pareto alternatives. The RAPTOR paper's own evaluation found its timetable-specific scan faster than baseline graph-based multi-label approaches on the tested urban networks; those historical timings are directional evidence, not a performance promise for Taipei today.

## Reliable Arrival semantics

### Live transit

GTFS Realtime `TripUpdate` can provide absolute arrival/departure times or delays, cancellations, added service, skipped stops, and prediction uncertainty. Missing updates must not be read as “on time”; the specification says they mean no realtime data is available. A supplied delay propagates to later stops until superseded or stopped by `NO_DATA` ([GTFS Realtime Trip Updates](https://gtfs.org/documentation/realtime/feed-entities/trip-updates/)). Feed freshness is part of feasibility: GTFS best practices say Trip Updates and vehicle positions should be no more than 90 seconds old and feeds should refresh at least every 30 seconds ([GTFS Realtime best practices](https://gtfs.org/documentation/realtime/realtime-best-practices/)).

Therefore every result should retain the Live Status snapshot timestamp and per-Leg data provenance. Stale or missing data should fall back explicitly to scheduled information with a larger safety buffer or a lower reliability classification; it must not silently masquerade as current information.

### Transfers

The base feasibility test should be:

`updated arrival + physical Transfer time + safety buffer <= updated departure`.

GTFS `transfers.txt` distinguishes timed, minimum-time, forbidden, and in-seat Transfers. Its `min_transfer_time` is intended to include the time a typical rider needs plus buffer for schedule variance ([GTFS Schedule reference, `transfers.txt`](https://gtfs.org/documentation/schedule/reference/#transferstxt)). Via's extra safety buffer should therefore be configurable by source quality, Travel Mode pair, station, and prediction uncertainty rather than blindly adding one global constant.

Do not begin with a probability axis in McRAPTOR. First enforce a minimum acceptable margin, then include residual Transfer risk in one generalized-cost value used for ranking. If Via later obtains calibrated stop/line/time-of-day delay distributions and wants contingency policies after a missed Transfer, prototype CSA MEAT against the same acceptance corpus before changing the core.

### YouBike

The authoritative GBFS model distinguishes functional vehicles physically available at a station from functional docks able to accept a return; `is_renting` and `is_returning` determine whether those counts are actually usable. It also states that station status reflects the operator's latest knowledge and should never be more than five minutes old ([GBFS `station_status.json`](https://github.com/MobilityData/gbfs/blob/master/gbfs.md#station_statusjson)).

For a YouBike Leg, admit a candidate only if the pinned snapshot shows:

- pickup station is installed and renting, with available bicycles at or above the pickup threshold;
- return station is installed and returning, with return slots at or above the return threshold; and
- the data is within Via's freshness policy.

The return-slot count is observed at query time, not a promise that a slot will remain available on arrival. A safety threshold can reduce, but cannot remove, this risk. Guidance should expose the observation timestamp and a plain-language caveat where the arrival horizon makes the snapshot weak evidence.

## Proposed planner boundary

```text
request + pinned Live Status snapshot
    ├── Walking/YouBike time-dependent graph search
    │     ├── direct Journey Options
    │     ├── access labels into transit
    │     └── egress labels from transit
    └── bounded McRangeRAPTOR over Bus/Metro
             └── candidate parent chains
                       ↓
        reconstruct Legs and Transfers
                       ↓
      enforce Reliable Arrival feasibility
                       ↓
      Pareto prune, rank, diversify, explain
```

Walking stays available as a connector regardless of rider-selected filters. Bus, Metro, and YouBike filters control whether their corresponding Legs may be generated. Apply Bus/Metro filtering before RAPTOR pattern scans and YouBike filtering before rental graph searches; filtering only after search wastes work and can incorrectly prune candidates against disallowed Travel Modes.

For the first implementation, keep the Pareto state small:

- arrival time;
- number of Transfers; and
- one generalized cost containing Walking burden, wait burden, and reliability penalty.

Apply hard ceilings for Walking duration, Journey duration, Transfers, and minimum reliability before or during search. After reconstruction, remove near-duplicates and cap the displayed Journey Options. This follows the practical lesson in OTP's RAPTOR documentation: exact multi-criteria search can return hundreds of paths, so production systems use a post-search filter chain ([OTP RAPTOR package documentation](https://raw.githubusercontent.com/opentripplanner/OpenTripPlanner/dev-2.x/raptor/src/main/java/org/opentripplanner/raptor/package.md)).

## Rust implementation recommendation

Implement a small, Via-owned engine around explicit interfaces rather than binding the domain model directly to one crate.

1. **Transit snapshot.** Dense integer IDs and structure-of-arrays storage for stops, transit patterns, trips, and stop times. Overlay an immutable Live Status version and let each query pin one version for consistency.
2. **RAPTOR kernel.** Start with earliest-arrival plus Transfer rounds and parent references; add departure-window reuse; then add only the bounded generalized-cost criterion. Use `vulture` as readable Rust prior art and a benchmark/reference oracle, after checking its exact API and test coverage against Via's requirements.
3. **Street/rental kernel.** Time-dependent Dijkstra/A* over Walking and bicycle edges with rental state (`not_renting`, `renting`). Gate pickup/return transitions from the pinned station snapshot.
4. **Orchestrator.** Generate only the allowed Journey grammar, combine access/egress labels with transit results, reconstruct typed Legs, and run Reliable Arrival checks.
5. **Verification corpus.** Include delayed and cancelled service, missing/stale Live Status, impossible and tight Transfers, zero/one/many bikes and return slots, every filter combination, midnight/service-day boundaries, and deterministic replay of the same pinned snapshot.

Keep algorithm records separate from Guidance text. Each accepted label should retain predecessor ID, source times, applied buffer, Live Status version, and rejection/dominance reason where practical. That makes Guidance generation stable and allows an operator to answer “why was this Journey Option accepted or rejected?” without replaying an opaque scalar score.

## Residual design decisions for grilling tickets

1. **Allowed Journey grammar.** May a Journey contain YouBike only as a direct, access, or egress Leg, or may it contain transit–YouBike–transit and more than one YouBike Leg? This decision changes the orchestrator and state space more than the choice among the three algorithm families.
2. **Reliable Arrival threshold policy.** What minimum Transfer margin, pickup count, return-slot count, and freshness limit apply by station, time horizon, accessibility need, and data quality? Which failures make a Journey Option infeasible versus merely less preferred?
3. **Uncertainty representation.** Is the product promising deterministic safety-buffer feasibility only, or a calibrated probability/grade? If probability is required, where will historical delay and YouBike inventory distributions come from, and how will calibration be tested?
4. **Pareto contract.** Which criteria are true nondominance axes, which are hard constraints, and which belong in generalized cost? How many materially distinct Journey Options should be returned?
5. **Live overtaking and snapshot rebuild.** If delays change the ordering of trips within one transit pattern, will Via split/re-sort the affected pattern, use a fallback scan, or rebuild the snapshot? Define the latency and consistency budget.
6. **Departure search window.** How wide should Range RAPTOR search, and does the UI ask “leave after,” “arrive by,” or both? The chosen window controls both alternatives and cost.
7. **Fallback behavior.** When GTFS Realtime or YouBike Live Status is absent/stale, should Via return schedule-only Journey Options, suppress the affected Travel Mode, or show them with a weaker reliability classification?
8. **CSA trigger.** What measurable failure of the recommended design—poor calibration, unacceptable missed-Transfer rate, or need for explicit contingency policies—would justify a CSA/MEAT prototype?

## Conclusion

Choose the **RAPTOR family as the transit algorithm, not as the whole multimodal planner**. Pair it with a time-dependent Walking/YouBike graph and a strict, versioned Reliable Arrival layer. This gives Via native Pareto alternatives, low-cost Live Status updates, direct Transfer reasoning, and explainable parent chains while keeping YouBike's inventory-constrained state in the model that represents it cleanly. Keep CSA/MEAT as the deliberate next experiment if Via later has the probabilistic data and product requirement to plan contingencies rather than buffered feasible Journey Options.
