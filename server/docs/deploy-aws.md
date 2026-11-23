# Deploying Via on AWS k3s (draft)

One m7g.large (arm64) in ap-east-2 runs k3s with Traefik, cert-manager, the Via server (x2), Valhalla and the TDX gateway. Infrastructure is in `infra/aws` (OpenTofu); node bootstrap is in `server/deploy/aws/bootstrap`; manifests are in `server/deploy/k8s/overlays/aws` (see its README).

## 1. Owner prerequisites

- An AWS account with billing alerts and credits applied.
- Opt in to the ap-east-2 (Taipei) region in the account settings.
- An admin IAM role or user for running OpenTofu locally. The node itself never needs it.
- A TDX application (client id and secret).
- Optional: a domain. Without one, see the no-domain interim below.
- The GHCR package `via-server` made public after CI pushes the first image.

## 2. Create the infrastructure

    cd infra/aws
    cp backend.example.hcl backend.hcl        # your state bucket
    cp terraform.tfvars.example terraform.tfvars
    tofu init -backend-config=backend.hcl
    tofu plan
    tofu apply

The state bucket (versioned, private) is created by hand beforehand; OpenTofu uses an S3 lockfile, so no DynamoDB table is needed. Set `repo_ref` to a branch or tag containing `server/deploy/aws/bootstrap`. Outputs: `public_ip`, `instance_id`, `artifact_bucket`. The node bootstraps itself through cloud-init (log: `/var/log/via-bootstrap.log`).

## 3. Secrets

Values never go through OpenTofu. Create them once:

    aws ssm put-parameter --region ap-east-2 --type SecureString --name /via/prod/TDX_CLIENT_ID --value '...'
    aws ssm put-parameter --region ap-east-2 --type SecureString --name /via/prod/TDX_CLIENT_SECRET --value '...'

## 4. Data

Copy a static build to `/var/lib/via/static/<build-id>`, point the relative symlink `current` at it, and put Valhalla tiles and `valhalla.json` in `/var/lib/via/valhalla`. The artifact fetch job (separate task) automates this; until then use S3 and `aws s3 sync` from a Session Manager shell.

## 5. First deploy

    aws ssm start-session --target <instance_id>
    sudo -i
    cd /opt/via && git pull
    VIA_API_HOST=api.yourdomain.example VIA_ACME_EMAIL=you@example.com \
      server/deploy/aws/bootstrap/deploy.sh <git-sha>

`deploy.sh` refuses to apply if the sha is not a hex commit or if `api.example.com` / `ops@example.com` remain. It builds the overlay in a temp copy, creates the `tdx-credentials` Secret from SSM and applies it. It is safe to re-run.

## 6. TLS check

Create a DNS A record to `public_ip` (automatic when `route53_zone_id` is set), then:

    kubectl get certificate -n via
    curl -i https://<host>/health/ready

No domain yet: use `<ip-with-dashes>.sslip.io` (for example `203-0-113-10.sslip.io`) as `VIA_API_HOST`; Let's Encrypt issues for it, with its rate limits. Replace it once the domain exists.

## 7. Deploy and rollback

Deploy: wait for the CI image workflow to publish `<sha>`, then re-run `deploy.sh <sha>`. Rollback: run `deploy.sh <previous-sha>`. Check `kubectl -n via rollout status deploy/server`.

## 8. Node rebuild

State lives on the EBS volume (daily snapshots, 7 kept) and in S3. To rebuild: `tofu apply -replace=aws_instance.node`, then restore data from S3 or a snapshot, redo section 5. The Elastic IP and DNS stay.
