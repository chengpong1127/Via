# Issue #11 — Journey Option ranking and diversity

Decision date: 2026-08-29  
Status: Current

## Decision

Via ranks Journey Options on the server with a versioned, deterministic pipeline. Explicitly impossible actions are an eligibility gate. Every displayed Journey Option is then classified as **Reliable** or **Feasible** from the timing and availability evidence actually used to construct it. Journey usefulness is the primary ordering signal: a materially earlier arrival outranks the other class, while Reliable is preferred when arrivals are within the configurable reliability-preference window. Dominance and diversity filters use the same ordering policy.

The server returns up to three materially different Journey Options in final order. Native clients render that order and the server-supplied class, label, and reasons. They never calculate a score or reorder results.

## Live Status eligibility

A Journey Option is eligible when its critical Legs and actions remain physically and operationally possible. Timing for every transit mode is resolved realtime-first with a static fallback: fresh usable realtime is preferred, while missing, empty, stale, not-yet-departed, timed-out, rate-limited, or failed realtime falls back to the corresponding static schedule or frequency estimate. Static fallback remains valid evidence for `feasible` ranking; it does not claim realtime reliability.

Required evidence is capability-specific:

- a Bus, Metro, TRA, or THSR Leg uses the selected `EffectiveTrip` timing basis; `RealtimePrediction` and `DelayAdjustedSchedule` are realtime-backed, while `Schedule` and `FrequencyEstimate` are static-backed;
- applicable Service Conditions remain required only when the source contract supplies them and they are blocking; transit timing does not require fresh realtime when static fallback can calculate the Leg;
- a YouBike Leg requires pickup and return availability when the provider supplies it; fresh usable realtime can support `reliable`, while missing, stale, degraded, unavailable, or rate-limited availability produces `feasible` unless an action is explicitly disabled or has zero inventory/capacity;
- a Metro Leg requires each Service Condition that the operator/source contract is expected to provide;
- Walking is calculated and does not require Live Status; and
- a datum that an operator does not provide is not treated as missing expected Live Status.

Taipei Metro can calculate expected wait and arrival from published frequency, station-to-station duration, and Transfer duration when a future train Arrival Estimate is unavailable. Missing future realtime is therefore eligible through static fallback, while an explicitly impossible action or blocking Service Condition still makes the affected Journey Option ineligible.

Schedule and frequency data can generate and calculate candidate Legs. The selected timing basis is carried through reconstruction and ranking so the class reflects the evidence used.

## Reliable and Feasible classes

Every eligible Journey Option has one of two server-owned classes:

| Class | Meaning |
| --- | --- |
| `reliable` | All transit timing facts and required YouBike availability facts are backed by fresh usable realtime, and all critical Legs and Transfers meet every configured safety buffer. |
| `feasible` | The Journey Option remains physically and operationally possible, but at least one timing or availability fact uses static, unknown, stale, degraded, unavailable, or otherwise non-realtime evidence, or a configured safety buffer is not met. |

`feasible` describes a possible Journey Option without claiming that every critical fact is currently backed by fresh realtime. It may also describe a live Journey Option with a tight margin.

The initial safety policy is:

- a Metro-to-Metro Transfer is Reliable when its available time includes the physical Transfer duration plus three minutes;
- a Transfer involving a Bus is Reliable when its available time includes the physical Transfer duration plus five minutes;
- a tighter Transfer is Feasible only when the physical Transfer itself still fits;
- a YouBike Leg is Reliable when the pickup station has at least three available bikes and the return station has at least three available return slots;
- a YouBike Leg with one or two bikes or one or two return slots is Feasible when both actions remain enabled; and
- zero bikes, zero return slots, a disabled action, an impossible Transfer, a cancelled critical Leg, or a blocking Service Condition makes the Journey Option ineligible.

Safety thresholds and station-specific overrides are versioned policy. A larger verified station-specific Transfer duration or buffer replaces the corresponding default.

## Server ranking pipeline

For one Planning Request and one frozen request-local evidence context, the server:

1. generates Pareto candidates;
2. removes candidates with an explicitly impossible critical action;
3. classifies each survivor as Reliable or Feasible;
4. removes candidates dominated under the arrival/class policy;
5. orders candidates by the arrival/class policy and deterministic tie-breakers;
6. groups and removes near-duplicates, retaining the earlier policy-preferred candidate;
7. selects up to three materially different Journey Options; and
8. applies labels and machine-readable ranking reasons.

The calculation pins a `ranking_policy_version`. Identical Planning Request inputs, schedule revision, ranking policy version, and realtime evidence must produce the same ordered results. Cost arithmetic uses deterministic integer or fixed-point operations, not platform-dependent floating-point tie behavior.

## Generalized cost

