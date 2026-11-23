# Design notes: genuinely different approaches (2026-11-24 night)

Each design carries a backlog ID, the constraint check against AGENTS.md, and an experiment
plan with a fixed baseline and metrics. All benchmarks need a quiet machine. Live runs keep
the 1 TDX plan/min pacing.

---

## B21: Departure-window profile search in Phase 2 (CSA / rRAPTOR idea)

**Source.** Dibbelt, Pajor, Strasser & Wagner, *Connection Scan Algorithm*, ACM JEA 23 (2018),
[arXiv 1703.05997](https://arxiv.org/abs/1703.05997). Profile CSA scans connections in
decreasing departure time and keeps Pareto profiles (departure, arrival), which answers "all
good journeys in a time range". rRAPTOR (Delling, Pajor & Werneck 2012) does the same with rounds.

**Why it fits Via, and where it does not.** Phase 1 is static: Taipei and New Taipei buses have
no GTFS trips, so a timetable-driven CSA cannot replace corridor discovery. Replacing it would
also break "MultimodalPlanner owns static round-based corridor discovery" and "no generic
routing-algorithm boundary". The useful part is profile *evaluation* in Phase 2. Today each
fixed corridor is evaluated for one departure time ("now"), so Phase 2 boards the first
feasible trip at each anchor. A profile pass over the same corridor, latest departure first,
would give:
- the latest departure from the origin that still reaches the same arrival, which is less
  waiting at stops and directly useful for "leave in N min" UI;
- dominance across departures: a later departure with the same arrival dominates.

**Constraint check.** It stays inside fixed-corridor evaluation: no new anchors, no TDX calls,
no corridor combining. It is allowed in principle. The app "always plans for departing now",
so the output is still departing-now journeys, with a better departure inside the window.

**Experiment (offline first, then live).**
1. In `CorridorEvaluator`, after a forward pass gives arrival A for a corridor, do a backward
   pass from A: at each transit anchor, take the latest trip that still connects. Report the
   latest origin departure D*.
2. Metric: wait time removed (D* − now) and total waiting at stops, over the 100 scenarios.
   Run it on recorded timing snapshots: Metro and TRA GTFS times need no TDX.
3. Success means the median wait removed is at least 2 min on Metro/TRA corridors, with no change
   in arrival. Cost: Phase 2 latency p50/p95.

---

## B22: ULTRA-style BikeShare shortcuts for Phase 1 (refines B11)

**Source.** Baum, Buchhold, Sauer, Wagner & Zündorf, *ULTRA: Unlimited Transfers for Efficient
Multimodal Journey Planning*, Transportation Science (2023), [arXiv 1906.04832](https://arxiv.org/abs/1906.04832);
code [kit-algo/ULTRA](https://www.github.com/kit-algo/ULTRA). Preprocessing finds the few
stop-to-stop transfers by an unrestricted mode (walk, bike) that are needed by some optimal
journey. Queries then use only those shortcuts.

**Via today.** Phase 1 transfers are walking edges capped at 12 per stop and 500 m. BikeShare
appears only in Phase 2, along anchors that already exist. So Phase 1 never proposes a corridor
whose middle hop is a 2 km YouBike ride, for example riverside stations or a gap between two
Metro lines.

**Design.** Offline in `via-data build`, for each pair of YouBike stations within a cycling
radius (≤ 3 km), compute the bike time with Valhalla Matrix. Then run an ULTRA-like witness
search: keep a station-to-station bike hop as a stop-to-stop shortcut (walk to dock, ride, dock,
walk) only if it beats transit plus walking for some departure. It is static, and availability is
still checked at request time in Phase 2 (AGENTS.md: "availability remains request-time data").

**Constraint check.**
- Allowed: derived static costs, like segment costs, are not timetable data.
- Phase 2 needs BikeShare to be evaluable between two transit anchors. That is already
  supported ("continuous BikeShare rental across later anchors").

**Experiment.**
1. Prototype the shortcut generation for Taipei and New Taipei only. Report the shortcut count
   and the artifact size delta.
2. Run `corridor_bench` with shortcuts as transfer edges tagged BikeShare. Count how many
   scenarios gain a corridor with a bike hop that is ≥ 5 min faster statically.
3. Live A/B with `bike_share` selected, 20 scenarios. Measure arrival delta and feasibility.
   The bike hop must survive the availability check.

---

## B23: Measure (and later correct) TDX bus ETA error against later TDX observations

**Ground truth (owner, 2026-11-23).** TDX live data is the truth source for ETAs. A
*prediction* made at time t can still be wrong about the bus's actual arrival. TDX itself
reveals the actual arrival later: the N1 row at the stop switches to "arriving" or "at stop" and
then to the next bus, and A1/A2 vehicle events give the actual stop arrival.

**Literature.**
- Kalman filters beat ANNs when data is scarce, and ANNs win with large histories
  ([TRB 1288350](https://trid.trb.org/view/1288350)).
- Bus delay residuals are heteroskedastic and heavy-tailed, so report quantiles, not only RMSE
  ([arXiv 2106.13576](https://arxiv.org/pdf/2106.13576)).
- AR log-normal travel-time models work as sequential real-time predictors
  ([arXiv 1904.03444](https://arxiv.org/pdf/1904.03444)).

**Step 1: measurement only, no product change.**
- A research logger polls N1 for a fixed set of about 30 stops on the EXP-008 corridors. That
  is one request per scope per minute, inside the quota and off the live A/B hours.
- Record (stop, route, predicted arrival, observed at) and the first "at stop" or "departed"
  time per bus.
- Output: ETA error by prediction horizon (0–5, 5–10, 10–20, 20+ min), with p50, p90 and
  bias per city.
- This quantifies how much Via's arrival times can be trusted. It also tells B18 how far
  estimates drift from the real next-but-one bus: compare estimated departures with the
  actual second and third buses.

**Step 2: only if the bias is large and stable.** Apply a per-horizon bias correction to the
predicted departure in `BusDepartureSource`.
- Needs owner sign-off: it changes TDX-sourced times.
- Storage stays out of the product. Fit the correction offline from the log and ship it as
  constants or a small table in the artifact. No database (AGENTS.md).

**Success metric.** Mean absolute error of Via's planned boarding time vs the observed bus
arrival, before and after correction, on held-out days.

---

## B24: YouBike availability forecasting at the planned pickup and drop-off times

**Literature.**
- Probabilistic short-horizon forecasts beat persistence, with 15 min the best horizon in the
  Bay Area study. Full distributions beat queue models (Feng, Hillston & Reijsbergen 2016).
- Poisson-mixture rate models are cheap and effective.
- For Taipei, an LSTM forecasts next-hour YouBike rentals for rebalancing
  ([MDPI Proc. 110/1/35](https://mdpi.com/2504-3900/110/1/35)).

**Via today.** Pickup and drop-off are judged on availability read at request time. A drop-off
reached 25 min later is judged on its current free docks.

**Design.** Keep request-time availability as the input (AGENTS.md). Add a static per-station,
per-15-min-of-week net flow rate (bikes/min) fitted offline from a research log of availability
snapshots. At evaluation, predicted docks at arrival = current docks − flow × Δt, and the
probability of a free dock comes from a Poisson model. Use it to rank or penalise risky drop-offs,
not to fabricate availability.

**Constraint check.** The derived static rate tables are like segment costs. Availability itself
stays request-time. Needs owner OK before it ships, because it changes a BikeShare feasibility
decision.

**Experiment.**
1. A research logger polls Taipei YouBike availability every 5 min for 7 days. One request per
   city per poll is about 0.2 req/min, which coexists with live tests.
2. Backtest: predict docks at +15 and +30 min. Compare persistence (Via today) with the rate
   model on MAE and on the "empty/full at arrival" classification (precision and recall).
3. Ship only if it beats persistence by ≥ 20 % on the empty/full F1.

---

## B25: True Pareto bags in Phase 1 corridor search (McRAPTOR / MCR; refines B2)

**Source.**
- McRAPTOR: Delling, Pajor & Werneck, *Round-Based Public Transit Routing*, Transportation
  Science (2015). Bags of non-dominated labels per stop per round.
- MCR extends this to multimodal labels (Delling et al., ESA 2013).

**Via today.** Each stop keeps `LABELS_PER_STOP = 4` labels, deduplicated by structure hash,
with `ROUTE_VARIANTS_PER_STRUCTURE = 2`. Rounds give the transfer criterion for free. Labels
are compared on static cost only. Walk time and mode set are kept only indirectly, through the
selection pass.

**Design.** Use labels with criteria (static cost, walking seconds), round = rides. Keep a bag
per stop and round with Pareto dominance, plus a bounded bag size and an ε-dominance on cost
(for example 60 s) to keep bags small. Mode-set diversity stays in selection, as AGENTS.md
requires.

**Constraint check.** It is still round-based and keeps the best corridor per ride count and
per mode set. No expansion cap is allowed ("without an expansion cap"), so bags must be bounded
by dominance, not truncation. That is the main risk for latency.

**Experiment.**
1. Implement behind a flag in `corridors/search.rs`. Run `corridor_bench` on a quiet machine.
2. Metrics:
   - latency p50/p95/p99 vs EXP-001;
   - the retained corridor set: how many scenarios gain a corridor with ≥ 3 min less walking at
     ≤ 2 min more static cost;
   - live Phase 2 arrival and walk deltas on 20 scenarios.
3. Stop if p95 more than doubles without a quality gain.

---

## Priority after EXP-008

| Order | ID | Why |
| --- | --- | --- |
| 1 | B23 step 1 | Cheap, quota-friendly. Grounds every ETA claim and tells whether B18 estimates are good |
| 2 | B21 | Inside fixed-corridor evaluation, no data needed, offline on Metro/TRA |
| 3 | B25 | Offline-only. Clear before/after with corridor_bench once the machine is quiet |
| 4 | B22 | Needs Valhalla Matrix in the build; larger change |
| 5 | B24 | Needs a 7-day log first; start the logger early, analyse later |
