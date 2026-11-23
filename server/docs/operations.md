# Operating Via on AWS

Monitoring is Grafana Alloy (one pod in namespace `via`) pushing metrics to Grafana Cloud free, plus one external uptime check. Logs stay on the node. Manifests: `deploy/k8s/overlays/aws/monitoring`. Deployment steps: [deploy-aws.md](deploy-aws.md).

## Setup

### 1. Grafana Cloud credentials (out of band, never committed)

Create a free Grafana Cloud stack, then in the stack's Prometheus details note the remote write URL and the numeric user, and create an access policy token with only the `metrics:write` scope. Create the Secret before `deploy.sh` (or any time; the Alloy pod waits in `CreateContainerConfigError` until it exists):

    kubectl create namespace via --dry-run=client -o yaml | kubectl apply -f -
    kubectl -n via create secret generic grafana-cloud \
      --from-literal=url='https://prometheus-prod-XX-REGION.grafana.net/api/prom/push' \
      --from-literal=user='123456' \
      --from-literal=token='glc_...'
    kubectl -n via rollout restart deploy/alloy

Rotate the token the same way. Check the pod: `kubectl -n via logs deploy/alloy | grep -i -E 'error|remote_write'` should show no 401/403.

### 2. Alert rules

`deploy/k8s/overlays/aws/monitoring/alerts/via-alerts.rules.yaml` is a standard Prometheus rule file. Load it into the stack's Mimir ruler with an access policy token that has `rules:write`:

    mimirtool rules load --address=https://prometheus-prod-XX-REGION.grafana.net \
      --id=<user> --key=<rules-token> deploy/k8s/overlays/aws/monitoring/alerts/via-alerts.rules.yaml

Then in Grafana, Alerting, Contact points: add an email (or Slack) contact point and route alerts to it with the default notification policy. Reloading the file replaces the `via` group.

### 3. External uptime check

Alloy cannot see an outage of the node itself, so add an outside check on `https://<api-host>/health/ready`: UptimeRobot (free, 5-minute interval, HTTPS keyword monitor expecting HTTP 200, email alert), or a Route 53 health check (HTTPS, path `/health/ready`, string matching optional, failure threshold 3, with a CloudWatch alarm to SNS; a small monthly cost). `/health/ready` is routed through Traefik, so this tests DNS, TLS, ingress and the server together. Check from outside only; do not exempt it from the rate limits (one request per few minutes is far below them).

## What is collected

Series stay under about 1000 active (budget 10000). Everything is allow-listed in `alloy-config.alloy`:

| Source | Kept | Notes |
| --- | --- | --- |
| server pods (2), scraped per pod, 30s | `via_*`, `up` | `job="server"`, `instance=<pod>`. Labels are only `dataset`/`reason`, so no per-request or per-location label exists. |
| tdx-gateway pod, 30s | `via_*`, `up` | `job="tdx-gateway"` |
| readiness probes, 30s | `probe_success`, `probe_duration_seconds` | `job="via-ready"`, `name="server"` or `"tdx-gateway"`, through the Services |
| node (embedded node exporter), 60s | `node_cpu_seconds_total`, `node_load1/5/15`, `node_memory_{MemTotal,MemAvailable,SwapTotal,SwapFree}_bytes`, `node_filesystem_{avail,size}_bytes` (real filesystems only), `via_artifact_fetch_*` | `job="node"`, `instance="via-node"` |

Nothing from Traefik, Valhalla or cert-manager is scraped. The `/metrics` endpoint is never routed by the ingress and is reachable only by Alloy (NetworkPolicy `allow-alloy-to-metrics`). The metrics are location-free by design.

Container logs are NOT shipped. Grafana Cloud free keeps logs for 14 days and the free plan does not let you shorten it (only Support can customize retention), which would break the 7-day privacy bound. Logs are read on the node (below).

### Metric gaps (server code, not changed here)

- There is no end-to-end HTTP request duration or status-code metric. "Plan latency" uses `via_planner_multimodal_search_duration_seconds` (the search phase only; buckets go up to 30s). Request-level latency is in the structured `elapsed_ms` log field only.
- There is no TDX 429 counter for the static or token paths, only live data (`via_live_rate_limit_blocks_total`, `via_bus_timing_rate_limited_requests_total`).
- The artifact fetch metric is written by `via-fetch-metric` (an `ExecStopPost` drop-in on `via-artifact-fetch.service`) to `/var/lib/via/textfile/via_artifact_fetch.prom`. It exists only after the first timer run (or `systemctl start via-artifact-fetch.service`).

## Dashboards and queries to watch

Explore (Grafana Cloud) queries:

- Readiness: `probe_success{job="via-ready"}`
- Plans per minute: `sum(rate(via_planning_requests_total[5m])) * 60`; failures: `sum(rate(via_planning_failures_total[5m])) * 60`; empty results: `sum(rate(via_planning_empty_total[5m])) * 60`
- Search p50/p95: `histogram_quantile(0.95, sum by (le) (rate(via_planner_multimodal_search_duration_seconds_bucket[10m])))`
- TDX 429 pauses: `sum(increase(via_live_rate_limit_blocks_total[1h]))`; bus 429s: `sum(increase(via_bus_timing_rate_limited_requests_total[1h]))`
- TDX upstream calls by dataset: `sum by (dataset) (rate(via_live_upstream_requests_total[5m]))`; gateway latency: `histogram_quantile(0.95, sum by (dataset, le) (rate(via_tdx_gateway_request_seconds_bucket[10m])))`
- Memory available: `node_memory_MemAvailable_bytes / node_memory_MemTotal_bytes`
- Disk free: `node_filesystem_avail_bytes / node_filesystem_size_bytes`
- CPU: `1 - avg(rate(node_cpu_seconds_total{mode="idle"}[5m]))` (planning is CPU-bound; sustained near 1 means capacity)
- Artifact: `via_artifact_fetch_last_success`, `time() - via_artifact_fetch_last_success_timestamp_seconds`

