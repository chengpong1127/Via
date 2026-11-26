# Deploying Via on AWS k3s (draft)

One m7g.large (arm64) in ap-southeast-2 (Sydney) runs k3s with Traefik, cert-manager, the Via server (x2), Valhalla and the TDX gateway. Infrastructure is in `infra/aws` (OpenTofu); node bootstrap is in `server/deploy/aws/bootstrap`; manifests are in `server/deploy/k8s/overlays/aws` (see its README).

Design points worth knowing:

- Persistent state lives on a separate encrypted gp3 data volume mounted at `/var/lib/via` (static builds, Valhalla tiles and the k3s state under `/var/lib/via/k3s`). The root volume is disposable.
- IMDSv2 is required with hop limit 1: pods cannot reach the node's IAM role (SSM secrets, S3). Only host processes such as `deploy.sh` can.
- Outbound traffic is open on purpose: the node must reach the TDX API, GHCR, Let's Encrypt, S3, SSM and package mirrors. Only 80 and 443 are open inbound; there is no SSH (use Session Manager).

## 1. Owner prerequisites

- An AWS free-plan account. Its region is locked to the one AWS assigns at sign-up (Sydney, ap-southeast-2, for Taiwan contacts), so no region opt-in is needed and nothing else can be chosen.
- The free plan ends after 6 months or when the credits run out, whichever comes first, and the account is then closed unless upgraded to a paid plan. Check the credit balance and the expiry date in the Billing console (Credits and Free Tier pages) before relying on it, set a budget alert, and plan the upgrade or migration in time to back up the data volume snapshots and the S3 bucket first.
- Check that the free plan allows `m7g.large` and the other resources here (EC2 instance types are restricted on free-plan accounts); if `tofu apply` is refused, pick an allowed type through `instance_type`.
- An admin IAM role or user for running OpenTofu locally. The node itself never needs it.
- The AWS CLI and the Session Manager plugin installed locally.
- A TDX application (client id and secret).
- Optional: a domain. Without one, see the no-domain interim below.
- The GHCR package `via-server` made public after CI pushes the first image.

## 2. Create the infrastructure

Create the state bucket once, by hand (versioned, encrypted, private):

    aws s3api create-bucket --bucket YOUR-TFSTATE-BUCKET --region ap-southeast-2 \
      --create-bucket-configuration LocationConstraint=ap-southeast-2
    aws s3api put-bucket-versioning --bucket YOUR-TFSTATE-BUCKET --versioning-configuration Status=Enabled
    aws s3api put-public-access-block --bucket YOUR-TFSTATE-BUCKET \
      --public-access-block-configuration BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true
    aws s3api put-bucket-encryption --bucket YOUR-TFSTATE-BUCKET \
      --server-side-encryption-configuration '{"Rules":[{"ApplyServerSideEncryptionByDefault":{"SSEAlgorithm":"AES256"}}]}'

Then:

    cd infra/aws
    cp backend.example.hcl backend.hcl
    cp terraform.tfvars.example terraform.tfvars
    tofu init -backend-config=backend.hcl
    tofu plan
    tofu apply

Set `repo_ref` in `terraform.tfvars` to a commit sha or tag that contains `server/deploy/aws/bootstrap`. cloud-init runs only on first boot, so changing `repo_ref` or the scripts later does not re-run it on an existing node; to re-run, execute `/usr/local/sbin/via-bootstrap` on the node. The state uses an S3 lockfile; no DynamoDB table is needed. Outputs: `public_ip`, `instance_id`, `artifact_bucket`, `data_volume_id`.

Wait for the bootstrap to finish before anything else:

    aws ssm start-session --region ap-southeast-2 --target <instance_id>
    sudo cloud-init status --wait
    sudo tail /var/log/via-bootstrap.log     # ends with BOOTSTRAP-OK

`BOOTSTRAP-FAILED` means check the log above it and re-run `/usr/local/sbin/via-bootstrap`.

