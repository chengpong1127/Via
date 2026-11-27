#!/usr/bin/env bash
# Renders the orbstack overlay (with volumes.example.yaml standing in for the git-ignored,
# generated volumes.yaml) and compares the weekly-refresh objects with weekly-refresh.golden.yaml.
# Refresh the golden after an intended change: tests/render.sh --update
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
overlay="$(dirname "$here")"
deploy="$(cd "$overlay/../../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/server"
cp -R "$deploy" "$tmp/server/deploy"
rendered_overlay="$tmp/server/deploy/k8s/overlays/orbstack"
cp "$rendered_overlay/volumes.example.yaml" "$rendered_overlay/volumes.yaml"
kubectl kustomize "$rendered_overlay" >"$tmp/rendered.yaml"

doc() { awk -v RS='---\n' -v kind="$1" '$0 ~ "(^|\n)kind: " kind "\n"' "$tmp/rendered.yaml"; }
{ doc CronJob; echo '---'; doc ServiceAccount; echo '---'; doc Role; echo '---'; doc RoleBinding; } >"$tmp/weekly-refresh.yaml"

if [ "${1:-}" = "--update" ]; then
  cp "$tmp/weekly-refresh.yaml" "$here/weekly-refresh.golden.yaml"
  echo "updated weekly-refresh.golden.yaml"
  exit 0
fi

fails=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1 (expected '$3', got '$2')"; fails=$((fails + 1)); fi
}

if diff -u "$here/weekly-refresh.golden.yaml" "$tmp/weekly-refresh.yaml"; then
  echo "ok   weekly-refresh objects match the golden file"
else
  echo "FAIL weekly-refresh objects differ from weekly-refresh.golden.yaml (intended? run tests/render.sh --update)"
  fails=$((fails + 1))
fi

check "schedule is Sunday 23:00" "$(sed -n 's/^  schedule: //p' "$tmp/weekly-refresh.yaml")" "0 23 * * 0"
check "timezone is stated explicitly" "$(sed -n 's/^  timeZone: //p' "$tmp/weekly-refresh.yaml")" "Asia/Taipei"
check "a second run never overlaps the first" "$(sed -n 's/^  concurrencyPolicy: //p' "$tmp/weekly-refresh.yaml")" "Forbid"
check "the job does not retry a failed run on its own" "$(sed -n 's/^      backoffLimit: //p' "$tmp/weekly-refresh.yaml")" "0"
check "TDX key comes from the tdx-credentials Secret" "$(grep -c 'name: tdx-credentials' "$tmp/weekly-refresh.yaml")" "2"
check "valhalla is reached through the in-cluster service" "$(grep -c 'value: http://valhalla:8002/' "$tmp/weekly-refresh.yaml")" "1"

script_in_configmap="$(awk -v RS='---\n' '$0 ~ /(^|\n)kind: ConfigMap\n/ && /name: refresh-scripts/' "$tmp/rendered.yaml" \
  | sed -n '/^  refresh-static.sh: |/,$p' | sed '1d' | sed -E 's/^    //' | sed '/^kind: ConfigMap$/,$d')"
check "ConfigMap carries refresh-static.sh unchanged" "$(printf '%s\n' "$script_in_configmap" | sed '/^[[:space:]]*$/d' | cksum)" \
  "$(sed '/^[[:space:]]*$/d' "$overlay/refresh-static.sh" | cksum)"

# The server Deployment, Service and Valhalla keep their one-replica OrbStack shape.
check "one server replica" "$(doc Deployment | awk '/^  name: server$/{s=1} s && /^  replicas:/{print $2; exit}')" "1"

echo "failures: $fails"
exit "$fails"
