# Rust Axum on Vercel for Journey Planning

Status: Historical Research

_Issue #10 research; checked 2026-08-29._

## Decision

**Conditionally viable, but not yet performance-cleared.** A Rust Axum request API can be packaged as an official Vercel Function and is a reasonable prototype deployment for the Journey planner. Vercel publishes no Rust cold-start bound or request-latency SLO, however, so documentation cannot establish the required p95 of two seconds. That target must be proven with a production-region load test that includes warm, cold, and scale-out invocations plus the real Live Status upstreams.

The simplest viable shape is:

1. one Rust binary under `api/` containing the entire Axum `Router`;
2. the official `vercel_runtime` 2.x crate with its `axum` feature and `VercelLayer` adapter;
3. one catch-all rewrite to that Function;
4. one compute region selected for proximity to the authoritative transit/YouBike data and durable store;
5. no correctness dependence on process memory, local files, or post-response work; and
6. refresh work invoked separately and writing to an external durable store.

This is the same packaging pattern as Vercel's [official Axum example](https://github.com/vercel/examples/tree/0c0a5d0459311d16da0f882ccbe259620ab5433e/rust/axum): its [manifest](https://github.com/vercel/examples/blob/0c0a5d0459311d16da0f882ccbe259620ab5433e/rust/axum/Cargo.toml) builds one `api/axum.rs` binary, its [handler](https://github.com/vercel/examples/blob/0c0a5d0459311d16da0f882ccbe259620ab5433e/rust/axum/api/axum.rs) wraps an Axum router in `VercelLayer` and passes the resulting Tower service to `vercel_runtime::run`, and its [`vercel.json`](https://github.com/vercel/examples/blob/0c0a5d0459311d16da0f882ccbe259620ab5433e/rust/axum/vercel.json) rewrites all paths to that binary. This adapter is natural for Axum because an Axum `Router` implements Tower's `Service<Request<B>>` interface ([Axum API](https://docs.rs/axum/latest/axum/struct.Router.html)). It is not a conventional always-listening Axum server: Vercel owns ingress and invokes the service.

## Constraint assessment

| Area | Documented constraint | Consequence for the Journey planner |
| --- | --- | --- |
| Runtime maturity | Vercel's Rust runtime is an **official runtime**, available on all plans, but remains **Beta**. It uses Fluid compute and supports streaming, logs, request metrics, and (nominally) `waitUntil` ([Rust runtime](https://vercel.com/docs/functions/runtimes/rust)). | Suitable for a measured prototype. Beta status is a release-risk decision for a production reliability commitment; it is not evidence of a latency failure. Pin the runtime crate and builder inputs rather than floating versions. |
| Packaging | Every Rust entrypoint in `api/` becomes a Function and must be declared as a unique `[[bin]]`. The official example puts the whole Axum router in one binary. Function bundles have a 250 MB uncompressed limit; request and response bodies are limited to 4.5 MB ([function limits](https://vercel.com/docs/functions/limitations)). | Prefer one binary to reduce duplicated compilation/bundle overhead and cold-start surfaces. Keep large timetable or graph artifacts out of the deployed binary unless their measured size and initialization cost are acceptable. Journey Option JSON should remain comfortably below 4.5 MB. |
| Execution duration | The Rust docs say the runtime runs on Fluid compute. The Fluid overview gives plan defaults/maxima of 300/300 seconds on Hobby and 300/800 seconds on Pro/Enterprise, while the detailed limits page scopes its numeric Fluid-duration table to Node.js and Python ([Fluid compute](https://vercel.com/docs/fluid-compute), [function limits](https://vercel.com/docs/functions/limitations)). `maxDuration` is a general per-function setting ([project configuration](https://vercel.com/docs/project-configuration/vercel-json)). | The two-second target, not Vercel's maximum, is the binding limit for synchronous planning. Set explicit upstream timeouts and return a controlled response rather than letting a slow request approach the platform timeout. Do not rely on the broad Fluid table for a long-running Rust job without deployment verification or Vercel confirmation. |
| Cold starts | Vercel scales Functions to zero when idle and reuses instances when traffic continues ([Functions lifecycle](https://vercel.com/docs/functions)). Production Functions not invoked for two weeks are archived; their first invocation after unarchiving adds **at least one second more than the usual cold start** ([runtimes](https://vercel.com/docs/functions/runtimes)). Paid production environments generally receive a pre-warmed instance, but cold starts can still occur during low traffic or scale-out, and Vercel publishes no Rust-specific latency bound ([compute fundamentals](https://vercel.com/docs/fundamentals/what-is-compute)). | A two-second p95 cannot be inferred from native-code speed. Minimize binary size and initialization, lazily load optional assets, and measure cold/scale-out cohorts. Do not use periodic self-pings as a correctness mechanism. |
| Concurrency | Platform automatic scaling reaches up to 30,000 concurrent executions on Hobby/Pro and 100,000+ on Enterprise, with a burst limit of 1,000 concurrent executions per 10 seconds per region; excess traffic can receive `503 FUNCTION_THROTTLED` ([concurrency scaling](https://vercel.com/docs/functions/concurrency-scaling)). Fluid's optimized multiple-invocations-per-instance feature is explicitly documented only for Node.js and Python, even though Rust runs on Fluid ([Fluid compute](https://vercel.com/docs/fluid-compute)). A Function has 1,024 file descriptors shared across concurrent executions ([function limits](https://vercel.com/docs/functions/limitations)). | Do not assume a particular Rust per-instance concurrency level or that a single in-memory Axum state is serialized. Make shared state thread-safe, bound fan-out and connection pools, and load-test the deployed Rust adapter. Global scale ceilings do not prove latency under burst. |
| Regions | Functions default to `iad1` (Washington, D.C.) unless configured. There is no Taiwan compute region; nearby candidates include Hong Kong (`hkg1`), Tokyo (`hnd1`), Osaka (`kix1`), Seoul (`icn1`), and Singapore (`sin1`) ([region list](https://vercel.com/docs/regions)). Vercel recommends placing compute near the data source, not simply near the rider ([region configuration](https://vercel.com/docs/functions/configuring-functions/region)). Pro supports up to three regions; globally replicated data is required to benefit safely from multi-region compute. | Do not accept the default `iad1`. Begin with a single region near the slowest authoritative upstream/durable store, then benchmark likely candidates from Taiwan. Adding regions while keeping one remote database can increase latency and state inconsistency. |
| Networking | Outbound calls are supported, but Vercel uses dynamic egress IPs by default. Pro/Enterprise Static IPs provide stable shared egress; private networking and dedicated egress require Enterprise Secure Compute ([Vercel IP allowlisting guide](https://vercel.com/kb/guide/how-to-allowlist-deployment-ip-address)). Network connections consume the shared file-descriptor budget. | Public transit APIs need ordinary HTTPS and should work. If an upstream requires IP allowlisting, budget for Static IPs; if it is private-only, the deployment may require Enterprise networking or a public gateway. Reuse HTTP connections conservatively and apply per-upstream deadlines, retries with jitter, and circuit breaking. |
| Filesystem and process state | The deployed filesystem is read-only except for up to 500 MB of writable `/tmp`; instances can be paused, recycled, or multiplied ([runtimes](https://vercel.com/docs/functions/runtimes)). Fluid can reuse a process and its global state across invocations, but that is an optimization boundary rather than durable storage ([Fluid compute](https://vercel.com/docs/fluid-compute)). | `/tmp` and in-memory graph/cache data may accelerate a warm instance but cannot be authoritative. Persist refresh timestamps, Live Status snapshots, and any precomputed planning artifacts in an external durable store. Every instance must be able to reconstruct its state. |
| Response caching | Function responses are not cached by default. Public responses can opt into Vercel's CDN with `s-maxage`, `stale-while-revalidate`, and related headers ([CDN cache](https://vercel.com/docs/caching/cdn-cache), [cache-control semantics](https://vercel.com/docs/caching/cache-control-headers)). Vercel's Runtime Cache API is documented through the Node package `@vercel/functions`; the Rust runtime docs do not publish a first-class Rust Runtime Cache client ([Runtime Cache API](https://vercel.com/docs/functions/functions-api-reference/vercel-functions-package)). | CDN-cache only inputs whose URL fully identifies the response and whose staleness is acceptable. Do not cache rider-specific responses publicly. Live Status freshness and Reliable Arrival semantics require explicit timestamps and conservative TTLs. Use a language-neutral external store as the Rust baseline rather than assuming Node-only cache APIs. |
| Background refresh | Vercel's Rust docs list `waitUntil` as supported. As of this review, however, [Vercel issue #17351](https://github.com/vercel/vercel/issues/17351) contains a production reproduction in `vercel_runtime` 2.4.0 where the future stops after the response, and the proposed [fix #17350](https://github.com/vercel/vercel/pull/17350) remains open and unmerged. Vercel Cron invokes a Function; Pro/Enterprise schedules can run once per minute and are dispatched within the specified minute, while Hobby is once daily with up to 59 minutes of timing uncertainty ([Cron limits](https://vercel.com/docs/cron-jobs/usage-and-pricing)). Overlapping cron invocations are possible ([Cron management](https://vercel.com/docs/cron-jobs/manage-cron-jobs)). | **Do not use Rust `waitUntil` for production Live Status refresh until the issue is fixed and reverified.** Use an idempotent refresh endpoint triggered by Vercel Cron or an external scheduler/queue, with a lease or compare-and-swap guard. If the product requires reliable refresh more often than once per minute, Vercel Cron alone is a hard incompatibility. |

## Hard incompatibilities

The request-serving Function itself has no documented hard incompatibility with Axum. These surrounding requirements would force either architectural separation or reconsideration of the hosting constraint:

- an always-on Axum listener, resident singleton, or correctness-critical in-memory state;
- durable writes to the deployed filesystem;
- reliable post-response refresh using the currently affected Rust `waitUntil` path;
- background refresh more frequent or more precisely scheduled than Vercel Cron supports;
- private-network access or dedicated egress without the required Vercel plan/networking product;
- a production policy that forbids Beta runtimes; or
- treating the p95 two-second target as guaranteed by the host without an application-specific benchmark.

Long-lived computation is also the wrong shape for the synchronous Journey request. It should be moved to a durable worker/scheduler and publish an immutable or versioned snapshot that the planner reads quickly.

## What documentation establishes—and what it does not

Documentation establishes that Vercel can build and invoke a Rust Axum Tower service, stream responses, place it in configurable regions, scale it, and cache eligible HTTP responses. It also establishes finite bundle, payload, filesystem, descriptor, scheduling, and plan limits.

Documentation does **not** establish:

- Rust cold-start, warm-request, or scale-out latency;
- the deployed Rust adapter's per-instance concurrency behavior;
- latency and reliability of the selected transit and YouBike APIs from each candidate region;
- the cost of loading the actual street/transit graph and constructing Journey Options;
- whether 1 vCPU / 2 GB is enough or 2 vCPU / 4 GB materially improves tail latency;
- the best region for Taiwan once all upstream round trips are included; or
- end-to-end p95 as observed by a rider.

## Required measurement gate

Before accepting Vercel/Rust for production, deploy the one-binary shape to a production environment and test at least `hkg1` plus the region nearest the authoritative data store. Use the realistic graph, Journey query distribution, Live Status calls, response sizes, and expected burst profile.

Record end-to-end p50/p95/p99 and error rate separately for:

1. warm steady-state requests;
2. fresh-deployment/cold requests;
3. scale-out under burst concurrency;
4. cache hit, cache miss, and stale-upstream conditions; and
5. each candidate region.

The gate passes only if the combined production traffic distribution—not merely warm handler time—meets p95 two seconds with an agreed error budget and with upstream timeouts exercised. Vercel's own slow-Function guide recommends comparing repeated cold/warm requests, inspecting server-processing versus network time, and measuring production deployments ([debugging slow Functions](https://vercel.com/docs/functions/debug-slow-functions)).

## Recommendation

Proceed with a time-boxed deployment benchmark using the official single-binary Axum pattern. Keep the synchronous Function stateless, configure an Asia region explicitly, place durable state near the Function, and externalize refresh. Treat the hosting decision as **unproven** until the measurement gate passes. Treat Rust `waitUntil` as **blocked** until Vercel merges a fix and the deployed runtime is verified. If the team requires a non-Beta runtime or cannot externalize refresh/state, choose a long-running Rust host for the planner while leaving the frontend and cacheable responses on Vercel.