## 3. Secrets

Values never go through OpenTofu. Create them once:

    aws ssm put-parameter --region ap-southeast-2 --type SecureString --name /via/prod/TDX_CLIENT_ID --value '...'
    aws ssm put-parameter --region ap-southeast-2 --type SecureString --name /via/prod/TDX_CLIENT_SECRET --value '...'

## 4. Data: publish and load

Artifacts travel through the private artifact bucket (`artifact_bucket` output). Nothing in the cluster holds AWS credentials: `fetch.sh` runs on the node host as root and uses the instance profile (IMDS hop limit 1 keeps pods out).

**Order matters when the artifact format gains fields** (for example Bus headway estimates, `headway_seconds_by_period` on Bus bindings): older servers reject unknown fields, so roll out the new server image first, then publish and activate the new static artifact. The nightly job builds with the image it runs, so the new image must be deployed before its next build is activated.

Publish from a machine with AWS credentials (owner or CI), with `VIA_ARTIFACT_BUCKET` set:

    server/deploy/aws/artifacts/publish.sh server/data/build/<build-id> \
      --tiles server/var/valhalla --tiles-id <tiles-id>

The static file is zstd-compressed with its sha256 and uploaded to `static/<build-id>/`; tiles (without `*.osm.pbf`) go to `valhalla/<tiles-id>/`, where the id defaults to a content hash. Manifests are created with a conditional put, so two publishers cannot silently overwrite an id; re-publishing identical content is a no-op, different content under an existing id is refused. Pointers are written last, tiles first and `static/latest` at the very end.

On the node (bootstrap installed zstd and jq and enabled a daily tiles timer, `via-artifact-fetch.timer`, 05:00 Asia/Taipei), load tiles (and, for a first manual static load, `fetch.sh --static`):

    sudo /opt/via/server/deploy/aws/artifacts/fetch.sh

For each of static and tiles (processed independently; the exit code is non-zero if either failed) it downloads the pointer target, verifies sha256 and the zstd stream, unpacks to `/var/lib/via/static/builds/<id>`, checks the JSON, runs `via-data validate` on the staged build, atomically switches the relative `current` symlink, then restarts and waits (up to 15 minutes) for `deploy/server`. Tiles are unpacked into `/var/lib/via/valhalla` (previous tiles move to `/var/lib/via/valhalla-builds`; the swap is two renames, a window of milliseconds in which a pod restart would briefly fail to mount) and `deploy/valhalla` is restarted. The last 3 builds are kept.

Validation runs `via-data` from the server image (`k3s ctr run --rm` with the staged build mounted read-only), using the image of the deployed `deploy/server`. Before the first deploy there is no deployment, so give the image explicitly, for example `VIA_SERVER_IMAGE=ghcr.io/chengpong1127/via-server:<sha> sudo -E fetch.sh`, or run the first deploy first (pods wait for data). Validation that cannot run is an error; `VIA_ALLOW_UNVALIDATED=1` overrides it and should be used only knowingly. If the deployments do not exist yet the restart is skipped.

If the server is not ready after the restart, the previous `current` is restored (or removed, on a first load) and the build is recorded in `builds/<id>.rejected` with an attempt count. The run exits non-zero while the latest pointer is rejected; after `VIA_MAX_ATTEMPTS` (default 2) it stops retrying until you remove the `.rejected` file or publish a new id.

Refresh: static data is rebuilt nightly (below). For manually published builds run `fetch.sh --static` on the node; tiles are refreshed by the 05:00 timer or `fetch.sh --tiles`.

Data rollback: `sudo fetch.sh --static <previous-id>` (kept builds are re-activated without re-downloading) and `sudo fetch.sh --tiles <previous-tiles-id>`; to point `latest` back for the timer, re-run `publish.sh` on the older build directory (identical content only rewrites the pointer).

### Nightly refresh (no manual step)

