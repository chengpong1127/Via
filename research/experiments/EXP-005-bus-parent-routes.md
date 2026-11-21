# EXP-005: New Taipei bus realtime dropped by parent-route guessing

Branch: `fix/bus-parent-route-realtime` (prototype on top of EXP-004 `89d6762`)
Status: keep — 2 earlier / 0 later / 18 same live; PR off main pending owner approval
Type: correctness / data bug found during EXP-004 live runs

## Symptom

7 of 10 random live scenarios return walking only (60–164 min walks) under every
variant, although Phase 1 finds transit corridors for all of them offline
(random_001: best static corridor 1,863 s vs a 6,419 s walk).

## Diagnosis (Saturday 2026-11-21, 13:54–14:00 +08:00)

- Traced `via-route --trace` for random_001 (25.136, 121.434) → (25.078, 121.472):
  24 retained corridors, all New Taipei buses (704, F123, 963, 927). The bus
  timing snapshot asked for 18 stops in 2 scopes and got **1** usable row; all 18
  boarding positions ended empty (`snapshot_summary=(0, 18, 0, [])`).
- One paced TDX probe for `StopUID eq 'NWT131498'` (the 704 boarding stop)
  returned 704 arriving in 519 s, as `RouteUID: NWT16517` with **no
  SubRouteUID**.
- GTFS route_id for 704 is the sub-route `NWT157552`. `parse_bus_arrivals` maps
  route-level rows to sub-routes by dropping the last digit (TPE153340 →
  TPE15334), so it looks for `NWT15755` and drops the row.
- The guess against TDX Bus/Schedule `SubRouteUID → RouteUID` pairs:

| city | sub-routes | guess correct | sub = route | guess wrong |
| --- | --- | --- | --- | --- |
| Taipei | 599 | 156 | 106 | 337 |
| New Taipei | 969 | 12 | 12 | 945 |
| Taoyuan | 451 | 0 | 451 | 0 |
| Keelung | 165 | 0 | 0 | 165 |
| InterCity | 1,653 | 0 | 0 | 1,653 |

  New Taipei estimates have no SubRouteUID, and New Taipei buses have no GTFS
  trips, so almost every New Taipei bus leg had no timing at all and Phase 2 could
  only offer walking. Taipei estimates carry SubRouteUID, so Taipei worked. Keelung
  and InterCity have GTFS trips as a fallback.

## Fix

- `BusPatternBinding.tdx_route_uid` (serde default, omitted when empty).
- `bus_headways::attach_parent_routes` fills it from the Bus/Schedule snapshot's
  RouteUID when it differs from the SubRouteUID: 1,735 bindings on the 2026-11-20
  artifact.
- `parse_bus_arrivals` prefers the bound parent route and keeps the last-digit
  guess only for bindings without one.
- `via-data build` prints the count; AGENTS.md and CONTEXT.md record the second
  use of the Bus/Schedule snapshot.

Re-trace of random_001 (14:00): 18 of 18 rows used (was 1), 3 boarding positions
with realtime predictions (was 0). The journey is still walking only. TDX
estimates give only the next bus per stop, and if it leaves before the walk to
the stop ends, there is no later departure: Taipei/New Taipei buses have no GTFS
trips or frequencies to fall back on. That is the next limit (see backlog).

## Live comparison (Saturday 2026-11-21 14:06–15:10 +08:00)

Same 20 scenarios, `live_compare <artifact> live-scenarios.json out.jsonl
base,base+parents`, both variants without headways, order rotated, one shared
realtime cache, 60 s per TDX request (1–5 requests per scenario).

| parents vs base | count |
| --- | --- |
| arrives > 30 s earlier | 2 |
| arrives > 30 s later | 0 |
| same | 18 |
| walking only (base / parents) | 4 / 4 |
| median / max planning latency | 494 / 1,030 ms vs 442 / 1,013 ms |

Wins:
- songshan_airport_to_wanhua −13.5 min: base walks 37 min to Zhongxiao Fuxing for
  BL; with parents, Bus 630 (大都會) then 673 from a stop 5 min away. Both are
  Taipei routes whose estimates arrived without a usable sub-route under the guess.
- random_003 −39 min: base walks 56 min to Xinpu for BL; with parents, New Taipei
  Bus 920 from a stop 3 min away.

The four walking-only scenarios (random_000, 001, 008, 009) stay walking only in
both variants: the predicted next bus at each candidate stop leaves before the
access walk ends (B18). Earlier runs had 7 walking only; the count varies with
which buses happen to be near at query time.

## Decision

Keep. It is a correctness fix (realtime rows that TDX returned were discarded),
no scenario got worse, and latency is unchanged. Ship as its own PR off `main`
together with the Bus/Schedule fetch it depends on, without the EXP-004 headway
waits. Waiting for owner approval to open it.

## Known limitations

- A route-level estimate is applied to every sub-route of that parent at the stop;
  a short-turn variant's bus can be offered for a longer sub-route.
- Needs the Bus/Schedule snapshot (currently fetched for 8 cities); other cities
  keep the guess.
