# EXP-011: live A/B of the corrected place build (PR #40) and labelled estimates (PR #42), plus Via vs Google

Branch: `research/exp-011-corrected-place-est` (`perf/headway-estimates` merged with `perf/place-common-lines` 0d08ec1, plus
toggles `PLACE_EQUIVALENCE` / `HEADWAY_ESTIMATES` and the `live_compare`, replay-pin changes). Prototype only.
Status: done 2026-11-25 09:07-10:30 Asia/Taipei. Date: 2026-11-25.

## Question

Does PR #40 (place-level edges with the timed place-mate hop, Bus-only complete-linkage places) show a measured gain in a live A/B,
alone and on top of est (#42)? How do the plans compare with Google Maps on the same pairs?

## Setup

- Artifact: built 2026-11-25 with the #42 build code from GTFS 2026-11-20, Bus routes 2026-11-23 (2,958 parent-route bindings)
  and Bus/Schedule 2026-11-21 (2,123 bindings with headways). Valhalla port-forward on 8002.
- Variants in one process, one realtime cache (TTL 90 s), order rotated per scenario: `base` (no place, no est), `place`, `est`,
  `place+est`. 20 named scenarios (`scenarios-named.json`), 1 TDX plan per 60 s per request used (3-5 requests per scenario).
- Production realtime freshness (fresh 120 s, degraded 360 s). The EXP-008 evening run used 600 s/900 s.
- Command (from `server/`): `VIA_TDX_BASE_URL=http://127.0.0.1:18766/basic/ VIA_CONFIG=<via.toml with that proxy and 5 s connect, 30 s
  request> VIA_VALHALLA_URL=http://127.0.0.1:8002/ live_compare data/build/20261125T004225Z/.via-static.json live-scenarios-named.json out.jsonl base,place,est,place+est`
  with the recording proxy `research/tools/tdx_record.py` in front of TDX (all 84 TDX responses are saved in
  `results/exp011-tdx-record.jsonl`, so the run can be replayed without TDX; see the replay section).
- Raw rows: `results/exp011-live-20261125.jsonl`. TDX requests per scenario (all four variants together): 3 to 6 (median 4).
  The variant that ran first paid for them; the place variant's own request counts were 0-3, i.e. one per scope as predicted.

## Result (live, 20 scenarios, 20 valid)

| variant | earlier by >30 s | later by >30 s | walk-only (base 2) | mean arrival change |
| --- | --- | --- | --- | --- |
| place | 1 (ntu_to_nangang -1,273 s) | 0 | 2 | -64 s |
| est | 6 (ntu -1,680, songshan_airport -354, linkou -306, yongning -3,064, xizhi -1,748, songshan_station -775 s) | 0 | 2 | -396 s |
| place+est | 6 (same scenarios; linkou -793 s, xizhi -4,081 s) | 0 | 2 | -537 s |

- place alone: one win. Base walked 3,197 s then took Metro BL; place found Bus 907 + Bus 9026 (a place-mate transfer with a
  138 s timed hop). This win depends on bus ride durations (see EXP-012: they are about 2.9 times too short).
- place on top of est: linkou_chang_gung_to_xinzhuang -487 s and xizhi_to_zoo -2,333 s better than est alone.
- Nothing got worse in any variant. TDX requests per plan did not rise with place.
- Walk-only in every variant: xinzhuang_to_muzha (11,204 s) and xiangshan_to_muzha (4,089 s). banqiao_to_neihu and
  yongning_to_xihu (base) end with a 4,000-5,000 s walk after a Metro BL ride. All four need the Wenhu Line (Metro BR). The artifact has the BR route and both patterns (24 stops) but 0 trips and 0 frequencies:
  the GTFS beta feed has no BR stop times (upstream gap, not worked
  around, see memory `gtfs-beta-gaps-upstream`). That is 4 of 20 scenarios.

## Replay without TDX (new harness, item 5)

`research/tools/tdx_record.py` (recording proxy) and `tdx_replay.py` (replay server). Replaying the same 20 scenarios from the
recorded responses, with departure pinned to the live planning time and fresh/degraded set to 1e8 s:
73 of 80 plans are identical to live (same legs, arrival within 5 s). The 7 that differ are shulin_to_nangang (all 4 variants, +120 s on
the same TRA ride) and xizhi_to_zoo (base, place +600 s on the same TRA ride; place+est picks another bus plan): TRA live-board
timing, cause not isolated. The replay server answers Bus, Metro and TRA live-board requests from the union of the recorded window
filtered by the requested stops/lines/trains (zero unanswered requests), so a changed fan-out still finds data. Bus requests are served from the union of the recorded window filtered by StopUID, so changed
fan-out still finds predictions. Replay of one 20-scenario x 4-variant run takes about 3 minutes and zero TDX requests.
Limits: a code change that needs stops or routes absent from the recorded window gets "no prediction"; departure pinned to
seconds; the replay corpus covers one morning.

## Via vs Google Maps (same pairs, same minutes)

Door-to-door minutes from now (Via arrival - plan time; Google arrive time - query time, top-listed option). Apple Maps: no
access (denied twice), column empty. Gap = Via place+est - Google.

| scenario | Via base | Via place+est | Google | gap (min) | Apple |
| --- | --- | --- | --- | --- | --- |
| taipei_main_to_xinyi | 21 | 21 | 16 | +5 | n/a |
| ntu_to_nangang | 68 | 40 | 54 | -14 | n/a |
| banqiao_to_neihu | 116 | 116 | 55 | +61 | n/a |
| tamsui_to_xindian | 70 | 70 | 84 | -14 | n/a |
| shilin_to_zhonghe | 50 | 50 | 50 | 0 | n/a |
| songshan_airport_to_wanhua | 44 | 38 | 40 | -2 | n/a |
| xinzhuang_to_muzha | 187 | 187 | 69 | +118 | n/a |
| sanchong_to_ximen | 9 | 9 | 25 | -16 | n/a |
| taoyuan_to_taipei | 54 | 54 | 42 | +12 | n/a |
| keelung_to_taipei | 50 | 50 | 58 | -8 | n/a |
| linkou_chang_gung_to_xinzhuang | 46 | 33 | 55 | -22 | n/a |
| shulin_to_nangang | 55 | 55 | 55 | 0 | n/a |
| yongning_to_xihu | 110 | 59 | 74 | -15 | n/a |
| xizhi_to_zoo | 127 | 59 | 69 | -10 | n/a |
| songshan_station_to_tamkang | 113 | 101 | 75 | +26 | n/a |
| guandu_temple_to_jiantan | 37 | 37 | 38 | -1 | n/a |
| keelung_chang_gung_to_taipei | 53 | 53 | 61 | -8 | n/a |
| xiangshan_to_muzha | 68 | 68 | 7 (suspect) | +61 | n/a |
| beitou_to_songshan_airport | 40 | 40 | 48 | -8 | n/a |
| banqiao_to_linkou | 50 | 50 | 33 | +17 | n/a |

Median gap -1.5 min, mean +9.1 min, 9 of 20 within 10 minutes.
Reading the gaps:
- Positive gaps over 50 min (4 scenarios) are the Wenhu Line missing from the feed (xinzhuang, banqiao_neihu, xiangshan; yongning
  with est is -15 only because est found a bus that is too fast, see below). Upstream, not worked around.
- banqiao_to_linkou +17: Google rides one direct bus 948 from the Banqiao bus station; Via's pattern for route 948 starts at
  Linkou, so the direct ride is not in the artifact (coverage gap, not investigated further).
- Negative gaps (Via faster than Google by 10-22 min) occur where Via uses buses: ntu, linkou, sanchong, yongning, xizhi.
  Via's bus ride times come from GTFS run times and are far too short: EXP-012. Via claims 9 minutes for Sanchong to Ximen
  (Bus 640, 328 s); Google says 25.

## Decision

- est (#42): the 6 wins are real search wins (walks of 1-3 h replaced by transit) but each depends on bus ride durations that
  EXP-012 shows are optimistic. Under a x2.5 ride-time scale (replay on a scaled artifact) the est wins shrink from 6 to 3
  (ntu -14 min, linkou small, xizhi -16 min) and yongning, songshan_airport and songshan_station disappear.
- place (#40): no robust measured win. The one live win (ntu, -21 min) vanishes under the x2.5 scale; the two place+est extras
  (linkou, xizhi) shrink to one (xizhi -11 min). Recommendation: keep #40 in draft; re-run the same replay after the bus
  ride-time fix lands (zero TDX). Structural benefit only until then.
- Next: EXP-012 (bus ride-time model) is now the highest-value item; est's value must be re-measured with it.