A Kubernetes CronJob, `nightly-static` (aws overlay), runs at 03:00 Asia/Taipei:

1. `via-data fetch --gtfs-only` (one paced TDX call, see `tdx.fetch.requests_per_minute`, default 4 per minute; the very first run has no snapshot and does a full paced fetch, a few minutes). Snapshots live in `/var/lib/via/pipeline` (mounted at `/build/server/data`).
2. If the new GTFS sha256 equals the one of the last successful build, the new snapshot is dropped and the job ends. Nothing is built, activated or restarted.
3. Otherwise `via-data build` (against the in-cluster `valhalla` service), then the artifact is copied to `/var/lib/via/static/incoming/<id>/` and `incoming/READY` is written.
4. On the host, `via-static-activate.path` sees `READY` and runs `fetch.sh --incoming`: the same validate (`via-data validate` from the server image), atomic `current` symlink switch, restart, readiness wait and automatic rollback as section 4. `READY` is consumed on success and on failure, so a rejected build is not retried in a loop; the next night builds again only if the GTFS changed or the build had not succeeded.

No AWS credentials are involved. The pod needs the `tdx-credentials` Secret (the same one the gateway uses) and write access to two host directories, which `install-k3s.sh` creates owned by uid 10001. On a node bootstrapped before this change run once: `sudo install -d -o 10001 -g 10001 /var/lib/via/pipeline /var/lib/via/static/incoming`, install the two `via-static-activate.*` units from `server/deploy/aws/artifacts/` and `systemctl enable --now via-static-activate.path`.

TDX quota: the job spends at most 4 requests per minute and the gateway shares the 5 per minute key, so the 03:00 slot (no traffic) is deliberate. Do not run live TDX experiments against the production key around that time.

Do not merge PR #19 before #27: the job depends on `via-data fetch --gtfs-only` and the paced fetch from #27 (PR in progress). The job requires the image to contain `via-data fetch --gtfs-only` and the paced fetch (#27, PR #39; exit 0 means a complete snapshot, 1 means nothing was published, config under `[tdx.fetch]`). Until the image has it the job fails and the previous data stays active.

Resources (PROVISIONAL): **not measured to completion**. Two local `via-data build` runs on the full Taiwan snapshot (about 540 MB static artifact) were aborted by `StreetRouting(Timeout)` after about 45 to 50 minutes on a loaded laptop with OrbStack Valhalla; they had reached a resident set of 1.5 GiB (peak footprint 2.0 GiB) and were still in the Valhalla matrix phase, so the true peak is at least 2 GiB. The job's `requests` 2Gi and `limits` 3Gi are provisional. Do not size a node (in particular 4 GiB) from them. The build is Valhalla-bound, so wall time depends on Valhalla throughput on the node, not on the job. To change the cadence, patch `spec.schedule` of the CronJob in the overlay; to change memory, patch the container resources.