## Alerts: meaning and first response

Every first response starts with `kubectl -n via get pods` and `kubectl -n via describe pod <pod>` (SSM session, see deploy-aws.md section 2).

### ViaReadinessDown
`/health/ready` through the `server` Service failed for 3 minutes: no ready server pod (static artifact missing or invalid, Valhalla not ready, or pods crashing). First: `kubectl -n via get pods`; `kubectl -n via logs deploy/server --tail=100`; check `ls -l /var/lib/via/static/current` and that `deploy/valhalla` is ready. After a bad data load run `fetch.sh --static <previous-id>` (deploy-aws.md section 4). After a bad image, `deploy.sh <previous-sha>`.

### TdxGatewayNotReady
The gateway pod is not ready. Plans still work but without live data. `kubectl -n via logs deploy/tdx-gateway`; check the `tdx-credentials` Secret exists with both keys; `kubectl -n via rollout restart deploy/tdx-gateway`.

### ViaMetricsMissing
Prometheus has no successful `up` for the server or the gateway for 10 minutes: Alloy is down, the credentials are rejected, or the pods are gone. `kubectl -n via logs deploy/alloy`; confirm the `grafana-cloud` Secret and token expiry. If readiness alerts still fire, they come from Alloy too, so also rely on the external uptime check.

### ViaPlanLatencyHigh
p95 search time over 8s for 10 minutes. The request timeout is 10s, so users are about to see failures. Check CPU (`kubectl top pods -n via`, node CPU query): the server is CPU-bound with `max_concurrent_requests=3`. Look for a load spike (plans per minute), a TDX slowdown (gateway latency) or a Valhalla problem. The per-IP limits (30 plans per minute, 10 in flight) are in `overlays/aws/middlewares.yaml`.

### ViaPlanFailuresHigh
More than 20% of plan requests failed over 15 minutes (at least 10 requests). `kubectl -n via logs deploy/server --tail=200 | grep -i error` (logs contain method, path, status and error code, never coordinates). Typical causes: Valhalla down, deadline exceeded under load, bad data build.

### TdxRateLimited
TDX returned 429 and the live cache paused all live calls for `Retry-After` (or `rate_limit_backoff_seconds`); plans continue without live data, using cached entities. See the incident steps below.

### NodeMemoryLow
Under 10% of the 8 GiB available for 10 minutes. The server limit is 2Gi per pod. `kubectl top pods -A`; look for a restarting or leaking pod (`kubectl -n via get pods` restarts, OOMKilled in `describe`). Restart the offender; if it recurs, lower load or move to a larger instance.

### NodeDiskLow
Under 15% free on `/` or `/var/lib/via` for 15 minutes. On the node: `df -h`, `du -xh /var/lib/via --max-depth=2 | sort -h | tail`. Old static builds (last 3 are kept) and tile builds (`/var/lib/via/valhalla-builds`) are the largest safe deletions; `journalctl --disk-usage`; `crictl images` and `k3s crictl rmi --prune` for images.

### ArtifactFetchFailed
The last `via-artifact-fetch.service` run failed (also fires as ArtifactFetchStale after 3 days without a success). `journalctl -u via-artifact-fetch.service -n 100`; look for `.rejected` files in `/var/lib/via/static/builds`; the previous build stays active. Fix the artifact, then publish a new build id or `fetch.sh --static` (deploy-aws.md section 4). Re-run the unit to refresh the metric: `systemctl start via-artifact-fetch.service`.

## TDX 429 incident steps

1. Confirm: `TdxRateLimited` fired, or `kubectl -n via logs deploy/server | grep 'rate limit'` and `kubectl -n via logs deploy/tdx-gateway | grep 'rate limit'`. TDX allows 5 requests per minute for our credentials; the server pauses itself, so users keep getting plans without live data.
2. Check who else is calling: only the single `tdx-gateway` pod should use the credentials (it runs with `Recreate`, one replica). Make sure no one is running `via-data`, a local server or a test with the same TDX client id: any use of the key counts against the same limit.
3. Check the demand: plans per minute and `sum by (dataset) (rate(via_live_upstream_requests_total[5m]))`. A spike means abuse or a campaign; tighten `plan-rate-limit` in `overlays/aws/middlewares.yaml` and redeploy.
4. Wait: the pause ends by itself after `Retry-After`. Do not restart the gateway repeatedly, since the cache and the pause state are in memory and a restart removes both.
5. If it persists for hours, check the TDX developer console (quota, key status) and raise the backoff via `rate_limit_backoff_seconds` ([configuration.md](configuration.md)).

## Logs and privacy

Container logs are read on the node, never exported:

    kubectl -n via logs deploy/server --tail=200
    kubectl -n via logs deploy/server --previous
    kubectl -n via logs deploy/alloy
    journalctl -u k3s -u via-artifact-fetch.service --since "1 hour ago"

The privacy policy keeps infrastructure logs (IP and timestamp only, no coordinates) for at most 7 days. This is enforced on the node: journald `MaxRetentionSec=6day`, kubelet log rotation (5Mi x 2) with a daily cleanup of rotated pod logs older than 6 days (details: deploy-aws.md section 10). Traefik access logs are off, and metrics carry no request data. Do not add a log shipper, enable access logs, or add request-derived labels without reviewing this bound. If a hard age bound on active log files is needed, restart the deployments weekly.
