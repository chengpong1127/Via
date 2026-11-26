#!/usr/bin/env bash
# Local Via stack on OrbStack Kubernetes. See docs/local-orbstack.md.
#   deploy/orbstack.sh up [build-id]    build the image from this checkout, apply, wait
#   deploy/orbstack.sh data <build-id>  point the server at another static build, restart
#   deploy/orbstack.sh status           pods, service, readiness
#   deploy/orbstack.sh lan              expose the server on the Wi-Fi, port 3001, NO AUTH (foreground)
#   deploy/orbstack.sh down             delete deployments and services (keeps data, tiles and the Secret)
# The tdx-credentials Secret must already exist in namespace via (docs/local-orbstack.md).
set -euo pipefail

CTX=orbstack
NS=via
SERVER_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OVERLAY="$SERVER_DIR/deploy/k8s/overlays/orbstack"
DATA_DIR="${VIA_DATA_DIR:-$SERVER_DIR/data}"
TILES_DIR="${VIA_TILES_DIR:-$SERVER_DIR/var/valhalla}"
kc() { kubectl --context "$CTX" -n "$NS" "$@"; }

latest_build() {
  local target
  target="$(readlink "$DATA_DIR/build/.via-static-latest.json")" || { echo "no $DATA_DIR/build/.via-static-latest.json" >&2; exit 1; }
  dirname "$target"
}

write_volumes() {
  local build_id="$1" real_data real_tiles
  [ -f "$DATA_DIR/build/$build_id/.via-static.json" ] || { echo "no static build $build_id under $DATA_DIR/build" >&2; exit 1; }
  [ -d "$TILES_DIR" ] || { echo "no Valhalla tiles at $TILES_DIR (set VIA_TILES_DIR)" >&2; exit 1; }
  real_data="$(cd "$DATA_DIR" && pwd -P)"
  real_tiles="$(cd "$TILES_DIR" && pwd -P)"
  esc() { printf '%s' "$1" | sed -e 's/[\\&#]/\\&/g'; }
  sed -e "s#/absolute/path/to/Via/server/var/valhalla#$(esc "$real_tiles")#" \
      -e "s#/absolute/path/to/Via/server/data/build/<build-id>#$(esc "$real_data/build/$build_id")#" \
      "$OVERLAY/volumes.example.yaml" > "$OVERLAY/volumes.yaml"
}

apply() {
  kubectl --context "$CTX" apply -k "$OVERLAY"
  kc rollout status deploy/valhalla deploy/tdx-gateway deploy/server --timeout=600s
}

case "${1:-}" in
  up)
    write_volumes "${2:-$(latest_build)}"
    docker build -t via-server:dev "$SERVER_DIR"
    kubectl --context "$CTX" apply -f "$SERVER_DIR/deploy/k8s/base/namespace.yaml"
    kc get secret tdx-credentials >/dev/null  # fail early if missing
    existed=0; kc get deploy server >/dev/null 2>&1 && existed=1
    apply
    if [ "$existed" = 1 ]; then  # same tag: restart to pick up a rebuilt via-server:dev
      kc rollout restart deploy/server deploy/tdx-gateway
      kc rollout status deploy/server deploy/tdx-gateway --timeout=600s
    fi
    ;;
  data)
    write_volumes "${2:?usage: orbstack.sh data <build-id>}"
    apply
    kc rollout restart deploy/server
    kc rollout status deploy/server --timeout=600s
    ;;
  status)
    kc get pods,svc
    curl -fsS -m 5 http://127.0.0.1:3000/health/ready && echo " ready"
    ;;
  lan)
    iface="${VIA_LAN_IFACE:-en0}"
    ip="$(ipconfig getifaddr "$iface" || true)"
    [ -n "$ip" ] || { echo "no IP on $iface; set VIA_LAN_IFACE (e.g. en1)" >&2; exit 1; }
    echo "WARNING: http://$ip:3001 has NO authentication; every device on this network can use it and spend the TDX budget. Trusted networks only. Ctrl-C to stop." >&2
    exec kubectl --context "$CTX" -n "$NS" port-forward svc/server 3001:3000 --address "$ip"
    ;;
  down)
    kc delete deploy,svc --all  # not the namespace: that would delete the Secret
    ;;
  *) sed -n 2,8p "$0"; exit 2 ;;
esac
