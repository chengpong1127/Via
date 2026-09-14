# Issue #12 — Server refresh, cache, and failure architecture

Decision date: 2026-08-29  
Status: Superseded
Superseded by: ADR 0002 and `server/README.md`.

## Decision

This document records the superseded refresh architecture. The current server uses stateless Rust Axum request handlers, an immutable checked-in or published `TransitNetwork` artifact, and demand-driven realtime reads through an in-process async cache. PostgreSQL stores provider aliases; it is not the authority for a global Live Status snapshot or refresh cycle.

An external scheduler calls one authenticated, idempotent refresh endpoint. A leased PostgreSQL lock permits only one refresh cycle at a time. The refresh cycle fetches independent government feeds with bounded parallelism, normalizes each result, evaluates Freshness, and publishes a new Network Snapshot in one atomic transaction. A Planning Request never calls a government feed and never starts refresh work.

Each Planning Request pins one published Network Snapshot for its complete calculation. Refresh work never changes an existing Network Snapshot or Journey Option. If only some feeds refresh successfully, Via can publish a new Network Snapshot that contains the successful observations and explicit degraded, schedule-only, or unavailable evidence for the failed feeds. Prior observations never become fresh only because a refresh failed.

## Runtime and retained state

The first deployment uses one Axum router in one Vercel Rust Function. Every instance must be able to start from PostgreSQL after losing its process memory or local files.

PostgreSQL holds:

- normalized immutable Live Status observations;
- immutable Network Snapshot records and their selected observation references;
- feed-health state and refresh history;
- the current published Network Snapshot reference;
- refresh leases and publication versions; and
- immutable static planning artifacts when they are too large or costly to ship with the Function.

The design uses PostgreSQL semantics but does not put Neon-specific types in the domain model. Use Neon's pooled connection endpoint and a small configurable connection pool per Function instance.

The initial retention policy is:

- normalized observations and Network Snapshot metadata: seven days;
- feed-health history: 30 days; and
- Planning Requests and Journeys: no retention after the response completes.

Scheduled batch deletion enforces these limits. The limits are versioned configuration and can change after operational measurement. Retained Live Status and feed-health data never contain a Planning Request, Journey, rider location, or stable rider identifier.

## Refresh and publication

The scheduler starts a refresh once per 60 seconds for TDX feeds that declare a one-minute update frequency. It authenticates the refresh endpoint. The endpoint is idempotent for one scheduled refresh key.

PostgreSQL grants a time-limited refresh lease. If another invocation holds a valid lease, the later invocation exits without starting duplicate work. A refresh initially permits no more than four simultaneous upstream requests. This limit, upstream deadlines, retry limits, and circuit-breaker policy are configurable.

Each source produces one of these refresh results:

- a successfully normalized new observation set;
- unchanged but successfully checked data;
- a rejected payload with explicit validation reasons;
- a rate-limited or source-error state; or
- a missing or timed-out response.

After all bounded source work finishes, one PostgreSQL transaction verifies the lease and expected publication version, stores the completed results, and changes the current Network Snapshot reference. If that check fails, the transaction does not publish. Planning Requests continue to use the previous published Network Snapshot until publication completes.

A partial refresh can be published when the new Network Snapshot remains internally coherent. It preserves the state and time of each source. Failed source data can support only the evidence class allowed by its evaluated Freshness. A partial refresh cannot turn missing, unknown, or expired evidence into normal operation.

## Freshness policy

Freshness is evaluated separately for each observation family, source capability, and use. The result keeps its evaluation time, policy version, and reason.

The initial TDX one-minute Live Status policy is:

| Observation age | State | Planning use |
| --- | --- | --- |
| `<= 120 seconds` | fresh | Can supply live evidence for Reliable Arrival. |
| `> 120 and <= 180 seconds` | degraded | Retained for diagnosis and explicit source state, but cannot support a Journey Option when this observation is required Live Status. |
| `> 180 seconds` | unavailable | Cannot support a Live Status claim or Reliable Arrival. Scheduled evidence can still be used when permitted. |

