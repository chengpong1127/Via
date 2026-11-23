#!/usr/bin/env bash
set -euo pipefail

K3S_VERSION="${K3S_VERSION:-v1.31.4+k3s1}"
CERT_MANAGER_VERSION="${CERT_MANAGER_VERSION:-v1.16.2}"
KUSTOMIZE_VERSION="${KUSTOMIZE_VERSION:-v5.5.0}"
KUBECONFIG="${KUBECONFIG:-/etc/rancher/k3s/k3s.yaml}"
export KUBECONFIG

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root" >&2
  exit 1
fi

mkdir -p /var/lib/via/static /var/lib/via/valhalla

if ! command -v aws >/dev/null 2>&1; then
  snap install aws-cli --classic
fi

if ! command -v k3s >/dev/null 2>&1; then
  curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION="$K3S_VERSION" sh -s - --write-kubeconfig-mode 644
fi

until kubectl get nodes 2>/dev/null | grep -q " Ready"; do
  sleep 5
done

if ! command -v kustomize >/dev/null 2>&1; then
  tmp="$(mktemp -d)"
  curl -sfL "https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize%2F${KUSTOMIZE_VERSION}/kustomize_${KUSTOMIZE_VERSION}_linux_arm64.tar.gz" \
    | tar -xz -C "$tmp"
  install -m 0755 "$tmp/kustomize" /usr/local/bin/kustomize
  rm -rf "$tmp"
fi

kubectl apply -f "https://github.com/cert-manager/cert-manager/releases/download/${CERT_MANAGER_VERSION}/cert-manager.yaml"
kubectl -n cert-manager rollout status deploy/cert-manager-webhook --timeout=300s

echo "k3s ${K3S_VERSION} and cert-manager ${CERT_MANAGER_VERSION} ready"
