#!/usr/bin/env bash
# Renders the aws-mvp overlay and asserts the MVP sizing. No cluster needed.
set -euo pipefail
cd "$(dirname "$0")"
out=$(kustomize build .)
fail() { echo "FAIL: $*" >&2; exit 1; }

replicas() { # deployment name -> replicas
  awk -v n="$1" '/^kind: Deployment/{d=1} d&&$1=="name:"&&!s{s=($2==n)} s&&$1=="replicas:"{print $2; exit} /^---/{d=0;s=0}' <<<"$out"
}
[ "$(replicas server)" = "1" ] || fail "server replicas must be 1"
[ "$(replicas valhalla)" = "1" ] || fail "valhalla replicas must be 1"
[ "$(replicas tdx-gateway)" = "1" ] || fail "tdx-gateway replicas must be 1"
grep -q 'memory: MEM_' <<<"$out" && fail "unresolved MEM_ placeholder"
grep -q 'kind: CronJob' <<<"$out" || fail "nightly CronJob missing"
echo "aws-mvp overlay OK"