Memory context: the aws overlay runs 2 server replicas (requests 1Gi, limit 2Gi each) and Valhalla has no memory limit; the MVP overlay (#38) sets 1 replica. Whether the build fits a given node is an open owner decision.

Manual runs: `kubectl create job --from=cronjob/nightly-static` ignores `concurrencyPolicy: Forbid` and spends the same 5 per minute TDX key as the gateway, so run it only in a quiet window. The job refuses to start while another nightly job holds `/var/lib/via/pipeline/.lock`, but it cannot see gateway traffic.

Failure handling: `READY` is consumed (moved to `READY.taken`, then deleted) before the activation lock, cluster and tool checks, so the path unit never loops. `fetch.sh --incoming` waits up to `VIA_LOCK_WAIT` seconds (default 7200) for the lock, so a 05:00 tiles run only delays it (the unit allows 12600 s, more than the lock wait plus the cluster wait and rollout). The "already built" checksum is written (`/var/lib/via/pipeline/.built-gtfs-sha`) only after a successful activation; a lost or rejected hand-over is rebuilt the next night. Disk budget: each fetch snapshot is about 0.5 GB and each build about 0.55 GB; the job keeps 2 snapshots and 1 build (pruned on every exit, failures included), the host keeps 3 activated builds, so plan for roughly 5 GB.

Operate: `kubectl -n via get cronjob,job -l app=nightly-static`, logs `kubectl -n via logs job/<name>`, activation log `journalctl -u via-static-activate`. Run it now: `kubectl -n via create job --from=cronjob/nightly-static manual-$(date +%s)`. Fallback if the node cannot afford the build: build in GitHub Actions and publish with `publish.sh` (section 4).

The 05:00 `via-artifact-fetch.timer` now refreshes **tiles only** (`fetch.sh --tiles`); static data comes from the nightly job. Do not publish static builds to S3 expecting the timer to activate them; use `fetch.sh --static` manually for that.

## 5. First deploy

    aws ssm start-session --region ap-southeast-2 --target <instance_id>
    sudo -i
    cd /opt/via && git fetch --tags origin && git checkout --detach <sha-or-tag>
    VIA_API_HOST=api.yourdomain.example VIA_ACME_EMAIL=you@example.com \
      server/deploy/aws/bootstrap/deploy.sh <image-git-sha>

`deploy.sh` refuses to apply if the sha is not hex, if the host or email is malformed, or if `api.example.com` / `ops@example.com` / `set-to-git-sha` remain. It builds the overlay in a temp copy, creates the `tdx-credentials` Secret from SSM and applies it. It is safe to re-run.

## 5b. Monitoring

Create the `grafana-cloud` Secret, load the alert rules and add the external uptime check as described in [operations.md](operations.md) (the Alloy pod is part of the overlay and waits for the Secret).

## 6. TLS check

Create a DNS A record to `public_ip` (automatic when `route53_zone_id` is set), then:

    kubectl get certificate -n via
    curl -i https://<host>/health/ready

No domain yet: use `<ip-with-dashes>.sslip.io` (for example `203-0-113-10.sslip.io`) as `VIA_API_HOST`. Let's Encrypt applies its certificate limits per registered domain, and `sslip.io` is shared by everyone using it, so issuance can be rate limited by other users; use it only for short interim testing and move to your own domain as soon as possible.

## 7. Deploy and rollback

Deploy: wait for the CI image workflow to publish `<sha>`, then re-run `deploy.sh <sha>`. Rollback: run `deploy.sh <previous-sha>`. Check `kubectl -n via rollout status deploy/server`.

## 8. Node rebuild

The data volume (`aws_ebs_volume.data`, `prevent_destroy`) and the Elastic IP survive replacement; the instance does not hold state.

1. `tofu apply -replace=aws_instance.node`. The volume is detached from the old instance and attached to the new one; cloud-init mounts it without reformatting and k3s resumes from `/var/lib/via/k3s`.
2. Wait for `cloud-init status --wait` and `BOOTSTRAP-OK`, then re-run `deploy.sh <sha>`.

If the data volume itself is lost or corrupt: find the latest snapshot (`aws ec2 describe-snapshots --owner-ids self --filters Name=tag:Name,Values=via-data`), create a volume from it in the same availability zone, remove the broken one from state (`tofu state rm aws_volume_attachment.data aws_ebs_volume.data`), import the new one (`tofu import aws_ebs_volume.data <vol-id>`), and apply. If there is no snapshot, recreate the volume empty and restore the data from S3 (section 4); cert-manager will reissue the certificate.

## 9. Destroying the stack

The data volume and the artifact bucket carry `prevent_destroy`, so `tofu destroy` fails on them by design. To tear down on purpose, take a final snapshot or sync the bucket first, remove the `lifecycle { prevent_destroy = true }` block from `aws_ebs_volume.data` (compute.tf) and `aws_s3_bucket.artifacts` (storage.tf), run `tofu apply`, and then `tofu destroy`. The bucket must also be emptied, including noncurrent versions, before it can be deleted.

## 10. Log retention

Infrastructure logs must not outlive 7 days (the privacy policy keeps only IP and timestamp for 7 days; Traefik access logs are off and Valhalla and the server log no locations, see the overlay README). `install-k3s.sh` enforces this idempotently:

- journald: `/etc/systemd/journald.conf.d/via.conf` sets `MaxRetentionSec=6day` and `MaxFileSec=1day` (retention is enforced per journal file, so files must roll daily; 6 days plus up to a day of file age keeps everything within 7).
- kubelet: a `kubelet.conf.d` drop-in under the k3s data directory (`/var/lib/via/k3s/agent/etc/kubelet.conf.d/10-via-logs.conf`; k3s passes this directory to the kubelet as `config-dir`) sets `containerLogMaxSize: 5Mi` and `containerLogMaxFiles: 2`, so each container keeps its active log plus one rotated file.
- cleanup: `via-log-cleanup.timer` runs daily and deletes rotated pod logs (`/var/log/pods/**/*.log.*`) older than 6 days (`-mmin +8640`; with the daily run a file lives at most about 7 days), and dangling symlinks in `/var/log/containers`. Active log files and live symlinks are never touched.

The honest bound: rotated files are gone within 7 days, and an active log file is bounded by size (5Mi), not by age. A very quiet container could therefore hold lines older than 7 days in its active file until it fills or the pod is replaced (every rollout replaces the pods). At the expected volume (access logs off, only method, path and status logged) this is acceptable; if a hard age bound is required, restart the deployments weekly.

Re-running the script on an existing node rewrites these files only when they differ, restarting journald or k3s only then.

Day-to-day operation, alerts and incident steps: [operations.md](operations.md).

## 11. MVP profile (owner-only testing, minimum cost)

For the first weeks, with about US$100 of credits and no public users, run the same stack on one smaller node. Profile files: `infra/aws/mvp.tfvars.example` (instance and volume sizes) and `server/deploy/k8s/overlays/aws-mvp` (one server replica, memory limits for 8 GiB; `check.sh` there renders it and asserts the sizing, run by `.github/workflows/deploy-checks.yml` together with `bootstrap/test-profile.sh`). `deploy.sh` applies the overlay when `VIA_PROFILE=mvp` is set. The default profile (m7g.large, 40+40 GB, 2 replicas) stays the production design.

### Free-plan instance types (verify first)

AWS documents that free-plan accounts can only launch a short list of EC2 types (at the time of writing t3.micro, t3.small, t4g.micro, t4g.small, c7i-flex.large, m7i-flex.large). This was not verifiable without the account, so treat it as likely: if `tofu apply` refuses `t4g.large`, the cheapest allowed type that has 8 GiB is `m7i-flex.large` (x86_64). It needs an amd64 AMI and an amd64 server image (CI builds arm64 only today), so the alternative is to upgrade the account to a paid plan (credits carry over) and keep Graviton. t4g.small (2 GiB) is allowed but too small: Valhalla, the server and k3s alone use about 3 GiB.

### Which instance fits

Memory limits in the overlay: server 2 Gi, Valhalla 1.5 Gi, nightly build 2 Gi (provisional; the in-node build memory from #36 was not yet published), gateway 128 Mi, Alloy (monitoring) 256 Mi, plus about 1 Gi for k3s, Traefik and cert-manager. Server, Valhalla, gateway and k3s limits sum to about 4.75 Gi (actual use is usually lower). During the nightly build the sum is about 6.75 Gi of 8 GiB, leaving only about 1.25 Gi of headroom; this is why the build limit must be confirmed by the #36 measurement. On t4g.medium (4 GiB) the build cannot run next to the other pods. Choose:

- **t4g.large (8 GiB), recommended.** Everything fits including the nightly build.
- t4g.medium (4 GiB), only with the nightly build moved off the node (GitHub Actions build, then `publish.sh`; the owner decides, see "Open decision"). Saves about US$30 per month. On this node suspend the CronJob, which is otherwise OOM-killed nightly: `kubectl -n via patch cronjob nightly-static -p '{"spec":{"suspend":true}}'` (re-run after each `deploy.sh`, which re-applies it unsuspended).

### Cost per month (ap-southeast-2, on-demand, approximate; confirm in the AWS Pricing Calculator)

| Item | t4g.large | t4g.medium + CI build | default m7g.large |
|---|---|---|---|
| Compute | 59 | 29 | 72 |
| EBS gp3 (20+20 GB MVP, 40+40 GB default) | 4 | 4 | 8 |
| Public IPv4 (the Elastic IP; the instance has no second public IP) | 4 | 4 | 4 |
| Snapshots (2 daily copies, incremental) | 1 | 1 | 2 |
| S3 (bucket under 1 GB, a few requests) | 0 | 0 | 0 |
| Data transfer out (owner testing) | 0 | 0 | 0 |
| **Total US$ / month** | **about 68** | **about 38** | **about 86** |
| **US$100 lasts** | **about 1.5 months** | **about 2.6 months** | **about 1.2 months** |

Compute is the lever. Stopping the instance when not testing (`aws ec2 stop-instances`) removes compute, leaving about US$9 per month for EBS, the idle Elastic IP and snapshots; k3s and the data survive a stop. Starting again needs no redeploy.

Caveats: the table is approximate (about 5 percent either way, not verified against the calculator; compute may be US$2-3 lower per month). t4g instances are burstable: with 2 vCPU the baseline is 30 percent each, and the nightly build or a burst of planning requests can exhaust CPU credits, after which the node is throttled (unlimited mode, the default for t4g, instead bills the surplus at about US$0.05 per vCPU-hour). Watch `CPUCreditBalance` during the first weeks.

### Nothing else to pay for

No NAT gateway (public subnet, security group only allows 80 and 443), no load balancer (Traefik on the node), no Route 53 (DNS is at the registrar: one A record to the Elastic IP), no CloudWatch agent, alarms or log groups (Alloy sends metrics to Grafana Cloud's free tier), no extra snapshots beyond the lifecycle policy. Data transfer in is free. Zero request-coordinate logging and the 7-day log bound (section 10) are unchanged in this profile.

### Open decision

If the nightly in-node build is judged too heavy for the node you choose, the fallbacks are weekly in-node builds or a GitHub Actions build with `publish.sh`. This note does not choose; report the measured peak from #36 and decide.

### First deploy, cheapest path

1. Sign in to the AWS console, confirm credits and set a US$50 budget alert. Try the MVP instance in the console launch wizard first, or rely on `tofu plan` failing safely, to learn which types the free plan allows.
2. `cp infra/aws/mvp.tfvars.example infra/aws/mvp.tfvars`; edit `terraform.tfvars` (bucket name, `repo_ref`); `tofu plan -var-file=terraform.tfvars -var-file=mvp.tfvars`, then `apply` (sections 2 and 3).
3. Put the TDX credentials into SSM (section 3). Open an SSM session, wait for `BOOTSTRAP-OK`.
4. Deploy with `VIA_PROFILE=mvp VIA_API_HOST=tilo.chengpeng.dev VIA_ACME_EMAIL=<owner email> deploy.sh <image-sha>` (section 5). Without DNS use `<ip-with-dashes>.sslip.io` for the first test.
5. Load data: the first static build is made by running the nightly job once (`kubectl -n via create job --from=cronjob/nightly-static first-run`), then `sudo fetch.sh --tiles` after publishing tiles (section 4). Point the A record at the Elastic IP and check `https://<host>/health/ready`.
6. When not testing, stop the instance. To scale up later, change `instance_type` and apply without the mvp var-file.
