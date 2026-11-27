# Routing research loop

Epic #21 goal G3: a continuous loop that finds, measures and PRs improvements to planning
accuracy and speed. Experiment records and the backlog (B1 onward, EXP-001 onward) live in
the research log on the `research/*` branches and `claude/via-routing-research-*`, not here.
Findings below are from 2026-11-23 to 2026-11-26.

## Rules of the loop
- One researcher at a time owns live TDX. Experiments use a separate key (`via-experiments`) kept in a git-ignored `server/.env.experiment`, so they do not starve the deployed gateway or the owner's phone. Never print or commit credentials.
- TDX limit: the key was upgraded on 2026-11-26 to 5 requests per second, shared by every process (gateway, `via-route`, scripts, the phone). Start live plans at least 3 s apart, at most 2 at once, back off on any 429. The earlier 5 requests per minute rule no longer applies.
- Cadence (Asia/Taipei): 08:00 to 22:00 live experiments and map comparisons; 22:00 to 08:00 literature, offline prototypes and `corridor_bench`.
- Every claim needs before and after numbers: correctness, arrival-time accuracy, latency p50 and p95. Latency needs a quiet machine (load swung from 3 to 250 and made runs differ more than the change).
- Retained change: branch off `main` with fmt, clippy `-D warnings` and tests passing, PR with numbers, never merged by the agent. Ideas that broke an `AGENTS.md` constraint were prototyped only, then PR'd after the owner approved (headway-estimated Bus departures were approved 2026-11-23; AGENTS.md was updated in that PR, #42).
- Ground truth: TDX live data (Bus N1 estimates, Metro live boards, TRA delays) is the truth for arrival times. Use Google Maps (built-in browser) and Apple Maps (computer use) only for route choice and coverage. Google mislabels coordinate-only destinations; use place names.
- GTFS beta gaps (for example Wenhu and Circular lines with 0 trips) are upstream; do not work around them.
- Data refresh: download GTFS only (`--gtfs-only`, #39); Valhalla needs a port-forward to the OrbStack service on 8002 and `VIA_VALHALLA_URL`.

## Findings
- Common lines: sharing lines across stops of one place widens boardable lines from 1.25 to 2.73 per edge offline (shipped as #40; Phase 1 p50 114.8 to 99.6 ms and p95 257.6 to 216.9 ms, with run-to-run spread larger than the difference).
- EXP-008 live A/B (19 valid of 20 scenarios): labelled headway estimates arrive 23 to 36 min earlier in 3 scenarios; place sharing 3.5 to 10 min earlier in 2; both combined earlier in 4; nothing got worse; TDX requests per plan unchanged by place sharing. Estimates shipped as #42.
- EXP-009 (place-aware fan-out of walking transfers) not retained: 69.5% of stops hit the 12-edge cap, 74% of kept edges go to a place already reached, and on the Taipei core the cap removes 84% of reachable walking pairs. One edge per place hides other lines (17 of 97 scenarios worse). Whole places nearest-first gave 15 to 19 better and 7 to 9 worse by over 30 s on static cost, a metric the search does not optimise, so a live A/B is needed.
- EXP-010 (TDX N1 bus ETA error, 81 evening buses, one hour only): MAE 33 s at 0 to 5 min, 76 s at 5 to 10, 93 s at 10 to 20; bias +12 to +27 s up to 10 min and -135 s beyond 20 min (n=52). Rerun in daytime.
- Literature (2024 to 2026): D-ULTRA (Bez and Sauer 2024), delay-aware ULTRA (Katkalo et al. 2026, 1.9 to 4.2x faster), Early Pruning RAPTOR (Rohovyi et al. 2026, up to 57%), hub-label RAPTOR, T-REX. None fit the static Phase 1 better than EXP-009 or B22.
- One Google Maps data point: Taipei Main to Xinyi, Google 23 min versus Via 26.5 min.
- Fetch fragility (B19): any single timeout discarded a whole snapshot and local DNS (about 1.4 s) exceeded the 1 s connect timeout; two 40-minute fetches were lost. Addressed by paced, resumable fetch (#39).
- A production artifact without `tdx_route_uid` bindings left parent-route mapping (#16) inactive until a real `via-data fetch` and `build`.

## Direction (unchanged)
Seek genuinely different methods, not tuning: CSA or profile search, ULTRA bike shortcuts (B11), McRAPTOR Pareto bags (B2), stop-place equivalence (B12), YouBike availability forecasting, Phase 2 and reconstruction latency (B9), arrival accuracy against live TDX, re-plan latency for navigation. Next steps recorded at the pause: live A/B of place plus estimates with and without rule B nearest-first on 20 scenarios; a daytime ETA error rerun; more Google and Apple comparisons.
