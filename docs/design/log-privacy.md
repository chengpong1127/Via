# Log and coordinate privacy

Decision record: [ADR 0001](../adr/0001-no-request-coordinates-retained.md). Owner
decision 2026-11-23. Shipped in PRs #44 (server) and #47 (monitoring); AWS node pieces
are in `server/docs/deploy-aws.md` section 10 and `server/docs/operations.md`.

## What was found
- The only real leak in `server/src` was reqwest errors wrapped by the TDX client: their source chain and Debug output carried the full request URL with query string. Fixed by dropping the URL when the error is built and redacting query and fragment in one helper.
- Spans use `skip_all` and a path-only HTTP span; metric labels are static (`dataset` only); no `tracing` call logged coordinates.
- Redirects are off (`Policy::none()`) on the TDX request client and the gateway client; confirm on the next data refresh that nothing relied on redirects; the GTFS download client keeps default redirects (carries no coordinates).
- Stop IDs, mode sets, counts and timings may stay in logs.

## Guard rails
- Regression tests capture TRACE output for a journey plan, a rejected plan, nearby stops and a TDX error chain, and assert no request coordinate appears (`tests/api/log_privacy.rs`, `tests/support/log_capture.rs`).
- Not covered by a guard: `Debug` of `GeoCoordinate` and `PlanningInput` still prints coordinates if someone logs them. A redacting Debug or a CI grep is an open idea. The access debug event logs stop IDs and walk seconds, which the policy wording should allow.
- Nearby-stops sends coordinates in the query string; moving them to a POST body is a possible API change, not done.

## Infrastructure bound (at most 7 days for IP and timestamp)
- Traefik access logs are disabled and its level is ERROR, so request paths and queries never reach disk.
- Valhalla must not log request bodies or locations; check its logging config when the tiles config changes.
- Size-based rotation alone does not bound age. Review found that a systemd tmpfiles `e 7d` rule on live pod paths is unsafe (active files keep fresh mtimes, symlinks of long-running pods get deleted). The shipped design: kubelet `containerLogMaxSize 5Mi` / `containerLogMaxFiles 2`, journald `MaxRetentionSec=6day` and `MaxFileSec=1day`, and a daily timer deleting rotated pod logs older than 6 days (`find -mmin +8640`) plus dangling symlinks. A rotated file can wait up to about 7 days. A quiet container's active log is bounded by size only.
- Grafana Cloud free keeps logs 14 days and cannot be shortened, so logs stay on the node; only metrics are shipped.
- Tested on a real k3s VM: kubelet drop-in applied, journald values, timer behaviour and idempotence, Traefik rendered with no access log.

## Before publishing the privacy policy
1. The stripping change is merged and deployed.
2. After the first AWS deploy: send nearby-stops and plan requests with distinctive coordinates, then grep every pod log and journald; the result must be empty (never run during the tests).
3. Reverse proxies and any load balancer omit the query string from logs.
4. Owner full name and contact are in the policy; confirm the "6 days" text against the 7-day wording.
