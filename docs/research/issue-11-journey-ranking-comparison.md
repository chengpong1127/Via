# Issue #11 research: how journey planners rank Journey Options

Research date: 2026-08-29  
Scope: Publicly documented ranking behavior for public-transit Journey Options in Google Maps, Apple Maps, Citymapper, Transit, and Moovit, with OpenTripPlanner and Valhalla as transparent implementation references.

## Executive answer

No reviewed consumer map app publishes a reproducible formula for ranking public-transit Journey Options. It would be unsafe to claim that Google Maps, Apple Maps, Citymapper, Transit, or Moovit uses a particular weighted score, lexicographic order, or Pareto algorithm.

The disclosed behavior nevertheless shows a common product shape:

1. calculate several Journey Options;
2. apply hard eligibility or user-preference rules where needed;
3. compare time with rider burdens such as Walking and Transfers;
4. use Live Status and disruption information to improve the estimate or explain risk;
5. show a small, useful set rather than every candidate; and
6. let the rider change the priority, for example to less Walking or fewer Transfers.

OpenTripPlanner (OTP) makes this shape explicit. Its RAPTOR search produces Pareto-optimal candidates using time, number of Transfers, and generalized cost; a later filter chain removes expensive and similar Journey Options. Valhalla is a more direct scalar-cost example: elapsed time, penalties, and preference factors influence one path cost. These engines show that a numeric server-side score is normal, but OTP is the closer match for Via because it keeps feasibility, Pareto search, generalized-cost ranking, and diversity filtering as separate concerns.

**Recommendation for Via:** use a server-owned ranking pipeline, not one public “reliability percentage.” First remove Journey Options whose required Live Status is unavailable. Then classify the remaining Journey Options as **Reliable** or **Feasible** from the safety-buffer rules already agreed for issue #11. Rank Reliable Journey Options before Feasible Journey Options. Within each class, use a documented generalized cost to compare arrival time, Transfers, Walking, waiting, and tight-buffer risk. Finally, apply dominance and similarity filters and return up to three materially different Journey Options. The API should return the class, final order, labels, and machine-readable reasons; an internal scalar score may be returned only in diagnostic output.

## Evidence rules

- **Disclosed fact** means an official product/help/developer document or official source code states the behavior.
- **Inference** means the behavior is consistent with the disclosed facts, but the implementation is proprietary or unstated.
- “Best,” “fastest,” and “recommended” are kept distinct. A product calling one result “best” does not disclose how it computed that result.
- This report does not infer a proprietary formula from screen order or marketing language.

## Comparison

| Planner | What is publicly disclosed | Scalar score disclosed? | Pareto or lexicographic order disclosed? | Live Status / reliability behavior | Alternative diversity |
| --- | --- | --- | --- | --- | --- |
| Google Maps | The best route is highlighted. Google says transport options are ranked from a combination of factors that can include duration, distance, price, mode preference, and relevance; mode preference and duration are generally most important. Transit requests can prefer less Walking or fewer Transfers. | **No.** A combination of factors is disclosed, but not weights or a formula. | **No.** | Public-transit schedules and disruptions are used, but no official source reviewed defines missing Live Status as a display gate. | Routes API can return a default route and up to three alternatives, but its diversity rule is not disclosed. |
| Apple Maps | Transit results show estimated time and, where available, cost. Riders can prefer bus, rail, or ferry and choose a Journey Option. Maps can show schedules and delays. | **No.** | **No.** | Delays can be displayed. No reviewed source states a missing-live-data gate or reliability tier. | Multiple routes are shown, but no selection or similarity rule is disclosed. |
| Citymapper | The default convention is described as fastest; “Simple” focuses on fewest Transfers. Current “Best for you” balances speed, affordability, simplicity, physical effort, and disruption risk, and can explain factors such as tight Transfers and service frequency. Step-free routing makes accessibility a hard condition and ranks simplicity ahead of travel time. | **No.** It discloses factors, not their calculation or weights. | **No.** Step-free results demonstrate a product-level priority tier, but not the underlying algorithm. | Disruption risk, tight Transfers, and frequency can affect or explain recommendations. Citymapper does not publish a general Live Status availability gate. | It selects a few options and currently highlights two “Best for you” results, but no general similarity threshold is disclosed. |
| Transit | Transit describes its real-time trip planner as finding the fastest Journey Option. Its desktop planner lists the “best” option first, can prioritize less Walking, and can put accessible options first. | **No.** | **No.** The interface exposes preference switches, but their composition is undisclosed. | Real-time arrivals and disruptions improve trip planning. Transit explicitly continues to return schedule-based transit Journey Options offline, so Live Status is not a universal gate. | It returns one or more options; no diversity formula is disclosed. |
| Moovit | Suggested results expose duration, Walking, Transfers, departure/arrival time, and Travel Mode. A “fastest route” is used for some instant suggestions. Preferred modes personalize results, but less-preferred modes may still appear when needed. Newer smart features consider preferences, road conditions, weather, disruptions, tight Transfers, frequency, early arrival, and minimal Walking. | **No.** | **No.** | Moovit distinguishes live arrivals from scheduled arrivals and can show scheduled results. Tight Transfers and disruptions can be warnings or recommendation inputs, not documented universal gates. | Several suggestions are shown, but no similarity rule is disclosed. |
| OpenTripPlanner | RAPTOR uses time, number of Transfers, and generalized cost as Pareto criteria. Itinerary filters then sort, cap relative generalized cost, and group similar results. | **Yes, internally.** Generalized cost is an integer diagnostic value assembled from configurable time-equivalent costs and reluctance factors. | **Yes, Pareto.** Not a reliability-first lexicographic tier by default. | Real-time updates can be used or ignored; vehicle-rental availability can be a planning input. Reliability classes are application policy, not an OTP default. | Explicit similarity grouping can keep one result above 85% similarity and up to three above 68% similarity; same routes/stops can also be removed. |
| Valhalla | Dynamic run-time costing adds elapsed-time costs, path-only penalties, and multiplicative factors. Transit preferences include bus, rail, and Transfers; Walking distances can be capped. | **Yes.** This is a scalar path-cost model. | **No public Pareto transit result model in the reviewed docs.** | No Via-like Live Status availability or Reliable/Feasible classification is documented. | Not a strong public precedent for a diversified transit result set. |

