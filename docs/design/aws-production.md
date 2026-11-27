# AWS production backend

Epic #21 goal G2. Runbook: `server/docs/deploy-aws.md`; monitoring: `server/docs/operations.md`;
code: `infra/aws`, `server/deploy/aws/`, `server/deploy/k8s/overlays/aws`. This note keeps the
reasoning behind them (researcher report 2026-11-23, then owner and review decisions).

## Architecture and why
- One EC2 m7g.large (2 vCPU, 8 GiB, arm64), single-node k3s, about US$80/month before credits. Measured: server pod about 700 MiB, CPU-bound (100% during a plan), `max_concurrent_requests=3`.
- No NAT gateway and no load balancer: Elastic IP, gp3 40 GB root plus a separate encrypted data volume (about US$2/month) holding `/var/lib/via` and the k3s data dir, so a node rebuild keeps data.
- Ingress: k3s Traefik with cert-manager and Let's Encrypt HTTP-01. Only `/v1/*`, `/openapi.json` and `/health/ready` are routed; `/metrics` stays internal. Per-IP rate limit 30 plans/minute plus an in-flight limit.
- Pods: server x2 (maxSurge 1, maxUnavailable 0, memory 1Gi request / 2Gi limit), Valhalla x1, TDX gateway x1 with Recreate and a NetworkPolicy allowing only the server to reach it. Exactly one gateway replica holds TDX credentials, so the shared rate limit is respected.
- Images: public GHCR, built on free `ubuntu-24.04-arm` runners, deployed by git SHA (never a mutable tag; `IfNotPresent` would not pull a moved tag). `via-data` is in the server image so artifact validation can run before activation.
- Secrets: SSM SecureString `/via/prod/*`, written to a Kubernetes Secret at deploy time through a 0600 file or process substitution, never on a command line.
- Artifacts: private versioned S3. The static artifact shrinks from about 540 MB to 19 MB with zstd; Valhalla tiles are about 1 GB (the PBF is not needed). Host-side `fetch.sh` verifies sha256, validates, flips a relative `current` symlink atomically and restarts; a failed validation or readiness check restores the previous build, and a rejected build is retried at most twice. Today static data is built in-cluster by CronJob `nightly-static` (03:00 Asia/Taipei) and handed to the host (`via-static-activate.path`, `fetch.sh --incoming`); the 05:00 S3 timer refreshes tiles only (see the "Nightly refresh" section of `server/docs/deploy-aws.md`). The owner decided weekly instead of nightly (#53, 2026-11-26; OrbStack weekly CronJob #71); not yet implemented. Peak memory of the in-cluster build was never measured (#36): measure it before relying on it; the fallback is a GitHub Actions build plus `publish.sh`. The fetch uses the paced `--gtfs-only` mode (#39).
- Monitoring: Grafana Alloy to Grafana Cloud free (metrics only) plus an external uptime check. Backups: daily EBS snapshots kept 7 days (volumes must carry the `Backup=via` tag) and S3 versioning. IaC: OpenTofu with S3 state. Access: SSM Session Manager, no SSH.
- IMDSv2 required with hop limit 1 (manager decision after review): pods cannot assume the node role, so S3 and SSM access happens only from host processes (`deploy.sh`, `fetch.sh`).
- Single node, no HA (owner decision): an uptime alert and the rebuild runbook replace redundancy.

## Region, credits and cost (researcher, 2026-11-23; unverified, one uncited source; check the console, AWS pages conflict)
- New accounts get about US$100 at sign-up plus up to US$100 for completing 6 activities (US$20 each). The free plan ends after 6 months or when credits run out; the account then closes (90 days to upgrade). Upgrading to a paid plan keeps remaining credits. Credit expiry is stated as 6 or 12 months on different AWS pages.
- The free plan locks the region: Taiwan contacts get Sydney (ap-southeast-2). Taipei (ap-east-2) needs a paid plan with advanced features (irreversible) or the advanced sign-up path. The first design assumed Taipei (about 10% cheaper than Tokyo); the owner chose the free plan in Sydney on 2026-11-23 knowing the credits cover about 2.5 months of the full design. Whether m7g.large and the Elastic IP are allowed on the free plan was unconfirmed: confirm the allowed instance type at apply. A smaller MVP profile is proposed in open PRs #38 and #37 (owner decision pending).
- Domain: Route 53 .com about US$16/year, .app about US$20 (HSTS preloaded), .dev about US$17; Route 53 cannot register .tw. sslip.io or nip.io work for an interim Let's Encrypt host but suit production poorly (bound to the IP, shared certificate quota of 50 per 7 days, Public Suffix List status uncertain, weak for App Store review). The runbook documents the sslip.io interim path.

## Lessons from review and testing (round 1 findings, all fixed before merge)
- Default-deny ingress blocks cert-manager HTTP-01 solver pods (label `acme.cert-manager.io/http01-solver`, port 8089) from Traefik: needs an allow policy or no certificate is issued.
- k3s ServiceLB can hide client IPs, putting everyone in one rate-limit bucket; the overlay sets Traefik `externalTrafficPolicy: Local`, which is correct for single-node only.
- Pins written from memory were wrong or end of life; verify versions and add sha256 for downloaded manifests and binaries. Resolve the repo ref to a SHA and check it out detached so re-runs pick up new code.
- Build the AMI lookup with the Canonical owner ID and name filter, not an SSM parameter; whether ap-east-2 has it was never confirmed.
- Placeholder guards must not fail open (`if rendered=$(...) && grep` hides a render failure); render as its own statement.
- Artifact fetch: match S3 "not found" by exact status (a loose "404" match let publish overwrite), use `--if-none-match '*'` on put, refuse changed content for an existing ID, fail rather than skip without `flock`, and tie the tiles ID to the tree hash.
- Alloy: do not mount host `/`, run non-root with a read-only root filesystem and no capabilities; node disk space comes from a node textfile metric written by a timer. The p95 plan latency histogram records successes only and the search phase only, so the alert needs a traffic guard and is an early warning.
- Server metric gaps (not done): record the search histogram on every exit, an end-to-end HTTP duration and status histogram without location labels, a 429 counter on static and token paths, readiness and build-ID gauges.

## Never verified (do before relying on it)
- `promtool check rules`, and `alloy fmt` / `alloy validate` on the final monitoring config (the Alloy config passed once before the hardening edits). Rules were only parsed as YAML.
- Alloy against a live cluster; the live Traefik pod; a real EC2, EBS and ap-east-2 AMI; the post-deploy pod-log coordinate grep. (`install-k3s.sh` and `deploy.sh` ran end to end on a real k3s VM, not on real EC2. The server image build is exercised by `.github/workflows/server-image.yml`. A `promtool check rules` step is in `server/docs/operations.md`.)
- Traefik route priority of Exact `/v1/journey-plans` versus Prefix `/v1`, and the per-IP rate limit actually applying.
- ACME HTTP-01 passing through the NetworkPolicies.
- `externalTrafficPolicy: Local` preserving the client IP.
- Real AWS S3 behaviour of `put-object --if-none-match '*'` and of a real 403.
- Real `k3s ctr run` image validation.
- The systemd timers and the path unit running on a real node.
- EC2 NVMe `/dev/disk/by-id` naming for the data volume.
- aws-cli installed via snap on the node.
- cloud-init execution itself.
- `tofu plan` and `apply`, DLM snapshots, Route 53 and IAM policies against a real account.
- A real GHCR image pull.
- Linux-only paths of the artifact scripts beyond the tester's Ubuntu run.
- Peak memory of the in-cluster nightly static build (#36).
- Low value: T10 load and capacity test; the kustomize pin in `deploy-checks.yml` (inside open PR #38); a visual check of the 預估班次 label in the app.
- Open nits: `jq` stream check in artifact validation accepts an empty file; tar excludes for `.tiles-id` and `.via-verified`; `deploy.sh` uses GNU `sed -i` (fine on the node); the NetworkPolicy ports could be narrowed per pod; HelmChartConfig could move into the bootstrap track.
- Pre-existing: the OrbStack overlay did not render in a fresh checkout without an untracked `volumes.yaml`.
