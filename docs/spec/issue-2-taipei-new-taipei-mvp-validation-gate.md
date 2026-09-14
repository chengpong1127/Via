# Issue #2 — Taipei–New Taipei MVP validation gate

Decision date: 2026-08-29  
Status: Current

## Decision

Via may expand beyond Taipei–New Taipei only after a field validation proves both that riders can make a Trusted Decision quickly and that the selected Journey Option remains feasible in real use. Speed without comprehension or field feasibility does not pass.

A **Trusted Decision** occurs when a rider selects a Journey Option without outside help, correctly identifies its Travel Modes and next required action, correctly understands the strength of its operational evidence, and the critical pickup, boarding, and Transfer actions remain feasible. **Time to Trusted Decision** begins when the complete Journey Options are usable and ends when the rider selects one and answers those comprehension checks correctly.

The gate is deliberately conjunctive. A failed hard safety, accuracy, unsupported-gap, or privacy requirement blocks expansion. Strong results elsewhere cannot average away a failed hard gate.

## Field scenario matrix

The study uses a fixed, versioned, replayable scenario matrix. It must cover:

- Journeys wholly within Taipei, wholly within New Taipei, and crossing between them;
- peak, off-peak, late-evening, and after-midnight Service Day conditions;
- Bus, Metro, YouBike, and multimodal Journey Options;
- zero-, one-, and two-Transfer Journeys;
- fresh operational evidence plus degraded, schedule-only, and unavailable cases that must suppress affected Journey Options;
- dense urban cores and weaker-coverage network edges;
- at least one real or safely simulated disruption; and
- at least one impossible YouBike pickup and one impossible YouBike return.

Every recorded trial identifies the scenario version, static network revision, and request-time evidence context used by its Planning Request. Field observations are timestamped so the displayed Journey Option can be reconciled with what the rider encountered.

## Rider cohorts and trial design

Recruit at least 60 riders. Include at least 15 members of each of these cohorts; one rider may qualify for more than one cohort, but every cohort must independently meet its thresholds:

1. daily Taipei–New Taipei transit riders;
2. occasional or unfamiliar riders;
3. regular YouBike users; and
4. riders with mobility, vision, language, or technology-confidence constraints.

Run at least 360 moderated field trials, with each rider completing six randomized Journeys. Balance coverage across scenario families and Travel Modes, and collect at least 30 observations for every critical scenario family. Randomize the order of Via and comparison products. Report first attempts separately from later attempts so learned familiarity cannot hide initial comprehension problems.

## Pass thresholds

### Time to Trusted Decision

After selecting a Journey Option, the rider must correctly identify:

1. the Travel Modes it uses;
2. the next required action; and
3. whether it is a Reliable or Feasible Journey Option and which live margin causes that class.

The study passes this measure only when:

- median Time to Trusted Decision is at most 30 seconds;
- p90 Time to Trusted Decision is at most 60 seconds;
- at least 90% of all trials answer all three checks correctly; and
- every rider cohort achieves at least 85% comprehension.

### Planning Request latency and reliability

Measure end-to-end time from submission of a Planning Request until complete Journey Options are usable. The deployed system must satisfy all of these limits:

| Condition | Threshold |
| --- | --- |
| Warm p50 | `<= 1 second` |
| Warm p95 | `<= 2 seconds` |
| Cold or cache-miss p95 | `<= 3.5 seconds` |
| Overall p99 | `<= 5 seconds` |
| Technical-error rate | `< 1%` |
| Any critical scenario p95 | `<= 4 seconds` |

A burst test at the expected expansion launch load must also keep the technical-error rate below 1%. The report separates warm, cold, cache-miss, stale-source, realtime-failure, and burst results rather than combining them into one favorable distribution.

### Field accuracy and safety

Reconcile Journey Options against timestamped field observations and retained planning-input revisions. The MVP passes only when:

- at least 95% of displayed Legs, Boarding Points, Travel Modes, and scheduled times are correct;
- at least 90% of selected Journey Options remain feasible without an unannounced missed Transfer, impossible boarding, or failed YouBike pickup or return;
- fewer than 2% of trials receive unsafe false assurance from stale, missing, or unsupported data;
- at least 95% of Bus Arrival Estimates fall within plus or minus three minutes of the observed arrival; and
- 100% of observed zero-bike, zero-return-slot, suspended, and unavailable cases are represented without conversion into normal service.

The unsafe-false-assurance and state-conversion requirements are hard gates. Missing or unknown evidence is never scored as correct merely because the rider happened to complete the Journey.

## Comparison baselines

Every scenario is paired against:

1. the rider's usual planning method; and
2. the strongest applicable public alternative, using Google Maps or Apple Maps together with an official operator or YouBike app when the Journey otherwise requires multiple apps.

Via passes the comparison only when it:

- is no worse by more than 10% on median Time to Trusted Decision;
- provides equal or better field feasibility in at least 90% of paired trials;
- reduces multi-app switching by at least 50% for multimodal Journeys; and
- has no statistically or practically meaningful regression for any rider cohort.

The report must publish paired results, sample sizes, uncertainty intervals, exclusions, and the predeclared rule used to judge practical significance. Product order is randomized to control for learning effects.

## Unsupported gaps and control comprehension

Via never silently fills an unsupported, missing, unknown, or stale gap. It suppresses a Journey Option that requires that evidence and explains why the affected Travel Mode is unavailable. It can show a Feasible Journey Option only when all required Live Status is fresh and the Journey Option is possible but a safety buffer is not met. Manual origin selection and Walking alternatives remain available where feasible.

The comprehension study passes only when:

- at least 95% of riders distinguish a Reliable Journey Option from a Feasible Journey Option and identify the tight live margin;
- at least 90% correctly understand that Bus, Metro, and YouBike filters are independent and Walking remains available as a connector;
- at least 95% understand that Start opens static Guidance and provides neither live navigation nor rider tracking;
- no rider cohort falls below 85% on any of these measures; and
- no unsupported gap produces a displayed Journey Option.

The final bullet is a hard gate. The filter and Start questions must be tested through observed tasks and neutral comprehension questions, not leading explanations.

## Expansion evidence and approval

The approval package is one versioned validation report containing:

- the scenario matrix and study protocol;
- anonymized trial results and cohort breakdowns;
- paired baseline comparisons;
- latency distributions and error rates by operating condition;
- timestamped field-observation reconciliation;
- unsupported-gap and control-comprehension results;
- failures, exclusions, uncertainty intervals, and deviations from the protocol; and
- reproducible references to the schedule revisions, freshness-policy versions, and planning-input revisions used.

Approval requires all of the following:

1. every hard safety, accuracy, unsupported-gap, and privacy threshold passes;
2. every other quantitative threshold passes overall and for every applicable rider cohort;
3. no unresolved severity-one or severity-two validation defect remains;
4. the results are reproduced during at least two separate weeks, including a peak-period session and a degraded-data session; and
5. both the product owner and engineering owner give written approval.

A failed hard gate blocks expansion. Any other miss requires a documented correction and retest; it cannot be waived by averaging it against stronger results. Changes to a threshold, scenario, exclusion rule, or calculation after evidence collection require a new report version and an explicit rationale.

## Privacy boundary

The validation study follows the accepted location and telemetry contract in issue #5. Operational reporting contains no rider location, Journey details, or stable rider identifier. Consented research records use study-scoped participant codes and the minimum data needed for the approved protocol. Planning-input references and field-observation records are stored separately from participant identity. Any optional qualitative recording requires explicit study consent and a stated retention period.

## Consequences

- Taiwan-wide expansion is an evidence-based release decision rather than a schedule milestone.
- Aggregate success cannot conceal a failure affecting an individual rider cohort.
- Via must preserve enough immutable operational evidence to reproduce validation results without retaining Planning Requests or rider Journeys in normal product telemetry.
- Unsupported coverage is a product state to explain, not data to infer or disguise.
- The gate tests the accepted static-Guidance product honestly: Start cannot be interpreted as live navigation or tracking.