When calibrated generalized-cost coefficients are available, Via may use a time-equivalent internal value within the same class:

```text
generalized_cost =
    calculated Journey duration
  + Transfer count × Transfer penalty
  + Walking duration × additional Walking factor
  + waiting duration × additional waiting factor
  + tight-buffer penalty
```

The coefficients are versioned configuration calibrated against an approved Taipei–New Taipei ordering corpus. They are not rider-facing facts and are not part of the public client contract. Every calculation records the component values and policy version so the order can be explained and replayed.

No coefficient can replace the cross-class arrival policy. Availability and class remain explicit product facts, not large score penalties.

The deterministic reference comparison is:

1. when classes differ and arrivals are more than the configured reliability-preference window apart, earlier calculated arrival;
2. when classes differ and arrivals are within that window, Reliable class;
3. for equal classes, earlier calculated arrival;
4. fewer Transfers;
5. less Walking;
6. earlier calculated arrival as a final tie-breaker;
7. earlier departure; and
8. stable Journey Option key.

The default reliability-preference window is five minutes and is explicit `RankingPolicy` configuration. After calibration, the same fields remain deterministic tie-breakers.

## Dominance

Journey Option A dominates Journey Option B when A is no worse under the arrival/class comparison policy and:

- arrives no later;
- has no more Transfers;
- has no more Walking; and
- is strictly better on at least one of those values.

For different classes, Reliable is no worse only within the reliability-preference window; outside that window, the earlier arrival is no worse regardless of class. Via removes B before diversity selection. A different Travel Mode or service sequence does not preserve a fully dominated Journey Option.

## Diversity and result count

An alternative is materially different when it has at least one of these properties:

- a different main Bus or Metro service sequence;
- fewer Transfers; or
- at least five minutes less Walking.

Journey Options with the same main service sequence, the same Transfer count, arrival times within five minutes, and less than five minutes difference in Walking are near-duplicates. Via keeps the lower-generalized-cost Journey Option, followed by the deterministic tie-breakers.

A different service sequence alone is not enough when the alternative arrives more than 15 minutes later and offers neither fewer Transfers nor at least five minutes less Walking.

Via returns fewer than three Journey Options when availability, feasibility, dominance, similarity, or material-difference filtering leaves fewer than three. It never includes an unavailable, dominated, or redundant Journey Option only to reach three.

## Labels

The server assigns at most one label to each returned Journey Option, in this priority order:

1. **Best now** — the first-ranked selected Journey Option, including a Feasible option when its materially earlier arrival puts it first;
2. **Fewer transfers** — a returned Journey Option with strictly fewer Transfers than Best now; and
3. **Less walking** — a returned Journey Option with strictly less Walking than Best now.

The first option receives `earliest_reliable_arrival` when it is Reliable and `earliest_arrival` when it is Feasible. A Journey Option that qualifies for both alternative labels receives **Fewer transfers** because it has higher label priority.

The API also returns machine-readable reasons such as `earliest_arrival`, `earliest_reliable_arrival`, `fewer_transfers`, `less_walking`, `tight_transfer_margin`, `low_pickup_bikes`, and `low_return_slots`. The numeric generalized cost remains internal except in authorized diagnostic output.

## Validation contract

The implementation is conformant only when automated fixtures prove that:

- missing, stale, degraded, not-yet-departed, timed-out, rate-limited, or unusable transit realtime falls back to static timing and classifies the affected Journey Option as Feasible when it remains possible;
- missing data that an operator is not expected to provide does not create false degradation;
- a Taipei Metro Journey Option can use a calculated wait without claiming a future train Arrival Estimate;
- the three-minute and five-minute Transfer safety boundaries classify exactly;
- zero bikes or return slots, and disabled pickup or return actions, block the affected Journey Option; unknown or unavailable bike availability remains Feasible;
- a materially earlier Feasible Journey Option precedes a substantially slower Reliable Journey Option;
- a Reliable Journey Option precedes a Feasible Journey Option whose arrival is within the configured reliability-preference window;
- dominance, near-duplicate, five-minute Walking, and 15-minute delay boundaries behave exactly as specified;
- labels follow their definitions and priority;
- one or two useful Journey Options are returned without filler; and
- replay with the same inputs, evidence, and policy version produces byte-equivalent ordering, classes, labels, and reasons.

## Consequences

- Via can show a possible Journey Option with a tight live margin without weakening the meaning of Reliable Arrival.
- A feed problem lowers confidence to Feasible when the journey remains possible instead of producing a false Reliable claim.
- The score can be calibrated without changing the eligibility, class, label, or diversity contracts.
- Clients receive explainable decisions and cannot drift in ranking behavior.
- The market comparison and calibration guidance are recorded in [the issue #11 ranking research](../research/issue-11-journey-ranking-comparison.md).
