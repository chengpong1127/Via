# AWS k3s overlay

Production overlay for a single-node k3s host (bundled Traefik). It reuses `../../base` unchanged.

    kubectl apply -k server/deploy/k8s/overlays/aws

## Prerequisites

- cert-manager installed (the `ClusterIssuer` is `letsencrypt-prod`, HTTP-01 through the `traefik` ingress class).
- k3s Traefik with its CRDs (`traefik.io/v1alpha1` `Middleware`) and a NetworkPolicy controller (k3s ships kube-router's).
- Secret `tdx-credentials` in namespace `via` with keys `TDX_CLIENT_ID` and `TDX_CLIENT_SECRET`, created at deploy time from SSM SecureString `/via/prod/*`. It is not in this overlay.
- Ports 80 and 443 reachable on the node, and the API DNS name pointing at it (needed for the HTTP-01 challenge).

## Volumes (hostPath on the node)

Create the directories before applying (`mkdir -p /var/lib/via/static /var/lib/via/valhalla`); the hostPath type is `Directory`, so pods stay pending otherwise.

| Path on node | Mounted at | Contents |
| --- | --- | --- |
| `/var/lib/via/static` | `/data/static` (server, read-only) | build directories plus a `current` symlink |
| `/var/lib/via/valhalla` | `/custom_files` (valhalla, read-only) | `valhalla.json` and tiles |

`VIA_STATIC` is `/data/static/current/.via-static.json`. Make `current` a relative symlink to the build directory (for example `ln -sfn build-20261123 /var/lib/via/static/current`), so it resolves inside the container; then `kubectl -n via rollout restart deploy/server`.

## Settings to change

- Host and email: replace `api.example.com` in the `patches` entry of `kustomization.yaml` (one place, both Ingresses and their TLS blocks), and `ops@example.com` in `cluster-issuer.yaml`.
- Image tag: always deploy an immutable commit sha (or `@sha256:` digest from the workflow summary): `cd server/deploy/k8s/overlays/aws && kustomize edit set image via-server=ghcr.io/chengpong1127/via-server:<git-sha>`. `newTag` ships as the placeholder `set-to-git-sha` and must be replaced. Never deploy `:main`: a mutable tag is not re-pulled on rollout restart. Rollback is applying an earlier sha. The GHCR package must be public (set it once in the package settings after the first push), otherwise add an `imagePullSecrets` entry. Change `newName` if the owner differs (lowercase).
- The host patch in `kustomization.yaml` addresses `/spec/rules/0` and `/spec/tls/0` by index; keep those positions if you edit `ingress.yaml`.
- Rate limits: `middlewares.yaml`. `plan-rate-limit` (30 requests per minute per client IP, burst 10) applies only to `/v1/journey-plans`; `in-flight-limit` (10 concurrent per IP) applies to all routes.

## Exposure

There is no HTTP to HTTPS redirect: plain HTTP is only used for the ACME challenge, and clients must use https. `traefik-config.yaml` sets the k3s Traefik Service to `externalTrafficPolicy: Local` so the real client IP reaches the rate limiter (otherwise ServiceLB SNAT would put all clients in one bucket). Verify with a request and the Traefik access logs after the first deploy. k3s may re-apply the HelmChartConfig by restarting Traefik.

The `allow-traefik-to-acme-solver` policy lets Traefik reach cert-manager's HTTP-01 solver pods (port 8089) despite the default deny.

Only `/v1/*`, `/openapi.json` and `/health/ready` are routed; `/metrics` is never exposed. NetworkPolicies deny all ingress in `via` except Traefik (kube-system) to server:3000, server to tdx-gateway:3100 and server to valhalla:8002.

The `memory` limit is 2Gi on the server (request 1Gi); tdx-gateway stays at one replica with `Recreate` so only one process talks to TDX.

## Privacy and log retention

The privacy policy allows infrastructure logs to keep only the client IP address and a timestamp, for 7 days, with no coordinates. `/v1/nearby-stops` carries latitude and longitude in the URL query string, so:

- Traefik access logs are explicitly disabled in `traefik-config.yaml` (`logs.access.enabled: false`). They would record the request path and query. Do not enable them without dropping the path, query and header fields. Re-enabling them with only the client host and timestamp would need `RequestPath`, `RequestAddr`, `RequestLine` and similar fields dropped and `fields.headers.defaultmode: drop`, and must be reviewed against the policy first.
- The Via server logs the method and path only, never the query string or the request body.
- Valhalla logs only the request line and status (`POST /route HTTP/1.1`, `400::No suitable edges near location`), never locations. This was verified against valhalla 3.9.0 for successful and failing `/route` and `/sources_to_targets` calls. It holds because the server sends coordinates in POST bodies; Valhalla would log a `GET ...?json=` query, so never call it that way. Valhalla 3.9.0 has no log-level setting to tighten further. This check is tied to the image tag in the base (`ghcr.io/valhalla/valhalla-scripted:3.9.0`): redo it (POST a route, a failing route and a matrix call, then read the container log for coordinates) whenever that tag changes.
- Retention (7 days) is enforced on the node by the bootstrap, see `server/docs/deploy-aws.md` section 10: journald `MaxRetentionSec=6day` with `MaxFileSec=1day`, kubelet container logs capped at 5Mi x 2 files, and a daily job that deletes rotated pod logs older than 6 days (a file lives at most about 7 days). Size rotation alone does not bound age, so the active log file of a very quiet container can hold older lines until it fills or the pod is replaced (every rollout).
