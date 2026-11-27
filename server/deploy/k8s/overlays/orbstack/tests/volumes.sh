#!/usr/bin/env bash
# orbstack.sh write_volumes fills every placeholder in volumes.example.yaml: tiles, the active
# build for the server, and the data directory for the weekly-refresh job.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
deploy="$(cd "$here/../../../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/server"
cp -R "$deploy" "$tmp/server/deploy"
mkdir -p "$tmp/server/data/build/20261126T132852Z" "$tmp/server/var/valhalla"
echo '{}' >"$tmp/server/data/build/20261126T132852Z/.via-static.json"
real="$(cd "$tmp/server" && pwd -P)"

fails=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1 (expected '$3', got '$2')"; fails=$((fails + 1)); fi
}

(
  # shellcheck disable=SC1091
  source "$tmp/server/deploy/orbstack.sh"
  write_volumes 20261126T132852Z
)
volumes="$tmp/server/deploy/k8s/overlays/orbstack/volumes.yaml"
check "no placeholder path is left" "$(grep -c '/absolute/path' "$volumes" || true)" "0"
check "server mounts the chosen build" "$(grep -c "path: $real/data/build/20261126T132852Z\$" "$volumes")" "1"
check "valhalla mounts the tiles" "$(grep -c "path: $real/var/valhalla\$" "$volumes")" "1"
check "the refresh job mounts the whole data directory" "$(grep -c "path: $real/data\$" "$volumes")" "1"
check "the refresh job tells the script the same node path" "$(grep -c "value: $real/data\$" "$volumes")" "1"
kubectl kustomize "$tmp/server/deploy/k8s/overlays/orbstack" >/dev/null
check "overlay renders with the generated volumes" ok ok

echo "failures: $fails"
exit "$fails"
