# EXP-001 — Dense-index corridor search

- Branch: `research/exp-001-dense-corridor-search`
- Status: retained (PR candidate)
- Date: 2026-11-21

## References
- Delling, Pajor, Werneck. *Round-Based Public Transit Routing* (RAPTOR), ALENEX 2012 — RAPTOR's speed relies on
  contiguous arrays indexed by stop/route ids, not hash maps.
- Standard engineering practice in OpenTripPlanner 2 (`RaptorTransitData` uses int stop indices) and R5.

## Problem
Phase 1 (`routing/corridors/search.rs`) took p50 ≈ 0.5–1.0 s per query on the real Taiwan network
(106 823 stops, 6 766 patterns, 1.07 M transfer edges). Profiling (`sample`, 25 s) showed the time in
`Search::insert`, `relax_transfers`, malloc/free, SipHash and `memcmp`: every label insert did
`entries_by_stop.entry(stop_id.clone())` (String allocation + SipHash), labels cloned `LineKey` Strings,
nodes cloned 3 String ids per expansion (~150 k expansions/query), and structure fingerprints hashed
String ids with SipHash.

## Hypothesis
Replacing String ids with dense `u32` indices assigned **in sorted-id order** keeps every iteration order and
tie-break identical (the original code sorts stops and patterns by id string), so the retained corridors are
byte-identical while removing allocation and hashing from the hot loop.

## Implementation
- `domain/transit/topology.rs`: `DenseTopology`, built once in `TransitRuntimeIndexes`: sorted stop / pattern ids,
  per-pattern `u32` stop arrays and segment times, pattern mode and line key (route for Bus/Metro, pattern for
  Rail/HSR — same semantics as `LineKey`), positions by stop, transfers by stop (same dedup and order).
- `routing/corridors/search.rs`: labels, bag entries and nodes are `Copy` and index based; `RoundLabels` is a
  `Vec<Vec<Label>>` with a touched list; nodes store a compact step and materialise `StaticCorridorStep`
  only for candidates; fingerprints use a splitmix64 mix of indices instead of SipHash over Strings.
- `select`, `equivalent_legs_for` and corridor construction unchanged.

## Correctness
- `cargo test`: all suites pass.
- Output equivalence: `corridor_bench` on 100 scenarios, both the 2026-11-02 and the 2026-11-20 artifacts —
  retained corridors, `states_expanded` and `unique_corridors_found` identical for every scenario.

## Results
See `benchmark_results.md`.

## Trade-offs
- Extra memory for the dense index (measured below) — built once at startup.
- Per query: two `Vec<Vec<Label>>` and one `Vec<Vec<BagEntry>>` of `stop_count` headers (~2.5 MB each).

## Future directions
- B4/B6 remaining allocations in `select` (`format!("{:?}")` sort keys).
- Reuse per-query buffers across requests (thread-local pool) if allocation shows up again.
