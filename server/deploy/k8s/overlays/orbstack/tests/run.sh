#!/usr/bin/env bash
# All orbstack overlay checks. Needs bash, jq and kubectl (for `kubectl kustomize`); no cluster.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
status=0
for t in refresh.sh render.sh volumes.sh; do
  echo "== $t"
  "$here/$t" || status=1
done
exit "$status"
