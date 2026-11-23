#!/usr/bin/env bash
set -euo pipefail

K3S_VERSION="v1.36.6+k3s1"
CERT_MANAGER_VERSION="v1.21.3"
CERT_MANAGER_SHA256="c04a51295adda2b1a775146751f973e787190778c7f35180861d9678876e5c74"
KUSTOMIZE_VERSION="v5.8.3"
KUSTOMIZE_SHA256="9867b76482cfc6d25b546abc1d2b5a32bd137505aba640649b4b378f64f6d68f"
DATA_DIR=/var/lib/via
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root" >&2
  exit 1
fi

if [ -f /etc/via-bootstrap.env ]; then
  # shellcheck source=/dev/null
  . /etc/via-bootstrap.env
fi

write_if_changed() {
  local path="$1" content="$2"
  mkdir -p "$(dirname "$path")"
  if [ -f "$path" ] && [ "$(cat "$path")" = "$content" ]; then
    return 1
  fi
  printf '%s\n' "$content" >"$path"
}

configure_log_retention() {
  if write_if_changed /etc/systemd/journald.conf.d/via.conf '[Journal]
MaxRetentionSec=6day
MaxFileSec=1day'; then
    systemctl restart systemd-journald
  fi

  rm -f /etc/tmpfiles.d/via-logs.conf

  local kubelet_dir="$DATA_DIR/k3s/agent/etc/kubelet.conf.d"
  if write_if_changed "$kubelet_dir/10-via-logs.conf" 'apiVersion: kubelet.config.k8s.io/v1beta1
kind: KubeletConfiguration
containerLogMaxSize: 5Mi
containerLogMaxFiles: 2' && command -v k3s >/dev/null 2>&1; then
    systemctl restart k3s
  fi

  write_if_changed /etc/systemd/system/via-log-cleanup.service '[Unit]
Description=Delete rotated pod logs older than 6 days

[Service]
Type=oneshot
ExecStart=/usr/bin/find /var/log/pods -type f -name "*.log.*" -mmin +8640 -delete
ExecStart=/usr/bin/find /var/log/containers -xtype l -delete' || true
  write_if_changed /etc/systemd/system/via-log-cleanup.timer '[Unit]
Description=Daily rotated pod log cleanup

[Timer]
OnCalendar=daily
Persistent=true

[Install]
WantedBy=timers.target' || true
  systemctl daemon-reload
  systemctl enable --now via-log-cleanup.timer
}

mount_data_volume() {
  if mountpoint -q "$DATA_DIR"; then
    return
  fi
  local device=""
  if [ -n "${VIA_DATA_VOLUME_ID:-}" ]; then
    device="/dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_${VIA_DATA_VOLUME_ID}"
    for _ in $(seq 1 60); do
      [ -e "$device" ] && break
      sleep 5
    done
    if [ ! -e "$device" ]; then
      echo "data volume $device did not appear" >&2
      exit 1
    fi
    local fstype rc=0
    fstype="$(blkid -o value -s TYPE "$device")" || rc=$?
    if [ "$rc" -eq 2 ]; then
      mkfs.ext4 -L via-data "$device"
    elif [ "$rc" -ne 0 ] || [ -z "$fstype" ]; then
      echo "cannot determine filesystem of $device (blkid rc=$rc); refusing to format" >&2
      exit 1
    fi
    grep -q 'LABEL=via-data' /etc/fstab || echo "LABEL=via-data $DATA_DIR ext4 defaults,nofail 0 2" >>/etc/fstab
    mkdir -p "$DATA_DIR"
    mount "$DATA_DIR"
  else
    mkdir -p "$DATA_DIR"
  fi
}

verify_sha256() {
  echo "$2  $1" | sha256sum -c -
}

