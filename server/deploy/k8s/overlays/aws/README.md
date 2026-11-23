# AWS k3s overlay

Production overlay for a single-node k3s host (bundled Traefik). It reuses `../../base` unchanged.

    kubectl apply -k server/deploy/k8s/overlays/aws

## Prerequisites

- cert-manager installed (the `ClusterIssuer` is `letsencrypt-prod`, HTTP-01 through the `traefik` ingress class).
- k3s Traefik with its CRDs (`traefik.io/v1alpha1` `Middleware`) and a NetworkPolicy controller (k3s ships kube-router's).
- Secret `tdx-credentials` in namespace `via` with keys `TDX_CLIENT_ID` and `TDX_CLIENT_SECRET`, created at deploy time from SSM SecureString `/via/prod/*`. It is not in this overlay.
- Ports 80 and 443 reachable on the node, and the API DNS name pointing at it (needed for the HTTP-01 challenge).

## Volumes (hostPath on the node)

| Path on node | Mounted at | Contents |
| --- | --- | --- |
| `/var/lib/via/static` | `/data/static` (server, read-only) | build directories plus a `current` symlink |
| `/var/lib/via/valhalla` | `/custom_files` (valhalla, read-only) | `valhalla.json` and tiles |

`VIA_STATIC` is `/data/static/current/.via-static.json`. Make `current` a relative symlink to the build directory (for example `ln -sfn build-20261123 /var/lib/via/static/current`), so it resolves inside the container; then `kubectl -n via rollout restart deploy/server`.

## Settings to change

- Host and email: replace `api.example.com` in the `patches` entry of `kustomization.yaml` (one place, both Ingresses and their TLS blocks), and `ops@example.com` in `cluster-issuer.yaml`.
- Image tag: `newTag` under `images` in `kustomization.yaml`, or `kustomize edit set image via-server=ghcr.io/chengpong1127/via-server:<git-sha>`. Default is `main`; deploy a commit sha for rollbacks. Change `newName` if the repository owner differs.
- Rate limits: `middlewares.yaml`. `plan-rate-limit` (30 requests per minute per client IP, burst 10) applies only to `/v1/journey-plans`; `in-flight-limit` (10 concurrent per IP) applies to all routes.

## Exposure

Only `/v1/*`, `/openapi.json` and `/health/ready` are routed; `/metrics` is never exposed. NetworkPolicies deny all ingress in `via` except Traefik (kube-system) to server:3000, server to tdx-gateway:3100 and server to valhalla:8002.

The `memory` limit is 2Gi on the server (request 1Gi); tdx-gateway stays at one replica with `Recreate` so only one process talks to TDX.
