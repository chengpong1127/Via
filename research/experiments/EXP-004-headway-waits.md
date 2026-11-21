# EXP-004 — Static headway waits in Phase 1

Branch: `research/exp-004-headway-waits` (on top of EXP-002 research head `0b7778d`)
Status: parked — live quality neutral (40 paired plans), latency lower; no PR until B15 / EXP-002 dedup
Owner decision: 2026-11-21, TDX Bus/Schedule may be used as a static headway source (B1 unblocked)

## Hypothesis

Phase 1 cost is ride + walk + transfer penalty only. Lines that do not run at the
query time, and very infrequent lines, look as good as frequent ones and take
corridor slots that Phase 2 then cannot time. Adding an expected boarding wait
(half the static headway for the query's service period) should keep practical
corridors without hurting the best option.

## Data

- TDX `v2/Bus/Schedule/City/{Taipei,NewTaipei,Keelung,Taoyuan}` and `InterCity`,
  fetched 2026-11-21 10:10–10:15 +08:00, one call per 61 s. Taipei 599 sub-routes
  (11.7 MB), New Taipei 969 (12.3 MB), InterCity 1,732 (61 MB), Keelung 165,
  Taoyuan 664.
- Join is direct: GTFS `route_id` = TDX `SubRouteUID`, pattern direction = TDX
  `Direction`. All 599 TPE, 963 NWT and 563 TAO GTFS bus routes match.
- GTFS buses without any trips: TPE, NWT, TAO, TXG, TNN, KIN and part of HSQ/PIF
  (3,560 patterns). Metro, TRA, THSR, THB intercity and KEE have GTFS trips.
- TDX timetables and frequency bands complement each other within a day
  (frequencies for daytime, explicit trips for evenings), so both are counted.

## Implementation

- `domain::transit::service`: `ServicePeriod` (Night 00–06, weekday 06–09,
  09–16, 16–19, 19–24, weekend 06–24, in Taipei time) and `PatternHeadways`.
- `TransitNetwork.pattern_headways` (serde default, validated, canonicalised).
- `data::bus_headways`: departures per period on the next Wednesday and Saturday
  after the fetch, aggregated per Bus/Metro route + direction (Taipei Metro splits
  each line into short-turn variants with one late trip each, so per-pattern
  counts were wrong). GTFS trips take precedence; TDX fills routes without
  trips. Rail/HSR are left without headways (one island-wide route per direction).
- `DensePattern.headways`; Phase 1 adds `min(headway/2, 900 s)` to each
  boarding, or 1,800 s when the line has no service in the period. The wait is a
  penalty: it ranks corridors but does not count toward the duration limit.
- `via-data fetch` downloads Bus/Schedule for the 8 cities without GTFS bus
  trips and spaces every TDX call (GTFS, bike pages, schedules) by 61 s;
  `via-data build` derives headways and reports unmapped sub-routes.

## Offline results (corridor_bench, 100 scenarios, 2026-11-20 GTFS)

| | base AM | headway AM | base 12:00 | headway 12:00 |
| --- | --- | --- | --- | --- |
| retained corridors | 2,241 | 2,194 | 2,241 | 2,183 |
| on a line with no service in the period | 496 | 41 | 767 | 114 |
| with a commuter / 上午 / 下午 line | 142 | 49 | 142 | 17 |
| multi-ride with a ride ≤ 60 s | 496 | 454 | 496 | 437 |
| mean best static time (no waits) | 1,990 s | 2,114 s | 1,990 s | 2,123 s |
| mean best static + expected wait | 3,093 s | 2,988 s | 3,235 s | 3,132 s |
| p50 / p95 / p99 latency | 91 / 203 / 247 ms | 63 / 219 / 302 ms | 91 / 200 / 245 ms | 69 / 239 / 300 ms |

Per scenario, best static + expected wait at AM: 35 better, 2 worse (max +135 s),
60 within ±30 s. Mean states expanded fall 28% (171.6k → 123.4k); a few long
scenarios expand up to 15% more because the 50% target slack now applies to a
larger cost.

The "no service" lines are genuinely not running at Monday 08:30: afternoon
commuter lines (內科22(下午), 南軟雙和(下午)), weekend-only 111 to Yangmingshan,
and single-trip variants (657經海山拖吊場, 988新北板橋公車站).

Side effect: with commuter lines gone, slots refill with stop-level duplicates
of the best corridor (ntu_to_nangang: 4× "254 → BL" differing only in stop IDs),
which strengthens the case for EXP-002 / anchor-as-place.

## Known limitations

- Wait is charged per pattern; equivalent lines over the same stop pair are not
  combined, so trunk segments served by many lines are under-rewarded.
- Rail and HSR have no wait, so their relative cost falls versus Bus/Metro.
- Night uses weekday service only; Friday/Saturday late-night service is not
  represented.
- Route-level Metro aggregation merges branches (R includes the Xinbeitou shuttle).
- Phase 1 is now time-dependent through the service period.

## Wait cap sweep (offline, same 100 scenarios)

`noservice`-style cap 0 charges only the 1,800 s no-service wait; running lines pay nothing.
Best + expected wait is scored with the full model in all rows; "vs base" counts
scenarios more than 30 s better / worse.

| period | variant | no-service corridors | commuter corridors | mean best + wait | vs base |
| --- | --- | --- | --- | --- | --- |
| Mon 08:30 | base | 496 | 142 | 3,093 s | — |
| | cap 0 | 9 | 140 | 3,065 s | +9 / −1 |
| | cap 300 | 21 | 120 | 3,043 s | +16 / −2 |
| | cap 900 | 41 | 49 | 2,988 s | +35 / −2 |
| Wed 12:00 | base | 767 | 142 | 3,235 s | — |
| | cap 0 | 13 | 7 | 3,215 s | +4 / −1 |
| | cap 900 | 114 | 17 | 3,132 s | +27 / −2 |
| Sat 10:30 | base | 1,024 | 142 | 3,384 s | — |
| | cap 0 | 25 | 12 | 3,329 s | +12 / −1 |
| | cap 300 | 78 | 18 | 3,297 s | +18 / −2 |
| | cap 900 | 184 | 27 | 3,253 s | +29 / −3 |

Latency p50 / p99 at Mon 08:30: cap 0 74 / 261 ms, cap 300 67 / 261 ms (cap 900 63 / 302 ms, base 91 / 247 ms).
All variants keep bus 669 (Taipei Main → Xinyi) and 306 (NTU → Nangang) in the
offline corridor sets, so the live losses below come from live access times, not
from the offline selection.

## Live comparison

### Run 1: base vs cap 900, Saturday 2026-11-21 10:25–11:53 +08:00

`examples/live_compare.rs`, 20 scenarios (10 named, 10 random), one process, shared
realtime cache (TTL 300 s), 3–7 TDX requests per scenario, 60 s pause per request.

| | count |
| --- | --- |
| headway arrives > 30 s earlier | 3 (xinzhuang_to_muzha −77 min, songshan_airport_to_wanhua −5, sanchong_to_ximen −4) |
| headway arrives > 30 s later | 3 (banqiao_to_neihu +13, ntu_to_nangang +5, taipei_main_to_xinyi +3) |
| same | 14 |
| mean arrival delta | −189 s (driven by xinzhuang_to_muzha) |

Losses:
- taipei_main_to_xinyi: base 669 bus (21 min, a bus happened to be near) vs Metro R
  (24 min). 669's weekend headway is ~90 min, so its capped 900 s wait pushes it past
  the target slack with live access times.
- ntu_to_nangang: base G + 306 (50 min) vs 37-min walk + BL (55 min).
- banqiao_to_neihu: both walk > 1 h because the Wenhu (BR) line is missing from the
  GTFS beta; not an algorithm difference.

Wins come from corridors with a transfer the base search dropped (xinzhuang_to_muzha
O → BL instead of a 3-hour walk, again partly a BR-gap artefact).

Latency (base 775 ms median vs headway 284 ms) is biased: base always ran first and
paid the TDX fetches into the shared cache. Run 2 rotates the order.

### Run 2: base vs no-service only vs cap 900, Saturday 2026-11-21 12:02–13:47 +08:00

Same scenarios, variant order rotated per scenario so each variant pays the TDX
fetches into the shared cache equally often. The no-service variant sets every known
headway to 1 s (running lines cost 0, lines without service 1,800 s).

| vs base | earlier | later | same |
| --- | --- | --- | --- |
| no-service only | 1 (xinzhuang_to_muzha −73 min) | 1 (banqiao_to_neihu +17 min) | 18 |
| cap 900 | 2 (xinzhuang_to_muzha −73, songshan_airport_to_wanhua −8) | 1 (banqiao_to_neihu +17) | 17 |

Median / max planning latency: base 523 / 1,001 ms, no-service 409 / 827 ms,
cap 900 300 / 843 ms.

Both outliers are dominated by the missing Wenhu (BR) line: the base Banqiao → Neihu
option (265區 + 310 then a 70-min walk) and the new Xinzhuang → Muzha option
(O → BL then an 80-min walk) both end in long walks no real journey would take.
Run 1's lucky-bus losses (669, 306) did not repeat at 12:00.

## Decision

Across both runs (40 paired plans) cap 900 is 5 earlier / 4 later / 31 same; the
non-BR-gap changes are small in both directions (−8 to +5 min). Offline gains are
measured with the same expected-wait model the variant optimises, so they are not
independent evidence of better journeys. Planning is faster (fewer corridors on
lines that are not running).

Parked, not discarded: keep the branch and the headway data path. Revisit after
B15 (combined frequency over equivalent lines) and EXP-002 place dedup, which
target the two observed failure modes (trunk segments under-rewarded, freed slots
refilled with stop-level duplicates). If only one variant ships, no-service only is
the lower-risk choice: it never penalises a running line.

## Side finding

7 of 10 random live scenarios return walking only (60–164 min walks) under every
variant, e.g. random_001 (25.136, 121.434) → (25.078, 121.472), a 107-min walk where
Tamsui-line and Luzhou-area transit exists. Tracked as EXP-005.
