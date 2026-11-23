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

## 4. Data

Put the static artifact and Valhalla files on the data volume before the first deploy, otherwise the server's startup probe times out: copy a static build to `/var/lib/via/static/<build-id>`, point the relative symlink `current` at it (`ln -sfn <build-id> /var/lib/via/static/current`), and put Valhalla tiles and `valhalla.json` in `/var/lib/via/valhalla`. The artifact fetch job (separate task) automates this; until then use `aws s3 sync` from a Session Manager shell.

## 5. First deploy

    aws ssm start-session --region ap-southeast-2 --target <instance_id>
    sudo -i
    cd /opt/via && git fetch --tags origin && git checkout --detach <sha-or-tag>
    VIA_API_HOST=api.yourdomain.example VIA_ACME_EMAIL=you@example.com \
      server/deploy/aws/bootstrap/deploy.sh <image-git-sha>

`deploy.sh` refuses to apply if the sha is not hex, if the host or email is malformed, or if `api.example.com` / `ops@example.com` / `set-to-git-sha` remain. It builds the overlay in a temp copy, creates the `tdx-credentials` Secret from SSM and applies it. It is safe to re-run.

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

- journald: `/etc/systemd/journald.conf.d/via.conf` sets `MaxRetentionSec=7day` and `MaxFileSec=1day` (retention is enforced per journal file, so files must roll daily).
- kubelet: a `kubelet.conf.d` drop-in under the k3s data directory (`/var/lib/via/k3s/agent/etc/kubelet.conf.d/10-via-logs.conf`; k3s passes this directory to the kubelet as `config-dir`) sets `containerLogMaxSize: 5Mi` and `containerLogMaxFiles: 2`, so each container keeps its active log plus one rotated file.
- cleanup: `via-log-cleanup.timer` runs daily and deletes rotated pod logs (`/var/log/pods/**/*.log.*`) older than 7 days, and dangling symlinks in `/var/log/containers`. Active log files and live symlinks are never touched.

The honest bound: rotated files are gone after 7 days, and an active log file is bounded by size (5Mi), not by age. A very quiet container could therefore hold lines older than 7 days in its active file until it fills or the pod is replaced (every rollout replaces the pods). At the expected volume (access logs off, only method, path and status logged) this is acceptable; if a hard age bound is required, restart the deployments weekly.

Re-running the script on an existing node rewrites these files only when they differ, restarting journald or k3s only then.
