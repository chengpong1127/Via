#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: VIA_API_HOST=<host> VIA_ACME_EMAIL=<email> $0 <git-sha>" >&2
  exit 2
}

[ $# -eq 1 ] || usage
sha="$1"
if ! [[ "$sha" =~ ^[0-9a-f]{7,40}$ ]]; then
  echo "image tag must be a lowercase hex git sha, got: $sha" >&2
  exit 2
fi

region="${VIA_REGION:-ap-east-2}"
ssm_prefix="${VIA_SSM_PREFIX:-/via/prod}"
export KUBECONFIG="${KUBECONFIG:-/etc/rancher/k3s/k3s.yaml}"

src="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../k8s" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp -R "$src/." "$work/"
overlay="$work/overlays/aws"

if [ -n "${VIA_API_HOST:-}" ]; then
  sed -i "s/api\.example\.com/${VIA_API_HOST}/g" "$overlay"/*.yaml
fi
if [ -n "${VIA_ACME_EMAIL:-}" ]; then
  sed -i "s/ops@example\.com/${VIA_ACME_EMAIL}/g" "$overlay"/*.yaml
fi

(cd "$overlay" && kustomize edit set image "via-server=ghcr.io/chengpong1127/via-server:${sha}")

if rendered="$(kubectl kustomize "$overlay")" &&
  grep -nE 'set-to-git-sha|api\.example\.com|ops@example\.com' <<<"$rendered"; then
  echo "placeholders remain; set VIA_API_HOST and VIA_ACME_EMAIL" >&2
  exit 1
fi

kubectl create namespace via --dry-run=client -o yaml | kubectl apply -f -

client_id="$(aws ssm get-parameter --region "$region" --with-decryption --name "$ssm_prefix/TDX_CLIENT_ID" --query Parameter.Value --output text)"
client_secret="$(aws ssm get-parameter --region "$region" --with-decryption --name "$ssm_prefix/TDX_CLIENT_SECRET" --query Parameter.Value --output text)"
kubectl -n via create secret generic tdx-credentials \
  --from-literal=TDX_CLIENT_ID="$client_id" \
  --from-literal=TDX_CLIENT_SECRET="$client_secret" \
  --dry-run=client -o yaml | kubectl apply -f -

kubectl apply -k "$overlay"
kubectl -n via rollout status deploy/server --timeout=300s