## Consumer apps

### Google Maps

**Disclosed facts.** Google Maps says that, when multiple routes exist, the best one is highlighted and the others remain alternatives. Its help documentation gives the broadest official ranking statement found for a consumer app: transport options are ranked using a combination of objective factors that can include duration, distance, price, mode preference, and relevance; it says mode preference, Journey duration, and sometimes price are generally the most important ([Google Maps Help: directions and route ranking](https://support.google.com/maps/answer/144339/get-directions-amp-show-routes-android?hl=en-GB)).

For transit specifically, Google exposes `LESS_WALKING` and `FEWER_TRANSFERS` as preferences that *influence* the returned route. The wording is preference/bias, not a hard constraint ([Google Routes API: TransitPreferences](https://developers.google.com/maps/documentation/routes/reference/rest/v2/TransitPreferences), [Google Directions API: transit routing preference](https://developers.google.com/maps/documentation/directions/get-directions#transit_routing_preference)). The Routes API can return a default route plus up to three alternatives, and identifies them as `DEFAULT_ROUTE` and `DEFAULT_ROUTE_ALTERNATE`; sometimes no alternative is available ([Google Routes API: alternative routes](https://developers.google.com/maps/documentation/routes/alternative-routes)).

Google also accepts operator-provided minimum, timed, forbidden, and recommended Transfers. When the operator does not provide a time for a timed Transfer, Google calculates one from multiple parameters and includes inter-stop Walking where applicable ([Google Transit Partners: Transfers](https://support.google.com/transitpartners/answer/6377424?hl=en)).

**Not disclosed.** Google does not publish the formula for the default transit Journey Option, the relative weights of time/Walking/Transfers, a Pareto definition, a similarity threshold for alternatives, or a rule that suppresses a Journey Option when Live Status is absent.

**Inference for Via.** Google is evidence for multi-factor ranking and rider-selectable preference profiles. It is not evidence for one particular weighted score. Its “up to three alternatives” pattern supports Via returning fewer than three when useful alternatives do not exist.

### Apple Maps

**Disclosed facts.** Apple Maps presents transit routes with estimated travel time and, in supported systems, fare. The rider can choose a Journey Option and can prefer particular transit methods ([Apple Support: get transit directions on iPhone](https://support.apple.com/guide/iphone/get-transit-directions-ipha44f57caa/26)). Apple also exposes schedules and delays for nearby transit departures. On macOS, the transit preference is by bus, rail, or ferry ([Apple Support: get directions in Maps on Mac](https://support.apple.com/guide/maps/get-directions-mps4d8a6bd2f/mac)).

**Not disclosed.** The reviewed Apple material does not define what makes the first route “best,” publish a score, specify a priority order among time/Walking/Transfers/fare, define alternative diversity, or state that missing Live Status removes a Journey Option.

**Inference for Via.** Apple provides UI precedent for server-selected alternatives plus rider-visible time, fare, and Travel Mode preferences. It provides no algorithmic precedent strong enough to choose between scalar and lexicographic ranking.

### Citymapper

**Disclosed facts.** Citymapper states that the conventional/default goal is to get the rider from A to B fastest. Its “Routing Powers” add explicit profiles: **Simple** focuses on the fewest Transfers, while mode-specific and step-free profiles change which Journey Options qualify ([Citymapper: Routing Powers](https://citymapper.com/news/2293/)). For step-free routing, Citymapper says routes contain no stairs, account for elevators and adjusted Walking times, avoid complicated Transfers and wide platform gaps, and prioritize simplicity over travel time ([Citymapper: step-free routing](https://citymapper.com/news/2005/step-free-routes)). This is useful evidence that a hard eligibility gate can precede a different ranking policy.

Citymapper's current “Best for you” explanation lists speed, affordability, simplicity, physical effort, and disruption risk as trade-offs. It says the top two results can use hidden factors such as tight Transfers and service frequency and provides rider-facing explanations ([Citymapper: Best for you](https://www4.citymapper.com/news/2838/citymapper-now-uses-ai-powers-to-help-you-choose-the-best-route)).

**Not disclosed.** Citymapper does not publish weights, a deterministic score, a Pareto algorithm, a reliability cutoff, or a similarity threshold.

**Inference for Via.** Citymapper is the strongest consumer-app precedent for: hard gates for a special contract; a default time-oriented rank; alternate profiles; and reason tags explaining why a result is recommended. Its disruption-risk language supports including risk inside ranking/explanation after data availability is established, but does not imply that unavailable data should remain displayable.

### Transit

**Disclosed facts.** Transit describes its real-time trip planner as a way to pick the fastest Journey Option and says service disruptions are included ([Transit: version 4.0](https://blog.transitapp.com/transit-4-0-is-now-live-2329f60fb3bb/)). Its desktop planner lists the “best” Journey Option first, exposes an option to minimize Walking, can put accessible Journey Options first, and refreshes results with current vehicle locations and arrival times ([Transit Support: desktop trip planner](https://help.transitapp.com/article/534-trip-planner-for-desktop)).

Transit also documents the opposite of a hard Live Status gate: offline planning still returns schedule-based transit, Walking, and cycling Journey Options, while real-time arrivals and service alerts are unavailable ([Transit Support: offline behavior](https://help.transitapp.com/article/90-does-transit-work-offline)).

**Not disclosed.** Transit does not publish the formula behind “best” or “fastest,” the priority between accessible-first and time, the alternative-selection method, or a reliability score.

**Inference for Via.** Transit supports using current Arrival Estimates in arrival-time ranking and supports explicit preference profiles. Its schedule-only fallback conflicts with Via's issue #11 product decision; it is a market comparison, not a rule Via should copy.

### Moovit

**Disclosed facts.** Moovit shows several suggestions and exposes total duration, Walking time, number of Transfers, departure time, arrival time, and Travel Mode. It marks a first-line arrival as live where real-time information exists, while its support material also distinguishes scheduled arrivals ([Moovit Support: plan a trip](https://support.moovitapp.com/hc/en-us/articles/212011605-Plan-a-Trip-Get-Directions), [Moovit Support: getting started](https://support.moovitapp.com/hc/en-us/articles/211391869-Getting-Started-with-Moovit)). Smart Trip preferences influence suggested Travel Modes, but Moovit may still show a less-preferred mode if no better Journey Option exists ([Moovit Support: preferred transit types](https://support.moovitapp.com/hc/en-us/articles/5880749202834-Trip-options-based-on-preferred-transit-types)).

Moovit's current assistant material says recommendations can consider rider preferences, changing road conditions, weather, and disruptions. It exposes insights including short Transfer risk, high frequency, earliest arrival, and minimal Walking ([Moovit Support: Moovit Assistant](https://support.moovitapp.com/hc/en-us/articles/35543186031634-Moovit-Assistant-Smarter-Trip-Planning-Than-Ever-Before)).

**Not disclosed.** Moovit does not publish its formula, weights, a Pareto set, a reliability cutoff, or a diversity rule.

**Inference for Via.** Moovit supports keeping Feasible Journey Options visible with explicit warnings and attributes. It also demonstrates that mode preference can be a soft preference rather than a hard filter, but Via's Bus/Metro/YouBike filters are already domain-level request constraints and should remain hard.

## Transparent routing engines

### OpenTripPlanner

OTP is the best public implementation reference for Via. Its documentation says transit search can produce many Pareto-optimal results using **time, number of Transfers, and generalized cost**. The itinerary filter chain then sorts, modifies, and removes less-preferred results ([OTP RouteRequest](https://docs.opentripplanner.org/en/dev-2.x/RouteRequest/)). This is not one of the two simplistic choices “Pareto or score”: OTP uses both, at different stages.

Generalized cost is a time-equivalent integer used internally and exposed for debugging. OTP's configurable preferences include Walking reluctance, waiting reluctance, board cost, Transfer slack, mode reluctance, preferred-service penalties, rental pickup/drop-off cost, and many other terms ([OTP RouterConfiguration](https://docs.opentripplanner.org/en/dev-2.x/RouterConfiguration/), [OTP GraphQL Itinerary](https://docs.opentripplanner.org/api/dev-2.x/graphql-gtfs/types/Itinerary)). A generalized-cost limit can remove a transit result when it is too expensive relative to another result, with a default limit function of `15m + 1.5 × cost` for overlapping results.

OTP treats result diversity as its own post-search concern:

- group results at least 85% similar and keep one;
- group results at least 68% similar and keep up to three;
- optionally remove a later result with the same routes and stops; and
- optionally remove results with the same first or last transit trip.

The percentages are OTP defaults, not universal usability findings. Via should not copy them without a Taipei acceptance corpus. The important pattern is separation: generalized cost chooses the better result *inside* a similarity group, while similarity decides whether both results deserve display.

OTP can use vehicle-rental availability as a planning input and can apply real-time updates, but it does not define Via's Reliable/Feasible product classes. Those remain a Via policy over its pinned Network Snapshot.

### Valhalla

Valhalla documents a classic scalar-cost model. Dynamic run-time costing combines:

- **costs**, which add seconds to path cost and estimated duration;
- **penalties**, which add only to path cost; and
- **factors**, which multiply edge cost to favor or avoid attributes.

Its multimodal transit settings include preferences for Bus, rail, and Transfers, plus maximum Walking distance at the beginning/end and between transit services ([Valhalla turn-by-turn API reference](https://github.com/valhalla/valhalla-docs/blob/master/turn-by-turn/api-reference.md#costing-models)). Valhalla's source code also represents a transit Transfer as a cost calculation ([Valhalla `DynamicCost`](https://github.com/valhalla/valhalla/blob/master/src/sif/dynamiccost.cc)).

Valhalla therefore proves that a single numeric cost can encode time and rider burden in a production routing engine. It is weaker evidence for Via's complete pipeline because the reviewed documentation does not show a transit Pareto result set, a Reliable/Feasible tier, or OTP-style similarity filtering.

## Implications for issue #11

### 1. A server-side score is reasonable, but it is not the whole contract

The industry evidence does not support returning candidates and letting each client rank them. Google, Citymapper, Transit, and Moovit present an already ordered result set; OTP and Valhalla calculate preference-aware cost inside the routing service. Via should also make ranking deterministic on the server so iOS, Android, and any later client show the same Journey Options for one Network Snapshot.

The score should be called **generalized cost** in engineering documentation, not “reliability score.” Reliability is a product classification; generalized cost is an ordering tool. Keep the numeric value internal because:

- its unit is artificial rather than a rider promise;
- it will need calibration;
- exposing it invites clients to re-sort; and
- a displayed number implies precision that the input data does not justify.

### 2. Use a tier before generalized cost

The accepted issue #11 discussion defines three states:

1. **Reliable Journey Option:** all required Live Status is available and the Journey Option passes the agreed safety buffers.
2. **Feasible Journey Option:** all required Live Status is available and the Journey Option remains physically possible, but a reliability buffer is not met—for example, only one YouBike/return slot or a one-minute Bus margin.
3. **Unavailable:** Live Status that the Travel Mode is expected to provide cannot be obtained or refreshed. Do not show the Journey Option.

Rank the first two states lexicographically: `Reliable` before `Feasible`. Do not try to encode this distinction only as a large numeric penalty. A penalty can eventually be outweighed by enough minutes saved, which would violate the agreed product meaning. This tier-plus-score design is analogous to Citymapper's hard step-free qualification before its simplicity-first ranking, although Via's exact classes are its own policy.

Taipei Metro is not “unavailable” merely because it has no forward train Arrival Estimate: issue #4 established that this datum is not provided. Via calculates Metro time from timetable/frequency and uses the Service Condition that is actually expected for that operator. Live Status availability must therefore be evaluated by Travel Mode and operator, not by one universal field list.

### 3. Proposed generalized cost inside each class

Use arrival time as the main rider outcome, then add explicit time-equivalent burdens:

```text
generalized_cost_seconds =
    calculated_journey_duration
  + transfer_count             * transfer_penalty_seconds
  + walking_duration_seconds   * walking_extra_factor
  + waiting_duration_seconds   * waiting_extra_factor
  + tight_buffer_penalty_seconds
```

The exact coefficients are unresolved calibration parameters, not facts learned from other apps. Begin with configuration, log each component separately, and test ordering examples rather than choosing weights by intuition. Do not include an unavailable-Live-Status penalty because those Journey Options have already been removed. Do not include a Reliable/Feasible penalty because the class is an earlier lexicographic tier.

Stable tie-breaking should follow generalized cost with calculated arrival, Transfers, Walking, departure time, and a stable Journey Option key. Recording all components makes “why did this rank first?” answerable without treating a single number as an explanation.

### 4. Keep dominance and diversity outside the score

A scalar score cannot ensure useful variety. Follow OTP's architecture:

1. remove dominated Journey Options;
2. group near-duplicates by the Bus/Metro service sequence and substantially shared Legs;
3. keep the lowest-cost Journey Option in each group;
4. select up to three groups, Reliable first and Feasible only when Reliable results do not fill the result set; and
5. return fewer than three rather than add an unavailable or redundant Journey Option.

Via should define its own material-difference thresholds from examples. OTP's 85%/68% defaults prove configurability, not that those values fit Taipei or Via's typed Legs.

### 5. Explain the order with labels and reasons

Consumer apps expose attributes rather than opaque scores: less Walking, fewer Transfers, fastest/earliest, high frequency, tight Transfer, and disruption risk. Via should do the same. Preserve the accepted labels:

- **Best now** only for the first Reliable Journey Option;
- **Fewer transfers** when strictly better than Best now on Transfers; and
- **Less walking** when strictly better than Best now on Walking.

Also return machine-readable reasons such as `earliest_reliable_arrival`, `fewer_transfers`, `less_walking`, `tight_bus_margin`, `low_pickup_bikes`, and `low_return_slots`. The client translates these into Guidance-adjacent text; it does not reinterpret the rank.

## Recommended issue #11 decision

Adopt this server pipeline:

```text
Planning Request + pinned Network Snapshot
    -> generate Pareto candidates
    -> remove candidates with unavailable required Live Status
    -> classify Reliable or Feasible
    -> remove dominated candidates
    -> compute generalized cost within each class
    -> group and remove near-duplicates
    -> sort Reliable first, then Feasible; generalized cost within each class
    -> select up to three materially different Journey Options
    -> apply Best now / Fewer transfers / Less walking labels and reasons
```

This is intentionally a hybrid:

- **hard availability gate** for data Via is supposed to receive;
- **lexicographic reliability class** to preserve the Reliable Arrival promise;
- **generalized scalar cost** for understandable trade-offs inside a class;
- **Pareto/dominance filtering** to avoid obviously inferior results; and
- **diversity filtering** to avoid three copies of the same Journey Option.

That design matches what can be learned responsibly from the market without pretending to know proprietary formulas. It also matches the McRAPTOR-plus-post-filter architecture already selected in [issue #8](./issue-8-routing-algorithms.md).

## Remaining calibration work

Issue #11 can decide the structure now. It should not freeze numeric weights until Via has an example corpus containing at least:

- 5–10 minutes faster versus one additional Transfer;
- short versus long Walking access;
- Reliable versus Feasible Journey Options;
- one versus three available YouBikes/return slots;
- tight versus comfortable Bus boarding margins;
- two Journey Options using nearly identical services;
- direct Walking versus a marginal transit benefit; and
- cases with one, two, three, and more than three useful Journey Options.

For each case, record the expected order and explanation. Fit coefficients to those decisions, then backtest on captured Network Snapshots. The generalized cost is acceptable only if small coefficient changes do not produce surprising class changes or unstable ordering.