mount_data_volume
configure_log_retention
mkdir -p "$DATA_DIR/static" "$DATA_DIR/valhalla" "$DATA_DIR/k3s"
# The nightly CronJob (uid 10001 in the server image) keeps its fetch snapshots in
# pipeline/ and hands finished builds to the host through static/incoming/.
install -d -o 10001 -g 10001 -m 0755 "$DATA_DIR/pipeline" "$DATA_DIR/static/incoming"

for pkg in zstd jq; do
  if ! command -v "$pkg" >/dev/null 2>&1; then
    apt-get install -y "$pkg"
  fi
done

if ! command -v aws >/dev/null 2>&1; then
  snap install aws-cli --classic
fi

if ! command -v k3s >/dev/null 2>&1; then
  curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION="$K3S_VERSION" sh -s - --data-dir "$DATA_DIR/k3s"
fi

deadline=$((SECONDS + 300))
until kubectl get nodes 2>/dev/null | grep -q " Ready"; do
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "node not Ready after 300s" >&2
    exit 1
  fi
  sleep 5
done

if ! command -v kustomize >/dev/null 2>&1; then
  tmp="$(mktemp -d)"
  curl -sfL -o "$tmp/kustomize.tgz" \
    "https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize%2F${KUSTOMIZE_VERSION}/kustomize_${KUSTOMIZE_VERSION}_linux_arm64.tar.gz"
  verify_sha256 "$tmp/kustomize.tgz" "$KUSTOMIZE_SHA256"
  tar -xz -C "$tmp" -f "$tmp/kustomize.tgz"
  install -m 0755 "$tmp/kustomize" /usr/local/bin/kustomize
  rm -rf "$tmp"
fi

tmp="$(mktemp -d)"
curl -sfL -o "$tmp/cert-manager.yaml" \
  "https://github.com/cert-manager/cert-manager/releases/download/${CERT_MANAGER_VERSION}/cert-manager.yaml"
verify_sha256 "$tmp/cert-manager.yaml" "$CERT_MANAGER_SHA256"
kubectl apply -f "$tmp/cert-manager.yaml"
rm -rf "$tmp"
kubectl -n cert-manager rollout status deploy/cert-manager-webhook --timeout=300s

if command -v systemctl >/dev/null 2>&1 && [ -d /opt/via/server/deploy/aws/artifacts ]; then
  install -m 0644 /opt/via/server/deploy/aws/artifacts/via-artifact-fetch.service /etc/systemd/system/
  install -m 0644 /opt/via/server/deploy/aws/artifacts/via-artifact-fetch.timer /etc/systemd/system/
  install -m 0755 /opt/via/server/deploy/aws/bootstrap/via-fetch-metric.sh /usr/local/sbin/via-fetch-metric
  mkdir -p "$DATA_DIR/textfile"
  write_if_changed /etc/systemd/system/via-artifact-fetch.service.d/metrics.conf '[Service]
ExecStopPost=/usr/local/sbin/via-fetch-metric' || true
  install -m 0755 /opt/via/server/deploy/aws/bootstrap/via-disk-metric.sh /usr/local/sbin/via-disk-metric
  write_if_changed /etc/systemd/system/via-disk-metric.service '[Unit]
Description=Write disk usage as a node textfile metric

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/via-disk-metric' || true
  write_if_changed /etc/systemd/system/via-disk-metric.timer '[Unit]
Description=Update disk usage metric every minute

[Timer]
OnCalendar=minutely

[Install]
WantedBy=timers.target' || true
  systemctl daemon-reload
  systemctl enable --now via-artifact-fetch.timer via-disk-metric.timer
  install -m 0644 /opt/via/server/deploy/aws/artifacts/via-static-activate.service /etc/systemd/system/
  install -m 0644 /opt/via/server/deploy/aws/artifacts/via-static-activate.path /etc/systemd/system/
  systemctl daemon-reload
  systemctl enable --now via-static-activate.path
  systemctl start --no-block via-disk-metric.service via-artifact-fetch.service
fi

echo "k3s ${K3S_VERSION} and cert-manager ${CERT_MANAGER_VERSION} ready"