These limits are Via policy, not a provider guarantee. Slower feeds use explicit source-specific limits. They do not inherit the TDX limits. The policy must use the distinct `measured_at`, `published_at`, and `received_at` meanings defined by the normalization model. `normalized_at` does not prove Freshness.

## Cache boundaries

Via does not put Planning Request responses in the public CDN cache. It does not cache a Planning Request, Journey, rider location, or Journey details in shared infrastructure.

A warm Axum instance can cache only immutable data keyed by its revision or Network Snapshot ID, including:

- static timetable and network planning artifacts; and
- normalized inputs for a published Network Snapshot.

The handler resolves and pins the published Network Snapshot through PostgreSQL. A memory-cache miss is reconstructed from PostgreSQL. Cache eviction or instance replacement changes performance only, not results. The first architecture does not add Redis or depend on a Rust-specific Vercel Runtime Cache client.

## Planning concurrency

Vercel can run many Function instances. Via does not assume that Rust invocations are serialized within an instance. Shared in-memory cache values are immutable after publication, and cache containers must be thread-safe.

Each Planning Request pins one Network Snapshot before calculation. It makes no upstream government request. Database connections, blocking work, and internal fan-out are bounded. The deployed system must measure warm, cold, and burst behavior before it claims the two-second p95 target.

## Failure boundaries

Journey Option construction and validation are isolated when the failure is local to one Journey Option. Examples include an invalid Leg, an impossible Transfer, malformed source data used only by that Journey Option, or an invariant failure local to that Journey Option. Via discards that Journey Option and continues to evaluate the remaining Journey Options.

The server records only an anonymous reason code and count for a discarded Journey Option. It does not record the Planning Request, Journey, location, Legs, or Journey details.

The Planning Request result contract is:

| Outcome | HTTP class | Meaning |
| --- | --- | --- |
| One or more valid Journey Options survive | `200` | Return the valid Journey Options. A failure local to one Journey Option does not fail the response. |
| Calculation completes but no feasible Journey Option exists | `200` | Return an empty result with an explicit domain reason. |
| The Planning Request is invalid | `400` | Reject before planning and identify the invalid input field without echoing sensitive values. |
| No coherent Network Snapshot can be pinned, or all permitted transport evidence is unavailable | `503` | Report temporary service unavailability. |
| The planner fails globally or detects inconsistent shared internal data | `500` | Return no partial Journey Options from that calculation. |

A failure applies to the complete Planning Request only when the server cannot trust the calculation as a whole. One invalid Journey Option does not fail the complete Planning Request.

## Validation contract

The implementation is conformant only when automated checks prove that:

- losing process memory does not change correctness;
- overlapping scheduler calls produce at most one published refresh result;
- retrying the same refresh key is idempotent;
- a failed publication-version check cannot replace a newer Network Snapshot;
- every Journey Option in one response uses the same pinned Network Snapshot;
- a partial refresh preserves each source's actual Freshness and failure reason;
- stale observations never become fresh after a source failure;
- the 120-second and 180-second Freshness boundaries behave exactly as specified;
- required Live Status classified as degraded or unavailable never supports a displayed Journey Option;
- slower sources use their own policy;
- a warm cache hit and a PostgreSQL reconstruction produce the same planning inputs;
- Planning Requests and Journeys never enter CDN, memory-cache, operational-log, or retained Live Status records;
- one invalid Journey Option does not remove valid Journey Options;
- a planner-wide invariant failure never returns partial Journey Options;
- no feasible Journey Option is distinct from temporary service unavailability; and
- deployed warm, cold, cache-miss, stale-source, partial-refresh, and burst tests report p50, p95, p99, and error rate.

## Consequences

- PostgreSQL is the first operational dependency for both request serving and refresh publication. Its availability and connection limits are part of the server reliability budget.
- The request path stays small and predictable because it does not wait for government APIs.
- Partial provider failure can reduce the strength or number of Journey Options without causing a search-wide outage.
- Warm-instance caching improves latency without adding an authoritative cache system.
- A sub-minute refresh requirement, if introduced later, needs a scheduler or worker that can meet that cadence; it cannot be assumed from Vercel Cron.
- The architecture remains consistent with the provider-neutral identity boundary and immutable Network Snapshot model accepted in issue #9 and ADR-0001.
